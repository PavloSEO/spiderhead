//! Live protocol tests: a real server on an ephemeral port, a real client.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use serde_json::{json, Value};

use spiderhead_control::auth;
use spiderhead_control::backend::Backend;
use spiderhead_control::client::Remote;
use spiderhead_control::protocol;
use spiderhead_control::server::{self, Handler};

/// A server answering `echo` with its params and `fail` with an error; everything else is
/// "unknown method".
fn start_test_server(token: &str) -> (String, std::net::SocketAddr) {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let token: Arc<str> = Arc::from(token);
    let token_string = token.to_string();
    let handler: Handler = Arc::new(|method: &str, params: &Value| match method {
        "echo" => Ok(params.clone()),
        "fail" => Err("the scan is not loaded".into()),
        _ => Err(format!("unknown method: {method}")),
    });
    std::thread::spawn(move || server::serve_with_handler(listener, token, handler).unwrap());
    (token_string, addr)
}

#[test]
fn happy_path_authenticate_call_and_disconnect() {
    let (token, addr) = start_test_server("0123456789abcdef");
    let mut remote = Remote::connect_with_token(&addr.to_string(), token).unwrap();
    assert!(remote.has_ui());
    assert!(remote.describe().contains(&addr.to_string()));

    let result = remote.call("echo", json!({"x": 7})).unwrap();
    assert_eq!(result, json!({"x": 7}));

    let error = remote.call("fail", json!({})).unwrap_err();
    assert_eq!(error, "the scan is not loaded");

    let error = remote.call("no.such", json!({})).unwrap_err();
    assert!(error.contains("unknown method"));
}

#[test]
fn wrong_token_is_refused() {
    let (_, addr) = start_test_server("0123456789abcdef");
    let err = Remote::connect_with_token(&addr.to_string(), "0123456789abcdeX".into())
        .err()
        .expect("the wrong token must be refused");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    // The server survives the refusal: a good client can still connect.
    let (token, addr) = start_test_server("fedcba9876543210");
    let mut remote = Remote::connect_with_token(&addr.to_string(), token).unwrap();
    assert_eq!(remote.call("echo", json!(null)).unwrap(), Value::Null);
}

#[test]
fn garbage_after_auth_gets_one_error_reply_and_the_connection_closes() {
    let (token, addr) = start_test_server("0123456789abcdef");
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(auth::auth_line(0, &token).as_bytes())
        .unwrap();
    stream.write_all(b"\n").unwrap();
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    reader.read_line(&mut reply).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(reply.trim()).unwrap()["ok"],
        json!(true)
    );

    let mut stream = reader.into_inner();
    stream.write_all(b"GET / HTTP/1.1\n").unwrap();
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    reader.read_line(&mut reply).unwrap();
    let v: Value = serde_json::from_str(reply.trim()).unwrap();
    assert_eq!(v["ok"], json!(false));
    // The server closed the connection after the error reply.
    let mut rest = Vec::new();
    let _ = reader.read_to_end(&mut rest);
    assert!(rest.is_empty());
}

#[test]
fn blank_lines_are_skipped() {
    let (token, addr) = start_test_server("0123456789abcdef");
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(auth::auth_line(0, &token).as_bytes())
        .unwrap();
    stream.write_all(b"\n\n\n").unwrap();
    stream
        .write_all(protocol::request_line(1, "echo", &json!({"ok": 1})).as_bytes())
        .unwrap();
    stream.write_all(b"\n").unwrap();
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    reader.read_line(&mut reply).unwrap(); // auth
    let mut reply = String::new();
    reader.read_line(&mut reply).unwrap(); // the echo answer (blank lines skipped)
    let v: Value = serde_json::from_str(reply.trim()).unwrap();
    assert_eq!(v["id"], json!(1));
    assert_eq!(v["result"], json!({"ok": 1}));
}

// regression (review 2026-10-27): a reply that fails to parse leaves its
// unread tail on the socket; the next call would read that tail as the
// answer to a different request. call() must drop the connection.
#[test]
fn bad_reply_drops_the_connection_instead_of_desyncing_the_next_call() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for (i, stream) in listener.incoming().enumerate() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            stream
                .write_all(b"{\"id\":0,\"ok\":true}\n")
                .unwrap_or_default();
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            if i == 0 {
                // Poison: a garbage line, then a stray reply that a kept
                // connection would hand to the *next* request.
                stream
                    .write_all(b"not json\n{\"id\":2,\"ok\":true,\"result\":\"stale\"}\n")
                    .unwrap_or_default();
            } else {
                stream
                    .write_all(b"{\"id\":2,\"ok\":true,\"result\":\"fresh\"}\n")
                    .unwrap_or_default();
            }
        }
    });
    let mut remote =
        Remote::connect_with_token(&addr.to_string(), "0123456789abcdef".into()).unwrap();
    let err = remote.call("echo", json!({})).unwrap_err();
    assert!(err.contains("bad reply"), "{err}");
    // On a fresh connection the next call cannot inherit the stale reply.
    assert_eq!(remote.call("echo", json!({})).unwrap(), json!("fresh"));
}

// regression (review 2026-10-27): after the request line was flushed the app
// may already be executing it — a read failure must not resubmit what could
// be a non-idempotent command. Only send-side failures may retry.
#[test]
fn a_sent_request_is_never_resubmitted() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let count = Arc::new(AtomicUsize::new(0));
    let count_thread = count.clone();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            stream
                .write_all(b"{\"id\":0,\"ok\":true}\n")
                .unwrap_or_default();
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) > 0 {
                // The request arrived — then the connection dies silently,
                // like the app crashing after it already ran the command.
                count_thread.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    let mut remote =
        Remote::connect_with_token(&addr.to_string(), "0123456789abcdef".into()).unwrap();
    let err = remote.call("scan.start", json!({})).unwrap_err();
    assert!(err.contains("did not answer"), "{err}");
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "the request must arrive exactly once"
    );
}

#[test]
fn a_dead_port_fails_fast_with_a_clear_reason() {
    // Port 1 is never ours; connecting is refused.
    let err = Remote::connect_with_token("127.0.0.1:1", "0123456789abcdef".into())
        .err()
        .expect("a dead port must be refused");
    assert_eq!(err.kind(), std::io::ErrorKind::ConnectionRefused);
    // Without a token source (no env var in tests, no file for this port) the client explains itself.
    let err = Remote::connect("127.0.0.1:1")
        .err()
        .expect("no token must be refused");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(err.to_string().contains(auth::TOKEN_ENV));
}
