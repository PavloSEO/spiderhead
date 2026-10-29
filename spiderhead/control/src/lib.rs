//! The SpiderHead control channel.
//!
//! A running SpiderHead UI can expose a loopback JSON-lines TCP port
//! (`spiderhead-desktop --control <port>`): one `{"id","method","params"}` line in, one
//! `{"id","ok","result"|"error"}` line out. Every action the window can take is a named
//! command, so an agent drives the live app exactly like a headless session.
//!
//! - [`server`] hosts the port: connections must authenticate first ([`auth`]), then each
//!   line becomes a [`ControlRequest`] for the application (in the UI: marshalled to the
//!   UI thread; headless: answered directly).
//! - [`Remote`] is the client side: it authenticates, sends requests and reads replies,
//!   reconnecting once if the app restarted.
//! - [`Backend`] is the minimal interface the tool layer (MCP, CLI) programs against, so
//!   it never knows whether it talks to a live window or a headless session.

#![forbid(unsafe_code)]

pub mod auth;
pub mod backend;
pub mod client;
pub mod protocol;
pub mod registry;
pub mod server;

pub use auth::{client_token, token_path, TOKEN_ENV};
pub use backend::Backend;
pub use client::Remote;
pub use registry::CommandInfo;
pub use server::{serve, serve_with_handler, ControlRequest};

/// Default control-channel port of the desktop app.
pub const DEFAULT_PORT: u16 = 7847;

/// `"7847"` → `"127.0.0.1:7847"`; `"host:port"` is kept as is.
pub fn control_addr(s: &str) -> String {
    if s.parse::<u16>().is_ok() {
        format!("127.0.0.1:{s}")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_port_gets_the_loopback_host() {
        assert_eq!(control_addr("7847"), "127.0.0.1:7847");
        assert_eq!(control_addr("127.0.0.1:7847"), "127.0.0.1:7847");
        assert_eq!(control_addr("localhost:99"), "localhost:99");
    }
}
