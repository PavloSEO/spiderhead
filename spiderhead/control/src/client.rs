//! The client side of the control channel: talk to a running SpiderHead UI.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::Value;

use crate::auth;
use crate::backend::Backend;
use crate::protocol::{self, MAX_REPLY};

/// A running SpiderHead UI, reached through its loopback control port.
pub struct Remote {
    addr: String,
    conn: Option<(BufReader<TcpStream>, TcpStream)>,
    next_id: u64,
}

impl Remote {
    /// Connect to `addr` (`127.0.0.1:7847`), failing fast when nothing is listening. The app
    /// requires a token: [`auth::TOKEN_ENV`], else the file the app wrote for this port.
    pub fn connect(addr: &str) -> std::io::Result<Self> {
        let token = auth::client_token(addr)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e))?;
        Self::connect_with_token(addr, token)
    }

    /// Like [`Remote::connect`] with a token the caller already has.
    pub fn connect_with_token(addr: &str, token: String) -> std::io::Result<Self> {
        let mut r = Self {
            addr: addr.to_string(),
            conn: None,
            next_id: 1,
        };
        r.reconnect(&token)?;
        Ok(r)
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    fn reconnect(&mut self, token: &str) -> std::io::Result<()> {
        self.conn = None;
        let mut last = std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("cannot resolve {}", self.addr),
        );
        for sa in self.addr.to_socket_addrs()? {
            match TcpStream::connect_timeout(&sa, Duration::from_secs(2)) {
                Ok(s) => {
                    s.set_nodelay(true).ok();
                    s.set_read_timeout(Some(protocol::IO_TIMEOUT)).ok();
                    s.set_write_timeout(Some(protocol::IO_TIMEOUT)).ok();
                    let read = s.try_clone()?;
                    let mut conn = (BufReader::new(read), s);
                    Self::authenticate(&mut conn, token)?;
                    self.conn = Some(conn);
                    return Ok(());
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// The handshake every connection starts with: the app closes one that doesn't send the
    /// token first.
    fn authenticate(
        conn: &mut (BufReader<TcpStream>, TcpStream),
        token: &str,
    ) -> std::io::Result<()> {
        let (reader, writer) = conn;
        writer.write_all(auth::auth_line(0, token).as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        let mut reply = String::new();
        let n = reader.by_ref().take(64 * 1024).read_line(&mut reply)?;
        let ok = n > 0
            && serde_json::from_str::<Value>(reply.trim())
                .ok()
                .and_then(|v| v.get("ok").and_then(Value::as_bool))
                == Some(true);
        if ok {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("the app refused the control token (is {} or the app's token file for this port out of date?)", auth::TOKEN_ENV),
            ))
        }
    }

    /// Write the request line body (reconnecting first when needed). An
    /// error here means the request never fully left — the app cannot have
    /// dispatched it, so the caller may safely retry on a fresh connection.
    fn send_request(&mut self, token: &str, line: &str) -> std::io::Result<()> {
        if self.conn.is_none() {
            self.reconnect(token)?;
        }
        let Some((_, writer)) = self.conn.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "not connected",
            ));
        };
        writer.write_all(line.as_bytes())
    }

    /// Terminate the request line and read the reply. An error here happens
    /// *after* the complete request was sent — the app may already be
    /// executing it, so the caller must not silently resubmit.
    fn finish_request(&mut self) -> std::io::Result<String> {
        let Some((reader, writer)) = self.conn.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "not connected",
            ));
        };
        writer.write_all(b"\n")?;
        writer.flush()?;
        let mut reply = String::new();
        let n = reader.by_ref().take(MAX_REPLY).read_line(&mut reply)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "control channel closed",
            ));
        }
        Ok(reply)
    }

    fn roundtrip(&mut self, token: &str, line: &str) -> std::io::Result<String> {
        self.send_request(token, line)?;
        self.finish_request()
    }
}

impl Backend for Remote {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        // The token never leaves this struct; fetch it once and reuse for reconnects.
        let token = auth::client_token(&self.addr).unwrap_or_default();
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let line = protocol::request_line(id, method, &params);
        // One retry with a fresh connection (the app may have restarted) —
        // but only when the request never fully left. Once the line is on
        // the wire the app may already be executing a non-idempotent
        // command, so a read failure must not silently resubmit it.
        let reply = match self.send_request(&token, &line) {
            Ok(()) => self.finish_request().map_err(|e| {
                self.conn = None;
                format!("SpiderHead UI at {} did not answer: {e}", self.addr)
            })?,
            Err(_) => {
                self.conn = None;
                self.roundtrip(&token, &line).map_err(|e| {
                    self.conn = None;
                    format!("SpiderHead UI at {} is not reachable: {e}", self.addr)
                })?
            }
        };
        let v: Value = match serde_json::from_str(reply.trim()) {
            Ok(v) => v,
            Err(e) => {
                // An unparsable or truncated reply (cut at MAX_REPLY) leaves
                // an unread tail that would desync the next request — drop
                // the connection so the next call starts clean.
                self.conn = None;
                return Err(format!("bad reply from app: {e}"));
            }
        };
        if v.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(v
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string())
        }
    }

    fn has_ui(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        format!("connected to the SpiderHead UI at {}", self.addr)
    }
}

/// Convenience for one-shot scripts: connect, call once, disconnect.
pub fn call_once(addr: &str, method: &str, params: Value) -> Result<Value, String> {
    let token = auth::client_token(addr)?;
    let mut remote =
        Remote::connect_with_token(&crate::control_addr(addr), token).map_err(|e| e.to_string())?;
    remote.call(method, params)
}
