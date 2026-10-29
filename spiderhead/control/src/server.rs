//! The app-side control server: a loopback JSON-lines port.
//!
//! Each connection may send many requests; each line `{"id":…,"method":…,"params":…}` gets one
//! reply line `{"id":…,"ok":…}`. The first line of a connection must authenticate with the
//! app's token (see [`crate::auth`]); a connection that doesn't — or that later sends a line
//! which is not a valid request — is answered once and closed, so nothing sent after it runs.
//!
//! Requests are handed to the application as [`ControlRequest`]s on a channel. A GUI app
//! drains that channel on its UI thread (all widget access stays there) and answers through
//! `reply`. [`serve_with_handler`] is the headless convenience form: a drainer thread calls a
//! closure per request, which is also how the tests drive the server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

use serde_json::Value;

use crate::auth::{self, AUTH_TIMEOUT};
use crate::protocol::{self, MAX_LINE};

/// One control request waiting for the application. Send the outcome through `reply`.
pub struct ControlRequest {
    pub method: String,
    pub params: Value,
    pub reply: Sender<Result<Value, String>>,
}

/// Type of the headless per-request handler.
pub type Handler = Arc<dyn Fn(&str, &Value) -> Result<Value, String> + Send + Sync>;

/// Listen on `listener`; requests arrive on the returned receiver. `wake` is called when a
/// request is queued (a GUI passes `|| ctx.request_repaint()`).
pub fn serve(
    listener: TcpListener,
    token: Arc<str>,
    tx: Sender<ControlRequest>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
) -> std::io::Result<()> {
    for stream in listener.incoming().flatten() {
        let tx = tx.clone();
        let token = Arc::clone(&token);
        let wake = wake.clone();
        let _ = std::thread::Builder::new()
            .name("control-conn".into())
            .spawn(move || {
                serve_connection(stream, &token, tx, wake);
            });
    }
    Ok(())
}

/// [`serve`] plus a built-in drainer thread answering every request with `handler`.
/// Blocks forever; intended for headless sessions and tests (call it from a thread).
pub fn serve_with_handler(
    listener: TcpListener,
    token: Arc<str>,
    handler: Handler,
) -> std::io::Result<Sender<ControlRequest>> {
    let (tx, rx) = channel::<ControlRequest>();
    std::thread::Builder::new()
        .name("control-drain".into())
        .spawn(move || {
            while let Ok(req) = rx.recv() {
                let _ = req.reply.send(handler(&req.method, &req.params));
            }
        })?;
    serve(listener, token, tx.clone(), None)?;
    Ok(tx)
}

/// The full life of one connection: authenticate, then a request line → a reply line.
fn serve_connection(
    stream: TcpStream,
    token: &str,
    tx: Sender<ControlRequest>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    let _ = stream.set_nodelay(true);
    let write_half = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let mut out = write_half;
    if !authenticate(&mut reader, &mut out, token) {
        return;
    }
    let _ = out.set_read_timeout(Some(protocol::IO_TIMEOUT));
    loop {
        let mut line = String::new();
        let read = reader.by_ref().take(MAX_LINE as u64).read_line(&mut line);
        match read {
            Ok(0) => return, // the client went away
            Ok(_) if line.ends_with('\n') => {}
            _ => return, // over the line cap or a dropped partial line: hang up
        }
        match protocol::parse_request(&line) {
            None => continue, // blank line
            Some(Err(message)) => {
                let _ = writeln!(out, "{}", protocol::error_reply(&Value::Null, message));
                return;
            }
            Some(Ok((id, method, params))) => {
                let (reply_tx, reply_rx) = channel();
                let request = ControlRequest {
                    method,
                    params,
                    reply: reply_tx,
                };
                if tx.send(request).is_err() {
                    let _ = writeln!(
                        out,
                        "{}",
                        protocol::error_reply(&id, "the application is shutting down")
                    );
                    return;
                }
                if let Some(wake) = &wake {
                    wake();
                }
                let answer = match reply_rx.recv() {
                    Ok(result) => match result {
                        Ok(result) => protocol::ok_reply(&id, result),
                        Err(message) => protocol::error_reply(&id, message),
                    },
                    // The application dropped the reply channel without answering.
                    Err(_) => protocol::error_reply(&id, "the application did not answer"),
                };
                if writeln!(out, "{answer}").is_err() {
                    return;
                }
            }
        }
    }
}

/// Read the connection's first line (at most [`auth::MAX_AUTH_LINE`] bytes, within
/// [`AUTH_TIMEOUT`]) and check the token. `true` when the peer may go on.
fn authenticate(reader: &mut BufReader<TcpStream>, out: &mut TcpStream, token: &str) -> bool {
    let _ = out.set_read_timeout(Some(AUTH_TIMEOUT));
    let mut first = String::new();
    let read = reader
        .by_ref()
        .take(auth::MAX_AUTH_LINE as u64)
        .read_line(&mut first);
    let verdict = match read {
        Ok(n) if n > 0 && first.ends_with('\n') => auth::check_first_line(&first, token),
        _ => Err(serde_json::json!({"ok": false, "error": "authentication required"})),
    };
    let allowed = verdict.is_ok();
    let reply = verdict.unwrap_or_else(|e| e);
    let _ = writeln!(out, "{reply}");
    let _ = out.set_read_timeout(None);
    allowed
}
