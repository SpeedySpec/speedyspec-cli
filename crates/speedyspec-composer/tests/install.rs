//! Installing a lock into vendor/: dists from the test's server, checked and swapped in whole.

use std::fs;
use std::path::Path;

use serde_json::{Value as Json, json};
use sha1::{Digest, Sha1};
use speedyspec_composer::{Lock, Maps, Platform, install};
use speedyspec_driver_zip::HttpFetch;
use speedyspec_testkit::{Entry, Route, Server, build_zip};

fn platform() -> Platform {
    Platform {
        php_version: "8.5.12".into(),
        extensions: vec!["json".into(), "curl".into()],
    }
}

fn dist(top: &str) -> Vec<u8> {
    build_zip(&[
        (&format!("{top}/"), Entry::Dir),
        (&format!("{top}/src/"), Entry::Dir),
        (
            &format!("{top}/src/Client.php"),
            Entry::file("<?php namespace Acme\\Http; class Client {}"),
        ),
    ])
}

fn sha1(bytes: &[u8]) -> String {
    Sha1::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn package(name: &str, url: &str, shasum: &str, require: Json) -> Json {
    json!({
        "name": name, "version": "1.0.0", "type": "library",
        "dist": {"type": "zip", "url": url, "shasum": shasum},
        "require": require,
        "autoload": {"psr-4": {"Acme\\Http\\": "src/"}},
    })
}

fn plugin(dir: &Path, packages: Vec<Json>) -> Lock {
    fs::write(
        dir.join("composer.json"),
        json!({"require": {"acme/http": "^1"}}).to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("composer.lock"),
        json!({ "packages": packages }).to_string(),
    )
    .unwrap();
    Lock::read(dir).unwrap().unwrap()
}

fn url(server: &Server, path: &str) -> String {
    format!("{}{path}", server.url())
}

#[test]
fn installs_packages_and_their_maps() {
    let zip = dist("acme-http-abc123");
    let sum = sha1(&zip);
    let server = Server::start(vec![("/http.zip", Route::ok(zip))]);
    let dir = tempfile::tempdir().unwrap();
    let lock = plugin(
        dir.path(),
        vec![package(
            "acme/http",
            &url(&server, "/http.zip"),
            &sum,
            json!({"php": "^8.0", "ext-json": "*"}),
        )],
    );
    let installed = install(dir.path(), &lock, &HttpFetch::new(), &platform()).unwrap();
    assert!(installed.warnings.is_empty());
    assert!(
        dir.path().join("vendor/acme/http/src/Client.php").is_file(),
        "the top directory is stripped"
    );
    let maps = Maps::read(dir.path()).unwrap().unwrap();
    assert_eq!(
        maps.find(dir.path(), "Acme\\Http\\Client"),
        Some(dir.path().join("vendor/acme/http/src/Client.php"))
    );
    assert!(!dir.path().join("vendor.speedyspec-new").exists());
}

#[test]
fn a_flat_dist_is_the_package_itself() {
    let zip = build_zip(&[
        ("src/", Entry::Dir),
        ("src/Client.php", Entry::file("<?php")),
        ("README", Entry::file("x")),
    ]);
    let server = Server::start(vec![("/flat.zip", Route::ok(zip))]);
    let dir = tempfile::tempdir().unwrap();
    let lock = plugin(
        dir.path(),
        vec![package(
            "acme/http",
            &url(&server, "/flat.zip"),
            "",
            json!({}),
        )],
    );
    install(dir.path(), &lock, &HttpFetch::new(), &platform()).unwrap();
    assert!(dir.path().join("vendor/acme/http/src/Client.php").is_file());
    assert!(dir.path().join("vendor/acme/http/README").is_file());
}

fn old_vendor(dir: &Path) {
    fs::create_dir_all(dir.join("vendor/old/kept")).unwrap();
    fs::write(dir.join("vendor/old/kept/file.php"), "<?php").unwrap();
}

#[test]
fn a_bad_sha1_is_refused_and_keeps_the_old_vendor() {
    let server = Server::start(vec![("/http.zip", Route::ok(dist("x")))]);
    let dir = tempfile::tempdir().unwrap();
    old_vendor(dir.path());
    let lock = plugin(
        dir.path(),
        vec![package(
            "acme/http",
            &url(&server, "/http.zip"),
            &"0".repeat(40),
            json!({}),
        )],
    );
    let error = install(dir.path(), &lock, &HttpFetch::new(), &platform())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("acme/http") && error.contains("checksum mismatch"),
        "{error}"
    );
    assert!(dir.path().join("vendor/old/kept/file.php").is_file());
    assert!(!dir.path().join("vendor.speedyspec-new").exists());
}

