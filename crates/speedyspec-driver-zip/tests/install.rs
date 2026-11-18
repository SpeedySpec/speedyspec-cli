//! Installing several sources under one base directory, and the zip driver built on it.

use std::any::Any;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use speedyspec_driver::{Driver, DriverInput, ObjectReader, Progress, ZipSource};
use speedyspec_driver_zip::{
    ExtractError, Fetch, FetchError, InstallError, ZipDriver, install_sources,
};
use speedyspec_hooks::{Key, Value};
use speedyspec_testkit::{Entry, build_zip};
use tempfile::TempDir;

/// Serves archives from memory; a URL it does not know is an HTTP 500.
#[derive(Default)]
struct FakeFetch {
    archives: HashMap<String, Vec<u8>>,
}

impl FakeFetch {
    fn with(mut self, url: &str, entries: &[(&str, Entry)]) -> Self {
        self.archives.insert(url.to_owned(), build_zip(entries));
        self
    }
}

impl Fetch for FakeFetch {
    fn fetch(&self, url: &str, to: &mut dyn Write) -> Result<u64, FetchError> {
        let Some(bytes) = self.archives.get(url) else {
            return Err(FetchError::Status {
                url: url.to_owned(),
                status: 500,
            });
        };
        to.write_all(bytes).unwrap();
        Ok(bytes.len() as u64)
    }
}

#[derive(Default)]
struct Quiet(Vec<String>);

impl Progress for Quiet {
    fn step(&mut self, message: &str) {
        self.0.push(message.to_owned());
    }
}

fn source(url: &str, destination: &str) -> ZipSource {
    ZipSource {
        url: url.to_owned(),
        destination: destination.to_owned(),
        sha256: None,
        strip_components: 0,
    }
}

fn install(
    sources: &[ZipSource],
    base: &Path,
    force: bool,
    fetch: &FakeFetch,
) -> Result<Vec<speedyspec_driver_zip::Placement>, InstallError> {
    install_sources(sources, base, force, fetch, &mut Quiet::default())
}

/// Every file under `dir`, as sorted relative paths with forward slashes.
fn files(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let found = fs::symlink_metadata(&path).unwrap();
            if found.is_dir() {
                walk(&path, root, out);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn no_staging_left(base: &Path) {
    for entry in fs::read_dir(base).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            !name.starts_with(".speedyspec-staging"),
            "left behind: {name}"
        );
    }
}

const WEB: &str = "https://x.test/web.zip";
const DOCS: &str = "https://x.test/docs.zip";
const API: &str = "https://x.test/api.zip";

fn three() -> FakeFetch {
    FakeFetch::default()
        .with(
            WEB,
            &[
                ("index.html", Entry::file("web")),
                ("js/app.js", Entry::file("js")),
            ],
        )
        .with(DOCS, &[("readme.md", Entry::file("docs"))])
        .with(API, &[("api.php", Entry::file("api"))])
}

#[test]
fn installs_one_source_into_an_empty_base() {
    let base = TempDir::new().unwrap();
    let placed = install(&[source(WEB, ".")], base.path(), false, &three()).unwrap();
    assert_eq!(files(base.path()), vec!["index.html", "js/app.js"]);
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0].url, WEB);
    assert_eq!(placed[0].destination, ".");
    assert!(placed[0].path.is_absolute());
    assert_eq!(placed[0].path, base.path().canonicalize().unwrap());
    no_staging_left(base.path());
}

#[test]
fn creates_a_missing_base() {
    let root = TempDir::new().unwrap();
    let base = root.path().join("new/site");
    install(&[source(WEB, ".")], &base, false, &three()).unwrap();
    assert_eq!(files(&base), vec!["index.html", "js/app.js"]);
}

#[test]
fn installs_three_parts_to_three_destinations() {
    let base = TempDir::new().unwrap();
    let sources = [
        source(WEB, "admin"),
        source(DOCS, "blog"),
        source(API, "api"),
    ];
    let placed = install(&sources, base.path(), false, &three()).unwrap();
    assert_eq!(
        files(base.path()),
        vec![
            "admin/index.html",
            "admin/js/app.js",
            "api/api.php",
            "blog/readme.md"
        ]
    );
    let destinations: Vec<&str> = placed.iter().map(|p| p.destination.as_str()).collect();
    assert_eq!(destinations, vec!["admin", "blog", "api"]);
    assert!(placed[0].path.ends_with("admin"));
}

