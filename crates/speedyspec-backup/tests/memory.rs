//! A backup streams: a 20 MB file and a 50 000-row dump go through without being held in memory. Measured with a
//! counting allocator, so this test has its own binary (`harness = false`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

use speedyspec_backup::flow::{SiteDatabase, backup};
use speedyspec_backup::manifest::Manifest;
use speedyspec_backup::rewrite::Move;
use speedyspec_driver::{BackupSpec, SiteUrl};

struct Counting;

static NOW: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = NOW.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
        PEAK.fetch_max(now, Ordering::SeqCst);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        NOW.fetch_sub(layout.size(), Ordering::SeqCst);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A database whose dump is 50 000 rows, written as it goes.
struct Big;

impl SiteDatabase for Big {
    fn export(&mut self, out: &mut dyn Write) -> Result<(), String> {
        for i in 0..50_000 {
            writeln!(out, "INSERT INTO `wp_rows` (`id`,`text`) VALUES\n({i},'row {i} with some text to make it longer');")
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn import(
        &mut self,
        _: &mut dyn BufRead,
        _: &Move,
        _: &mut Vec<String>,
    ) -> Result<u64, String> {
        unreachable!()
    }
    fn has_prefixed_tables(&mut self) -> Result<bool, String> {
        Ok(true)
    }
    fn site_url(&mut self, _: &SiteUrl) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn prefix(&self) -> String {
        "wp_".into()
    }
}

fn main() {
    let root = tempfile::tempdir().unwrap();
    let mut big = std::fs::File::create(root.path().join("big.bin")).unwrap();
    let chunk = vec![7u8; 1024 * 1024];
    for _ in 0..20 {
        big.write_all(&chunk).unwrap();
    }
    drop(chunk);
    let out = tempfile::tempdir().unwrap();
    let spec = BackupSpec {
        include: vec![".".into()],
        exclude: vec![],
        preserve: vec![],
        site_url: None,
        prefixed_columns: vec![],
        app_version: None,
    };
    let manifest = Manifest {
        app: "site".into(),
        parts: vec![],
        cli_version: "1".into(),
        app_version: None,
        extensions: vec![],
        engine: "mysql".into(),
        table_prefix: String::new(),
        site_url: None,
        source_install: "big".into(),
        source_driver: "zip".into(),
        created_at: "2026-11-19T10:00:00Z".into(),
    };
    let before = NOW.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    backup(
        root.path(),
        &spec,
        &mut Big,
        manifest,
        &out.path().join("big.tar.gz"),
    )
    .unwrap();
    let peak = PEAK.load(Ordering::SeqCst) - before;
    let limit = 64 * 1024 * 1024;
    if peak < limit {
        println!("ok: a_backup_streams (peak {} KiB)", peak / 1024);
    } else {
        eprintln!(
            "FAIL: a_backup_streams: peak {} KiB, limit {} KiB",
            peak / 1024,
            limit / 1024
        );
        std::process::exit(1);
    }
}
