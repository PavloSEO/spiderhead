//! Raw-socket checks on the fixture server: status lines, the fixed header
//! block, route overrides and file resolution, byte for byte.

use spiderhead_oracle_testkit::FixtureServer;
use std::io::{Read, Write};
use std::net::TcpStream;

/// One raw GET; returns the full response bytes (the server closes the
/// connection after answering, so read_to_end terminates).
fn get(port: u16, target: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
        .expect("write");
    let mut out = Vec::new();
    stream.read_to_end(&mut out).expect("read");
    out
}

fn head_of(response: &[u8]) -> String {
    let end = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(response.len());
    String::from_utf8_lossy(&response[..end]).into_owned()
}

fn body_of(response: &[u8]) -> &[u8] {
    let end = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(response.len());
    &response[end..]
}

fn port_of(server: &FixtureServer) -> u16 {
    server.base.rsplit(':').next().unwrap().parse().unwrap()
}

#[test]
fn static_page_carries_the_fixed_headers() {
    let server = FixtureServer::start("basic");
    let raw = get(port_of(&server), "/");
    let head = head_of(&raw);
    assert_eq!(
        head.lines().collect::<Vec<_>>(),
        [
            "HTTP/1.1 200 OK",
            "Date: Mon, 01 Jan 2026 00:00:00 GMT",
            "Server: oracle-fixture",
            "Content-Type: text/html; charset=utf-8",
            &format!("Content-Length: {}", body_of(&raw).len()),
            "Last-Modified: Mon, 01 Jan 2026 00:00:00 GMT",
            "Connection: close",
        ],
        "head: {head}"
    );
    assert!(body_of(&raw).starts_with(b"<!doctype html>") || body_of(&raw).starts_with(b"<"));
}

#[test]
fn missing_path_is_the_fixed_404() {
    let server = FixtureServer::start("basic");
    let raw = get(port_of(&server), "/nope.html");
    assert!(head_of(&raw).starts_with("HTTP/1.1 404 Not Found"));
    assert_eq!(
        body_of(&raw),
        b"<!doctype html><html><head><title>Not Found</title></head><body><h1>Not Found</h1></body></html>\n"
    );
}

#[test]
fn route_redirect_has_no_body_and_a_location() {
    let server = FixtureServer::start("redirects");
    let raw = get(port_of(&server), "/r301");
    let head = head_of(&raw);
    assert!(head.starts_with("HTTP/1.1 301 Moved Permanently"), "{head}");
    assert!(head.lines().any(|l| l == "Location: /final.html"), "{head}");
    assert_eq!(body_of(&raw), b"");
}

#[test]
fn route_status_with_retry_after_keeps_header_order() {
    let server = FixtureServer::start("errors");
    let raw = get(port_of(&server), "/unavailable.html");
    let head = head_of(&raw);
    assert!(
        head.starts_with("HTTP/1.1 503 Service Unavailable"),
        "{head}"
    );
    assert!(
        head.lines().last() == Some("Retry-After: 120"),
        "route headers come last: {head}"
    );
}

#[test]
fn route_body_and_status_without_file() {
    let server = FixtureServer::start("check-errors");
    let raw = get(port_of(&server), "/robots.txt");
    assert!(head_of(&raw).starts_with("HTTP/1.1 500 Internal Server Error"));
    assert_eq!(
        body_of(&raw),
        b"<!doctype html><html><head><title>Not Found</title></head><body><h1>Not Found</h1></body></html>\n"
    );
}

#[test]
fn text_files_expand_base() {
    let server = FixtureServer::start("canonical-hreflang");
    let raw = get(port_of(&server), "/canon-self.html");
    let body = String::from_utf8_lossy(body_of(&raw));
    assert!(body.contains(&server.base), "no base in body");
    assert!(!body.contains("{{BASE}}"), "placeholder left: {body}");
}

#[test]
fn dotdot_escape_is_a_404() {
    let server = FixtureServer::start("basic");
    let raw = get(port_of(&server), "/../../Cargo.toml");
    assert!(head_of(&raw).starts_with("HTTP/1.1 404"));
}

#[test]
fn head_reports_length_without_a_body() {
    let server = FixtureServer::start("basic");
    let mut stream = TcpStream::connect((
        "127.0.0.1",
        server
            .base
            .rsplit(':')
            .next()
            .unwrap()
            .parse::<u16>()
            .unwrap(),
    ))
    .expect("connect");
    stream
        .write_all(b"HEAD / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .expect("write");
    let mut out = Vec::new();
    stream.read_to_end(&mut out).expect("read");
    let head = head_of(&out);
    let len: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    assert!(len > 0, "head: {head}");
    assert_eq!(body_of(&out), b"");
}