#[test]
fn parent_then_child_destination_works() {
    let base = TempDir::new().unwrap();
    let sources = [source(WEB, "."), source(DOCS, "js/vendor/x")];
    install(&sources, base.path(), false, &three()).unwrap();
    assert_eq!(
        files(base.path()),
        vec!["index.html", "js/app.js", "js/vendor/x/readme.md"]
    );
}

#[test]
fn child_then_parent_destination_works() {
    let base = TempDir::new().unwrap();
    let sources = [source(DOCS, "js/vendor/x"), source(WEB, ".")];
    install(&sources, base.path(), false, &three()).unwrap();
    assert_eq!(
        files(base.path()),
        vec!["index.html", "js/app.js", "js/vendor/x/readme.md"]
    );
}

#[test]
fn refuses_a_non_empty_destination() {
    let base = TempDir::new().unwrap();
    fs::write(base.path().join("keep.txt"), "mine").unwrap();
    match install(&[source(WEB, ".")], base.path(), false, &three()) {
        Err(InstallError::Occupied { path }) => {
            assert_eq!(path, base.path().canonicalize().unwrap());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(files(base.path()), vec!["keep.txt"]);
    assert_eq!(
        fs::read_to_string(base.path().join("keep.txt")).unwrap(),
        "mine"
    );
    no_staging_left(base.path());
}

#[test]
fn empty_existing_directories_are_not_occupied() {
    let base = TempDir::new().unwrap();
    fs::create_dir_all(base.path().join("admin/cache")).unwrap();
    install(&[source(WEB, "admin")], base.path(), false, &three()).unwrap();
    assert_eq!(
        files(base.path()),
        vec!["admin/index.html", "admin/js/app.js"]
    );
}

#[test]
fn force_merges_and_keeps_other_files() {
    let base = TempDir::new().unwrap();
    fs::write(base.path().join("keep.txt"), "mine").unwrap();
    fs::write(base.path().join("index.html"), "old").unwrap();
    install(&[source(WEB, ".")], base.path(), true, &three()).unwrap();
    assert_eq!(
        files(base.path()),
        vec!["index.html", "js/app.js", "keep.txt"]
    );
    assert_eq!(
        fs::read_to_string(base.path().join("index.html")).unwrap(),
        "web"
    );
    assert_eq!(
        fs::read_to_string(base.path().join("keep.txt")).unwrap(),
        "mine"
    );
    no_staging_left(base.path());
}

#[test]
fn force_never_deletes() {
    let base = TempDir::new().unwrap();
    fs::create_dir_all(base.path().join("js/old")).unwrap();
    fs::write(base.path().join("js/old/legacy.js"), "old").unwrap();
    install(&[source(WEB, ".")], base.path(), true, &three()).unwrap();
    assert_eq!(
        files(base.path()),
        vec!["index.html", "js/app.js", "js/old/legacy.js"]
    );
}

#[test]
fn failed_download_changes_nothing() {
    let base = TempDir::new().unwrap();
    let sources = [
        source(WEB, "admin"),
        source("https://x.test/missing.zip", "blog"),
        source(API, "api"),
    ];
    match install(&sources, base.path(), false, &three()) {
        Err(InstallError::Extract(ExtractError::Fetch(FetchError::Status { status, .. }))) => {
            assert_eq!(status, 500);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        fs::read_dir(base.path()).unwrap().count(),
        0,
        "base must be untouched"
    );
}

#[test]
fn failed_checksum_changes_nothing() {
    let base = TempDir::new().unwrap();
    let mut bad = source(DOCS, "blog");
    bad.sha256 = Some("0".repeat(64));
    let result = install(&[source(WEB, "admin"), bad], base.path(), false, &three());
    assert!(
        matches!(
            result,
            Err(InstallError::Extract(ExtractError::Checksum { .. }))
        ),
        "{result:?}"
    );
    assert_eq!(
        fs::read_dir(base.path()).unwrap().count(),
        0,
        "base must be untouched"
    );
}

#[cfg(unix)]
#[test]
fn destination_through_outside_symlink_is_refused() {
    let base = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), base.path().join("out")).unwrap();
    for force in [false, true] {
        match install(&[source(WEB, "out/x")], base.path(), force, &three()) {
            Err(InstallError::OutsideBase { destination }) => assert_eq!(destination, "out/x"),
            other => panic!("{other:?}"),
        }
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
        no_staging_left(base.path());
    }
}

#[cfg(unix)]
#[test]
fn force_does_not_write_through_outside_symlink() {
    let base = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    fs::create_dir(base.path().join("site")).unwrap();
    std::os::unix::fs::symlink(outside.path(), base.path().join("site/js")).unwrap();
    let result = install(&[source(WEB, "site")], base.path(), true, &three());
    assert!(result.is_err(), "{result:?}");
    assert_eq!(
        fs::read_dir(outside.path()).unwrap().count(),
        0,
        "outside must be unchanged"
    );
    no_staging_left(base.path());
}

#[test]
fn later_source_replaces_an_earlier_one_at_the_same_path() {
    let fetch = FakeFetch::default()
        .with("https://x.test/1.zip", &[("a.txt", Entry::file("one"))])
        .with("https://x.test/2.zip", &[("a.txt", Entry::file("two"))]);
    let base = TempDir::new().unwrap();
    let sources = [
        source("https://x.test/1.zip", "."),
        source("https://x.test/2.zip", "./"),
    ];
    install(&sources, base.path(), false, &fetch).unwrap();
    assert_eq!(
        fs::read_to_string(base.path().join("a.txt")).unwrap(),
        "two"
    );
}

#[test]
fn reports_progress_for_each_source() {
    let base = TempDir::new().unwrap();
    let mut progress = Quiet::default();
    let sources = [source(WEB, "admin"), source(DOCS, "blog")];
    install_sources(&sources, base.path(), false, &three(), &mut progress).unwrap();
    let text = progress.0.join("\n");
    assert!(text.contains(WEB) && text.contains(DOCS), "{text}");
}

// ---- the driver ---------------------------------------------------------------------------

const ZIP_CLASS: &str = "SpeedySpec\\Cli\\Contracts\\ZipSource";

#[derive(Default)]
struct FakeReader {
    objects: RefCell<HashMap<usize, Vec<(String, Value)>>>,
}

impl FakeReader {
    fn zip_source(&self, url: &str, destination: &str) -> Value {
        let rc: Rc<dyn Any> = Rc::new(0u8);
        let properties = vec![
            ("url".to_owned(), Value::Str(url.to_owned())),
            ("destination".to_owned(), Value::Str(destination.to_owned())),
            ("sha256".to_owned(), Value::Null),
            ("stripComponents".to_owned(), Value::Int(0)),
        ];
        self.objects
            .borrow_mut()
            .insert(Rc::as_ptr(&rc) as *const () as usize, properties);
        Value::Opaque(rc)
    }
}

impl ObjectReader for FakeReader {
    fn class_of(&self, value: &Value) -> Option<String> {
        self.properties_of(value).map(|_| ZIP_CLASS.to_owned())
    }

    fn properties_of(&self, value: &Value) -> Option<Vec<(String, Value)>> {
        let Value::Opaque(rc) = value else {
            return None;
        };
        self.objects
            .borrow()
            .get(&(Rc::as_ptr(rc) as *const () as usize))
            .cloned()
    }
}

fn list(items: Vec<Value>) -> Value {
    Value::Array(
        items
            .into_iter()
            .enumerate()
            .map(|(i, item)| (Key::Int(i as i64), item))
            .collect(),
    )
}

fn flags(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn prepare(driver: &ZipDriver, given: &BTreeMap<String, String>) -> speedyspec_driver::Prepared {
    driver
        .prepare(&DriverInput {
            flags: given,
            interactive: false,
        })
        .unwrap()
}

#[test]
fn driver_describes_itself() {
    let driver = ZipDriver::new();
    assert_eq!(driver.id(), "zip");
    let names: Vec<(&str, bool)> = driver
        .flags()
        .iter()
        .map(|f| (f.name, f.takes_value))
        .collect();
    assert_eq!(names, vec![("base", true), ("force", false)]);
}

#[test]
fn driver_prepare_makes_base_absolute() {
    let driver = ZipDriver::new();
    let cwd = std::env::current_dir().unwrap();

    let prepared = prepare(&driver, &flags(&[]));
    assert_eq!(
        prepared.settings,
        vec![("base".to_owned(), cwd.to_string_lossy().into_owned())]
    );

    let prepared = prepare(&driver, &flags(&[("base", "x/y")]));
    let expected: PathBuf = cwd.join("x/y");
    assert_eq!(prepared.settings[0].1, expected.to_string_lossy());
}

#[test]
fn driver_check_answer_reports_item_index() {
    let reader = FakeReader::default();
    let answer = list(vec![
        reader.zip_source("https://x.test/a.zip", "a"),
        reader.zip_source("ftp://x.test/b.zip", "b"),
    ]);
    let error = ZipDriver::new().check_answer(&answer, &reader).unwrap_err();
    assert_eq!(error.field, "[1].url");
}

#[test]
fn driver_check_answer_rejects_duplicate_destination() {
    let reader = FakeReader::default();
    let answer = list(vec![
        reader.zip_source("https://x.test/a.zip", "site"),
        reader.zip_source("https://x.test/b.zip", "./site/"),
    ]);
    let error = ZipDriver::new().check_answer(&answer, &reader).unwrap_err();
    assert_eq!(error.field, "[1].destination");
}

#[test]
fn driver_check_answer_requires_a_list() {
    let reader = FakeReader::default();
    let error = ZipDriver::new()
        .check_answer(&Value::Null, &reader)
        .unwrap_err();
    assert_eq!(error.field, "");
    assert!(
        ZipDriver::new()
            .check_answer(&list(vec![]), &reader)
            .is_ok()
    );
}

#[test]
fn driver_install_returns_placements_as_arrays() {
    let base = TempDir::new().unwrap();
    let reader = FakeReader::default();
    let driver = ZipDriver::with_fetch(Box::new(three()));
    let given = flags(&[("base", base.path().to_str().unwrap())]);
    let prepared = prepare(&driver, &given);
    let answer = list(vec![reader.zip_source(WEB, "admin")]);
    let result = driver
        .install(&prepared, &answer, &reader, &mut Quiet::default())
        .unwrap();

    let Value::Array(items) = result else {
        panic!("expected a list");
    };
    assert_eq!(items.len(), 1);
    let Value::Array(fields) = &items[0].1 else {
        panic!("expected an array per placement");
    };
    let field = |name: &str| {
        fields
            .iter()
            .find(|(key, _)| *key == Key::Str(name.to_owned()))
            .map(|(_, value)| value.clone())
    };
    assert_eq!(field("url"), Some(Value::Str(WEB.to_owned())));
    assert_eq!(field("destination"), Some(Value::Str("admin".to_owned())));
    let expected = base.path().canonicalize().unwrap().join("admin");
    assert_eq!(
        field("path"),
        Some(Value::Str(expected.to_string_lossy().into_owned()))
    );
    assert_eq!(
        files(base.path()),
        vec!["admin/index.html", "admin/js/app.js"]
    );
}

#[test]
fn driver_install_honours_force() {
    let base = TempDir::new().unwrap();
    fs::write(base.path().join("keep.txt"), "mine").unwrap();
    let reader = FakeReader::default();
    let driver = ZipDriver::with_fetch(Box::new(three()));
    let answer = list(vec![reader.zip_source(WEB, ".")]);

    let given = flags(&[("base", base.path().to_str().unwrap())]);
    let refused = driver.install(
        &prepare(&driver, &given),
        &answer,
        &reader,
        &mut Quiet::default(),
    );
    let message = refused.unwrap_err().to_string();
    assert!(
        message.contains("not empty") && message.contains("--force"),
        "{message}"
    );

    let given = flags(&[("base", base.path().to_str().unwrap()), ("force", "1")]);
    driver
        .install(
            &prepare(&driver, &given),
            &answer,
            &reader,
            &mut Quiet::default(),
        )
        .unwrap();
    assert_eq!(
        files(base.path()),
        vec!["index.html", "js/app.js", "keep.txt"]
    );
}

#[test]
fn driver_install_with_no_sources_is_an_error() {
    let base = TempDir::new().unwrap();
    let reader = FakeReader::default();
    let driver = ZipDriver::with_fetch(Box::new(three()));
    let given = flags(&[("base", base.path().to_str().unwrap())]);
    let error = driver
        .install(
            &prepare(&driver, &given),
            &list(vec![]),
            &reader,
            &mut Quiet::default(),
        )
        .unwrap_err();
    assert_eq!(error.to_string(), "no plugin supplied anything to install");
}
