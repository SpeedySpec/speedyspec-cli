//! `fetch_and_extract`: one archive into one directory, and nothing anywhere else.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use speedyspec_driver_zip::{
    Archive, ExtractError, FetchError, HttpFetch, Limits, fetch_and_extract, fetch_and_extract_with,
};
use speedyspec_testkit::{Entry, Route, Server, build_zip};
use tempfile::TempDir;

/// A work area: `root/into` is the empty target; `root` must hold nothing else afterwards.
struct Area {
    root: TempDir,
}

impl Area {
    fn new() -> Area {
        let root = TempDir::new().unwrap();
        fs::create_dir(root.path().join("into")).unwrap();
        Area { root }
    }

    fn target(&self) -> PathBuf {
        self.root.path().join("into")
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.target().join(relative)).unwrap()
    }

    fn assert_untouched(&self) {
        assert_eq!(
            names(&self.target()),
            Vec::<String>::new(),
            "into must be left empty"
        );
        assert_eq!(
            names(self.root.path()),
            vec!["into"],
            "nothing may be left beside into"
        );
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn serve(entries: &[(&str, Entry)]) -> (Server, String) {
    let server = Server::start(vec![("/a.zip", Route::ok(build_zip(entries)))]);
    let url = format!("{}/a.zip", server.url());
    (server, url)
}

fn extract(area: &Area, url: &str, sha256: Option<&str>, strip: u32) -> Result<u64, ExtractError> {
    let archive = Archive {
        url,
        sha256,
        strip_components: strip,
    };
    fetch_and_extract(&archive, &area.target(), &HttpFetch::new())
}

fn unsafe_entry(result: Result<u64, ExtractError>) -> String {
    match result {
        Err(ExtractError::Unsafe { entry, .. }) => entry,
        other => panic!("expected Unsafe, got {other:?}"),
    }
}

#[test]
fn extracts_files_and_directories() {
    let (_server, url) = serve(&[
        ("a.txt", Entry::file("A")),
        ("d/", Entry::Dir),
        ("d/b.txt", Entry::file("B")),
    ]);
    let area = Area::new();
    assert_eq!(extract(&area, &url, None, 0).unwrap(), 2);
    assert_eq!(area.read("a.txt"), "A");
    assert_eq!(area.read("d/b.txt"), "B");
    assert_eq!(names(area.root.path()), vec!["into"]);
}

#[test]
fn strips_leading_components() {
    let (_server, url) = serve(&[
        ("top.txt", Entry::file("skipped")),
        ("wordpress/", Entry::Dir),
        ("wordpress/index.php", Entry::file("<?php")),
        ("wordpress/wp-includes/version.php", Entry::file("v")),
    ]);
    let area = Area::new();
    assert_eq!(extract(&area, &url, None, 1).unwrap(), 2);
    assert_eq!(area.read("index.php"), "<?php");
    assert_eq!(area.read("wp-includes/version.php"), "v");
    assert_eq!(names(&area.target()), vec!["index.php", "wp-includes"]);
}

#[cfg(unix)]
#[test]
fn keeps_mode_bits_masked() {
    use std::os::unix::fs::PermissionsExt;
    let (_server, url) = serve(&[("run.sh", Entry::File(b"#!/bin/sh".to_vec(), 0o4755))]);
    let area = Area::new();
    extract(&area, &url, None, 0).unwrap();
    let mode = fs::metadata(area.target().join("run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o755);
}

#[test]
fn verifies_sha256() {
    let bytes = build_zip(&[("a.txt", Entry::file("A"))]);
    let sum: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let server = Server::start(vec![("/a.zip", Route::ok(bytes))]);
    let url = format!("{}/a.zip", server.url());

    let area = Area::new();
    assert_eq!(extract(&area, &url, Some(&sum), 0).unwrap(), 1);

    let area = Area::new();
    let wrong = "0".repeat(64);
    match extract(&area, &url, Some(&wrong), 0) {
        Err(ExtractError::Checksum {
            expected, actual, ..
        }) => {
            assert_eq!(expected, wrong);
            assert_eq!(actual, sum);
        }
        other => panic!("{other:?}"),
    }
    area.assert_untouched();
}

#[test]
fn rejects_non_zip() {
    let server = Server::start(vec![("/a.zip", Route::ok("not a zip"))]);
    let area = Area::new();
    let result = extract(&area, &format!("{}/a.zip", server.url()), None, 0);
    assert!(
        matches!(result, Err(ExtractError::NotAZip { .. })),
        "{result:?}"
    );
    area.assert_untouched();
}

#[test]
fn rejects_empty_result() {
    let (_server, url) = serve(&[("d/", Entry::Dir)]);
    let area = Area::new();
    let result = extract(&area, &url, None, 0);
    assert!(
        matches!(result, Err(ExtractError::Empty { .. })),
        "{result:?}"
    );
    area.assert_untouched();

    let (_server, url) = serve(&[("a.txt", Entry::file("A"))]);
    let area = Area::new();
    let result = extract(&area, &url, None, 1);
    assert!(
        matches!(result, Err(ExtractError::Empty { .. })),
        "{result:?}"
    );
    area.assert_untouched();
}

#[test]
fn rejects_parent_traversal() {
    for name in ["../x", "a/../../x"] {
        let (_server, url) = serve(&[("ok.txt", Entry::file("fine")), (name, Entry::file("evil"))]);
        let area = Area::new();
        assert_eq!(unsafe_entry(extract(&area, &url, None, 0)), name);
        area.assert_untouched();
        assert!(!area.root.path().join("x").exists());
    }
}

#[test]
fn rejects_absolute_and_odd_names() {
    for name in ["/etc/x", "a\\b", "C:x", "a\0b", "a//b"] {
        let (_server, url) = serve(&[(name, Entry::file("evil"))]);
        let area = Area::new();
        let result = extract(&area, &url, None, 0);
        assert!(
            matches!(result, Err(ExtractError::Unsafe { .. })),
            "{name:?}: {result:?}"
        );
        area.assert_untouched();
    }
}

#[cfg(unix)]
#[test]
fn symlink_inside_is_created() {
    let (_server, url) = serve(&[
        ("a.txt", Entry::file("A")),
        ("link", Entry::Symlink("./a.txt".to_owned())),
    ]);
    let area = Area::new();
    extract(&area, &url, None, 0).unwrap();
    assert!(
        fs::symlink_metadata(area.target().join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(area.read("link"), "A");
}

#[cfg(unix)]
#[test]
fn symlink_outside_is_rejected() {
    for target in ["../../outside", "/etc", "d/../../x"] {
        let (_server, url) = serve(&[
            ("a.txt", Entry::file("A")),
            ("link", Entry::Symlink(target.to_owned())),
        ]);
        let area = Area::new();
        assert_eq!(
            unsafe_entry(extract(&area, &url, None, 0)),
            "link",
            "{target}"
        );
        area.assert_untouched();
    }
}

#[cfg(unix)]
#[test]
fn symlink_chain_that_climbs_out_is_rejected() {
    // Each link's target reads as inside, but on disk `up` is the parent of the target
    // directory and `up2` its grandparent.
    let (_server, url) = serve(&[
        ("ok.txt", Entry::file("fine")),
        ("a", Entry::Symlink(".".to_owned())),
        ("up", Entry::Symlink("a/..".to_owned())),
        ("up2", Entry::Symlink("up/..".to_owned())),
    ]);
    let area = Area::new();
    let entry = unsafe_entry(extract(&area, &url, None, 0));
    assert!(entry == "up" || entry == "up2", "{entry}");
    area.assert_untouched();
}

#[cfg(unix)]
#[test]
fn dangling_symlink_is_rejected() {
    let (_server, url) = serve(&[
        ("a.txt", Entry::file("A")),
        ("link", Entry::Symlink("./missing".to_owned())),
    ]);
    let area = Area::new();
    assert_eq!(unsafe_entry(extract(&area, &url, None, 0)), "link");
    area.assert_untouched();
}

#[cfg(unix)]
#[test]
fn symlink_with_a_parent_segment_that_stays_inside_is_allowed() {
    let (_server, url) = serve(&[
        ("pkg/bin/tool", Entry::file("#!/bin/sh")),
        ("bin/tool", Entry::Symlink("../pkg/bin/tool".to_owned())),
    ]);
    let area = Area::new();
    extract(&area, &url, None, 0).unwrap();
    assert_eq!(area.read("bin/tool"), "#!/bin/sh");
}

#[cfg(unix)]
#[test]
fn overlong_symlink_target_is_rejected() {
    let (_server, url) = serve(&[
        ("a.txt", Entry::file("A")),
        ("link", Entry::Symlink("x/".repeat(3000))),
    ]);
    let area = Area::new();
    assert_eq!(unsafe_entry(extract(&area, &url, None, 0)), "link");
    area.assert_untouched();
}

#[test]
fn download_larger_than_the_limit_is_refused() {
    let (_server, url) = serve(&[("a.txt", Entry::File(vec![b'x'; 5000], 0o644))]);
    let area = Area::new();
    let archive = Archive {
        url: &url,
        sha256: None,
        strip_components: 0,
    };
    let limits = Limits {
        download_bytes: 1000,
        ..Limits::default()
    };
    let result = fetch_and_extract_with(&archive, &area.target(), &HttpFetch::new(), &limits);
    let message = result.unwrap_err().to_string();
    assert!(message.contains("1000 bytes"), "{message}");
    area.assert_untouched();
}

#[test]
fn unpacking_more_than_the_limit_is_refused() {
    let (_server, url) = serve(&[
        ("a.txt", Entry::File(vec![b'x'; 600], 0o644)),
        ("b.txt", Entry::File(vec![b'y'; 600], 0o644)),
    ]);
    let area = Area::new();
    let archive = Archive {
        url: &url,
        sha256: None,
        strip_components: 0,
    };
    let limits = Limits {
        unpacked_bytes: 1000,
        ..Limits::default()
    };
    let result = fetch_and_extract_with(&archive, &area.target(), &HttpFetch::new(), &limits);
    let message = result.unwrap_err().to_string();
    assert!(message.contains("1000 bytes"), "{message}");
    area.assert_untouched();
}

#[cfg(unix)]
#[test]
fn writes_through_an_inside_symlink() {
    let (_server, url) = serve(&[
        ("real/", Entry::Dir),
        ("d", Entry::Symlink("./real".to_owned())),
        ("d/x", Entry::file("X")),
    ]);
    let area = Area::new();
    extract(&area, &url, None, 0).unwrap();
    assert_eq!(area.read("real/x"), "X");
}

#[cfg(unix)]
#[test]
fn never_writes_through_an_outside_symlink() {
    let (_server, url) = serve(&[("d/x", Entry::file("evil"))]);
    let area = Area::new();
    let outside = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), area.target().join("d")).unwrap();
    assert_eq!(unsafe_entry(extract(&area, &url, None, 0)), "d/x");
    assert_eq!(
        names(outside.path()),
        Vec::<String>::new(),
        "the outside directory is unchanged"
    );
}

#[test]
fn later_duplicate_replaces_earlier() {
    let (_server, url) = serve(&[("a.txt", Entry::file("1")), ("a.txt", Entry::file("2"))]);
    let area = Area::new();
    extract(&area, &url, None, 0).unwrap();
    assert_eq!(area.read("a.txt"), "2");
}

#[test]
fn file_versus_directory_conflict() {
    let (_server, url) = serve(&[("a", Entry::file("file")), ("a/b", Entry::file("B"))]);
    let area = Area::new();
    assert_eq!(unsafe_entry(extract(&area, &url, None, 0)), "a/b");
    area.assert_untouched();
}

#[test]
fn failed_download_leaves_nothing() {
    let server = Server::start(vec![("/a.zip", Route::status(500))]);
    let area = Area::new();
    let result = extract(&area, &format!("{}/a.zip", server.url()), None, 0);
    assert!(
        matches!(
            result,
            Err(ExtractError::Fetch(FetchError::Status { status: 500, .. }))
        ),
        "{result:?}"
    );
    area.assert_untouched();
}

#[test]
fn truncated_download_leaves_nothing() {
    let bytes = build_zip(&[("a.txt", Entry::file("A"))]);
    let server = Server::start(vec![("/a.zip", Route::truncated(bytes, 10))]);
    let area = Area::new();
    let result = extract(&area, &format!("{}/a.zip", server.url()), None, 0);
    assert!(
        matches!(result, Err(ExtractError::Fetch(FetchError::Io { .. }))),
        "{result:?}"
    );
    area.assert_untouched();
}
