//! Process-facing inputs the logic reads: environment variables, DNS resolution and the zstd capability.
//!
//! All of these go through [`Host`], so every decision is testable without the network or the real environment.
//! without touching the network or the real environment. Tests use [`FakeHost`].

use std::collections::BTreeMap;
use std::net::SocketAddr;

/// Everything outside the pure logic: env vars, DNS and zstd capability.
pub trait Host {
    /// The exact process variable `name`, or `None` when unset.
    fn var(&self, name: &str) -> Option<String>;
    /// Resolve `host` at `port` like `socket.getaddrinfo`; `Err` when resolution fails
    /// or yields nothing.
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String>;
    /// Whether a bounded zstd decoder is available.
    fn zstd_available(&self) -> bool;
}

/// The real process: `std::env`, the system resolver, and no zstd decoder built in.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemHost;

impl Host for SystemHost {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
        use std::net::ToSocketAddrs;
        let found: Vec<SocketAddr> = (host, port)
            .to_socket_addrs()
            .map_err(|error| error.to_string())?
            .collect();
        if found.is_empty() {
            Err(format!("no address for {host}"))
        } else {
            Ok(found)
        }
    }

    fn zstd_available(&self) -> bool {
        false
    }
}

/// In-memory [`Host`] for tests: a fixed environment, fixed DNS answers and a zstd flag.
#[derive(Debug, Default, Clone)]
pub struct FakeHost {
    /// Environment variables by exact name.
    pub env: BTreeMap<String, String>,
    /// DNS answers by lowercase host name; a missing host fails to resolve.
    pub dns: BTreeMap<String, Vec<SocketAddr>>,
    /// Whether zstd decoding is reported as available.
    pub zstd: bool,
}

impl Host for FakeHost {
    fn var(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    fn resolve(&self, host: &str, _port: u16) -> Result<Vec<SocketAddr>, String> {
        match self
            .dns
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(host))
        {
            Some((_, addrs)) if !addrs.is_empty() => Ok(addrs.clone()),
            _ => Err(format!("no address for {host}")),
        }
    }

    fn zstd_available(&self) -> bool {
        self.zstd
    }
}