#[test]
fn an_unsafe_dist_is_refused() {
    let zip = build_zip(&[
        ("x/", Entry::Dir),
        ("x/../../evil.php", Entry::file("<?php")),
    ]);
    let server = Server::start(vec![("/evil.zip", Route::ok(zip))]);
    let dir = tempfile::tempdir().unwrap();
    old_vendor(dir.path());
    let lock = plugin(
        dir.path(),
        vec![package(
            "acme/http",
            &url(&server, "/evil.zip"),
            "",
            json!({}),
        )],
    );
    let error = install(dir.path(), &lock, &HttpFetch::new(), &platform())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("acme/http") && error.contains("refused"),
        "{error}"
    );
    assert!(dir.path().join("vendor/old/kept/file.php").is_file());
    assert!(!dir.path().parent().unwrap().join("evil.php").exists());
}

#[test]
fn a_missing_extension_names_the_package_before_downloading() {
    let server = Server::start(vec![("/http.zip", Route::ok(dist("x")))]);
    let dir = tempfile::tempdir().unwrap();
    let lock = plugin(
        dir.path(),
        vec![package(
            "acme/http",
            &url(&server, "/http.zip"),
            "",
            json!({"ext-gmp": "*"}),
        )],
    );
    let error = install(dir.path(), &lock, &HttpFetch::new(), &platform())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("acme/http") && error.contains("gmp"),
        "{error}"
    );
    assert!(server.requests().is_empty());
}

#[test]
fn a_failed_download_names_the_package() {
    let server = Server::start(vec![("/missing.zip", Route::status(404))]);
    let dir = tempfile::tempdir().unwrap();
    let lock = plugin(
        dir.path(),
        vec![package(
            "acme/http",
            &url(&server, "/missing.zip"),
            "",
            json!({}),
        )],
    );
    let error = install(dir.path(), &lock, &HttpFetch::new(), &platform())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("acme/http") && error.contains("404"),
        "{error}"
    );
    assert!(!dir.path().join("vendor").exists());
}

#[test]
fn a_leftover_old_vendor_is_put_back_rather_than_deleted() {
    // A crash between moving vendor/ aside and moving the new one in leaves only vendor.speedyspec-old.
    let server = Server::start(vec![("/missing.zip", Route::status(404))]);
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("vendor.speedyspec-old/old/kept")).unwrap();
    fs::write(
        dir.path().join("vendor.speedyspec-old/old/kept/file.php"),
        "<?php",
    )
    .unwrap();
    let lock = plugin(
        dir.path(),
        vec![package(
            "acme/http",
            &url(&server, "/missing.zip"),
            "",
            json!({}),
        )],
    );
    assert!(install(dir.path(), &lock, &HttpFetch::new(), &platform()).is_err());
    assert!(
        dir.path().join("vendor/old/kept/file.php").is_file(),
        "the only copy was deleted"
    );
}

#[test]
fn the_checksum_is_checked_before_anything_is_unpacked() {
    let zip = build_zip(&[
        ("x/", Entry::Dir),
        ("x/../../evil.php", Entry::file("<?php")),
    ]);
    let server = Server::start(vec![("/evil.zip", Route::ok(zip))]);
    let dir = tempfile::tempdir().unwrap();
    let lock = plugin(
        dir.path(),
        vec![package(
            "acme/http",
            &url(&server, "/evil.zip"),
            &"0".repeat(40),
            json!({}),
        )],
    );
    let error = install(dir.path(), &lock, &HttpFetch::new(), &platform())
        .unwrap_err()
        .to_string();
    assert!(error.contains("checksum mismatch"), "{error}");
}
