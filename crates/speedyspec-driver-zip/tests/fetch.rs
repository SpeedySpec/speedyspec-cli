//! `HttpFetch` against a loopback server.

use speedyspec_driver_zip::{Fetch, FetchError, HttpFetch};
use std::time::{Duration, Instant};

use speedyspec_testkit::{Route, Server};

fn fetch(url: &str) -> (Result<u64, FetchError>, Vec<u8>) {
    let mut body = Vec::new();
    let result = HttpFetch::new().fetch(url, &mut body);
    (result, body)
}

#[test]
fn fetches_a_body() {
    let server = Server::start(vec![("/a", Route::ok("hello"))]);
    let (result, body) = fetch(&format!("{}/a", server.url()));
    assert_eq!(result.unwrap(), 5);
    assert_eq!(body, b"hello");
}

#[test]
fn follows_redirects() {
    let server = Server::start(vec![("/c", Route::ok("end"))]);
    server.route("/b", Route::redirect(format!("{}/c", server.url())));
    // A relative Location must work too.
    server.route("/a", Route::redirect("/b"));
    let (result, body) = fetch(&format!("{}/a", server.url()));
    assert_eq!(result.unwrap(), 3);
    assert_eq!(body, b"end");
}

#[test]
fn stops_after_ten_redirects() {
    let server = Server::start(vec![("/loop", Route::redirect("/loop"))]);
    let (result, body) = fetch(&format!("{}/loop", server.url()));
    assert!(
        matches!(result, Err(FetchError::TooManyRedirects { .. })),
        "{result:?}"
    );
    assert!(body.is_empty());
    assert_eq!(
        server.requests().len(),
        11,
        "the first request plus ten redirects"
    );
}

#[test]
fn reports_http_status() {
    let server = Server::start(vec![]);
    let (result, body) = fetch(&format!("{}/missing", server.url()));
    assert!(
        matches!(result, Err(FetchError::Status { status: 404, .. })),
        "{result:?}"
    );
    assert!(body.is_empty());
}

#[test]
fn truncated_body_is_an_error() {
    let server = Server::start(vec![("/t", Route::truncated("0123456789", 3))]);
    let (result, _) = fetch(&format!("{}/t", server.url()));
    assert!(matches!(result, Err(FetchError::Io { .. })), "{result:?}");
}

#[test]
fn refuses_http_to_a_non_loopback_host() {
    let (result, body) = fetch("http://example.com/x");
    match result {
        Err(FetchError::Insecure { url, .. }) => assert_eq!(url, "http://example.com/x"),
        other => panic!("{other:?}"),
    }
    assert!(body.is_empty());
}

#[test]
fn refuses_a_redirect_to_plain_http_elsewhere() {
    let server = Server::start(vec![("/a", Route::redirect("http://example.com/x"))]);
    let (result, _) = fetch(&format!("{}/a", server.url()));
    match result {
        Err(FetchError::Insecure { to, .. }) => assert_eq!(to, "http://example.com/x"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn errors_name_the_url() {
    let server = Server::start(vec![]);
    let url = format!("{}/missing", server.url());
    let (result, _) = fetch(&url);
    let message = result.unwrap_err().to_string();
    assert!(message.contains(&url), "{message}");
    assert!(message.contains("404"), "{message}");
}

#[test]
fn a_server_that_never_answers_times_out() {
    let server = Server::start(vec![("/stall", Route::stalled("", 0))]);
    let fetch = HttpFetch::with_timeouts(Duration::from_millis(300), Duration::from_millis(300));
    let started = Instant::now();
    let mut body = Vec::new();
    let result = fetch.fetch(&format!("{}/stall", server.url()), &mut body);
    assert!(matches!(result, Err(FetchError::Io { .. })), "{result:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "must not wait for the server"
    );
}

#[test]
fn a_body_that_stalls_times_out() {
    let server = Server::start(vec![("/stall", Route::stalled("0123456789", 3))]);
    let fetch = HttpFetch::with_timeouts(Duration::from_secs(5), Duration::from_millis(300));
    let started = Instant::now();
    let mut body = Vec::new();
    let result = fetch.fetch(&format!("{}/stall", server.url()), &mut body);
    assert!(matches!(result, Err(FetchError::Io { .. })), "{result:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "must not wait for the server"
    );
}
