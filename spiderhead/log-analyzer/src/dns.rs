//! Forward-confirmed reverse DNS for crawler authenticity.
//!
//! A user agent is trivial to spoof, so a `Googlebot` string proves nothing. A crawler is
//! authentic when the client's PTR hostname ends in an official suffix and that hostname
//! resolves forward to the same IP. Name resolution and time are injected so the logic
//! runs offline and deterministically in tests.

use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use crate::bots::suffixes_for;
use crate::net::parse_ip;

/// How long a verification result stays fresh, in seconds.
pub const DNS_CACHE_TTL: f64 = 3600.0;
/// Default per-lookup deadline.
pub const RDNS_TIMEOUT: Duration = Duration::from_secs(3);
/// Address with a stable reverse record, used to tell "DNS blocked" from "bot is fake".
pub const DNS_CANARY: &str = "8.8.8.8";
const CACHE_CAPACITY: usize = 1024;

/// Why a lookup produced no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsFailure {
    /// The name or address definitively does not exist (NXDOMAIN).
    NotFound,
    /// The name exists but holds no record of the requested type.
    NoData,
    /// A temporary failure; says nothing about the record.
    TryAgain,
    /// Any other failure, with the resolver's description.
    Other(String),
}

/// Name resolution used by verification.
pub trait Resolver: Send + Sync {
    /// PTR hostname for `ip`.
    fn reverse(&self, ip: IpAddr) -> Result<String, DnsFailure>;
    /// Addresses `host` resolves to, in the family of `ipv6`.
    fn forward(&self, host: &str, ipv6: bool) -> Result<Vec<String>, DnsFailure>;
}

/// Time source and pacing for verification.
pub trait Clock: Send + Sync {
    /// Monotonic seconds, for cache expiry within one process.
    fn monotonic(&self) -> f64;
    /// Wall-clock Unix seconds, stored with persisted evidence.
    fn wall(&self) -> f64;
    /// Pause between fresh lookups so resolvers are not hammered.
    fn sleep(&self, duration: Duration);
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn monotonic(&self) -> f64 {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_secs_f64()
    }

    fn wall(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64())
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Bounded number of in-flight resolver threads. A stalled system resolver cannot be
/// interrupted, so abandoned workers keep their slot until they return.
#[derive(Debug)]
pub struct Gate {
    capacity: usize,
    used: AtomicUsize,
}

impl Gate {
    /// A gate admitting `capacity` concurrent workers.
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity,
            used: AtomicUsize::new(0),
        })
    }

    fn acquire(&self) -> bool {
        let mut current = self.used.load(Ordering::SeqCst);
        loop {
            if current >= self.capacity {
                return false;
            }
            match self.used.compare_exchange(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    fn release(&self) {
        self.used.fetch_sub(1, Ordering::SeqCst);
    }
}

type Evidence = Map<String, Value>;
type CacheKey = (String, Vec<String>);

fn base(reason: &str) -> Evidence {
    let mut m = Map::new();
    m.insert("verified".into(), Value::Null);
    m.insert("hostname".into(), Value::Null);
    m.insert("forward_addresses".into(), json!([]));
    m.insert("forward_match".into(), Value::Null);
    m.insert("reason".into(), json!(reason));
    m
}

/// Shared verifier: resolver, clock, worker gate and the freshness cache.
pub struct Verifier {
    resolver: Arc<dyn Resolver>,
    clock: Arc<dyn Clock>,
    gate: Arc<Gate>,
    timeout: Duration,
    cache: Mutex<IndexMap<CacheKey, (f64, Evidence)>>,
}

impl Verifier {
    /// A verifier with default deadline and four concurrent workers.
    pub fn new(resolver: Arc<dyn Resolver>, clock: Arc<dyn Clock>) -> Self {
        Self::with_limits(resolver, clock, Gate::new(4), RDNS_TIMEOUT)
    }

    /// A verifier with an explicit worker gate and per-lookup deadline.
    pub fn with_limits(
        resolver: Arc<dyn Resolver>,
        clock: Arc<dyn Clock>,
        gate: Arc<Gate>,
        timeout: Duration,
    ) -> Self {
        Self {
            resolver,
            clock,
            gate,
            timeout,
            cache: Mutex::new(IndexMap::new()),
        }
    }

    /// Forget every cached result.
    pub fn clear_cache(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    /// The most resolver lookups admitted at once; callers must not exceed it.
    pub fn capacity(&self) -> usize {
        self.gate.capacity
    }

    /// Verify one IP against official suffixes.
    ///
    /// The result distinguishes a failed authenticity check (`verified: false`) from an
    /// unavailable lookup (`verified: null`); collapsing both would misreport evidence.
    pub fn verify_bot_rdns(&self, ip: &str, suffixes: &[&str]) -> Evidence {
        self.verify_with_timeout(ip, suffixes, self.timeout)
    }

    /// As [`Verifier::verify_bot_rdns`] with a caller-chosen deadline.
    pub fn verify_with_timeout(&self, ip: &str, suffixes: &[&str], timeout: Duration) -> Evidence {
        if suffixes.is_empty() {
            let mut m = Map::new();
            m.insert("verified".into(), Value::Null);
            m.insert(
                "reason".into(),
                json!("No official PTR suffix is available for this bot"),
            );
            m.insert("hostname".into(), Value::Null);
            return m;
        }
        let Some(address) = parse_ip(ip) else {
            let mut m = Map::new();
            m.insert("verified".into(), Value::Null);
            m.insert("hostname".into(), Value::Null);
            m.insert("reason".into(), json!("Invalid IP address"));
            return m;
        };
        if !self.gate.acquire() {
            return base("DNS resolver worker capacity is exhausted");
        }
        let (sender, receiver) = channel();
        let resolver = Arc::clone(&self.resolver);
        let gate = Arc::clone(&self.gate);
        let owned: Vec<String> = suffixes.iter().map(|s| s.to_string()).collect();
        let ip_text = ip.to_string();
        let spawned = std::thread::Builder::new()
            .name("rdns-lookup".into())
            .spawn(move || {
                let result = lookup(resolver.as_ref(), address, &ip_text, &owned);
                gate.release();
                let _ = sender.send(result);
            });
        if spawned.is_err() {
            self.gate.release();
            return base("DNS resolver worker is unavailable");
        }
        match receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                base("DNS verification deadline exceeded")
            }
        }
    }

    /// Whether reverse DNS works from this environment at all. Without this check, blocked
    /// DNS would make every bot look fake.
    pub fn dns_available(&self) -> bool {
        let result = self.verify_bot_rdns(DNS_CANARY, &[".google"]);
        !result.get("hostname").is_none_or(Value::is_null)
    }

    fn fresh(&self, key: &CacheKey) -> Option<Evidence> {
        let now = self.clock.monotonic();
        let cache = self.cache.lock().ok()?;
        let (stamp, result) = cache.get(key)?;
        (now - stamp < DNS_CACHE_TTL).then(|| result.clone())
    }

    fn is_cached(&self, ip: &str, suffixes: &[&str]) -> bool {
        self.fresh(&(
            ip.to_string(),
            suffixes.iter().map(|s| s.to_string()).collect(),
        ))
        .is_some()
    }

    fn cached_verification(&self, ip: &str, suffixes: &[&str]) -> Evidence {
        let key = (
            ip.to_string(),
            suffixes.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        );
        if let Some(mut hit) = self.fresh(&key) {
            hit.insert("cached".into(), json!(true));
            return hit;
        }
        let mut result = self.verify_bot_rdns(ip, suffixes);
        result.insert("checked_at".into(), json!(self.clock.wall()));
        if let Ok(mut cache) = self.cache.lock() {
            cache.shift_remove(&key);
            cache.insert(key, (self.clock.monotonic(), result.clone()));
            while cache.len() > CACHE_CAPACITY {
                cache.shift_remove_index(0);
            }
        }
        result.insert("cached".into(), json!(false));
        result
    }

    /// Verify only the explicit bounded page of `rows`; never extrapolate one IP to a family.
    ///
    /// Each row needs `bot` and `ip`. Returns the verification summary with the rows (now
    /// carrying evidence and a `verdict`) under `checks`.
    pub fn verify_rows(&self, rows: &[Evidence], enabled: bool, max_dns: usize) -> Evidence {
        let get = |row: &Evidence, key: &str| {
            row.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let eligible = rows.iter().any(|row| {
            let suffixes = suffixes_for(&get(row, "bot"));
            !suffixes.is_empty()
                && parse_ip(&get(row, "ip")).is_some()
                && !self.is_cached(&get(row, "ip"), suffixes)
        });
        let available = (enabled && max_dns > 0 && eligible).then(|| self.dns_available());
        let mut checks: Vec<Value> = Vec::new();
        let mut requests = 0usize;
        for row in rows {
            let ip = get(row, "ip");
            let suffixes = suffixes_for(&get(row, "bot"));
            let mut result = base("DNS verification was not requested");
            result.insert("cached".into(), json!(false));
            if enabled && suffixes.is_empty() {
                result.insert(
                    "reason".into(),
                    json!("No official PTR suffix is available for this bot"),
                );
            } else if enabled && parse_ip(&ip).is_none() {
                result.insert("reason".into(), json!("Invalid IP address"));
            } else if enabled && self.is_cached(&ip, suffixes) {
                result = self.cached_verification(&ip, suffixes);
            } else if enabled && available == Some(false) {
                result.insert(
                    "reason".into(),
                    json!("Reverse DNS is unavailable; this does not mean the bot is fake"),
                );
            } else if enabled && requests < max_dns {
                result = self.cached_verification(&ip, suffixes);
                let fresh = result.get("cached") == Some(&json!(false));
                requests += usize::from(fresh);
                if fresh {
                    self.clock.sleep(Duration::from_millis(50));
                }
            } else if enabled {
                result.insert(
                    "reason".into(),
                    json!("DNS request budget exhausted; request another bounded page"),
                );
            }
            let verdict = match result.get("verified") {
                Some(Value::Bool(true)) => "verified",
                Some(Value::Bool(false)) => "fake",
                _ => "unchecked",
            };
            let mut merged = row.clone();
            for (k, v) in result {
                merged.insert(k, v);
            }
            merged.insert("verdict".into(), json!(verdict));
            checks.push(Value::Object(merged));
        }
        let checked = enabled
            && checks
                .iter()
                .any(|c| c.get("checked_at").is_some_and(|v| !v.is_null()));
        let mut out = Map::new();
        out.insert("checked".into(), json!(checked));
        out.insert("requested".into(), json!(enabled));
        out.insert("dns_available".into(), json!(available));
        out.insert("checks".into(), Value::Array(checks));
        out.insert("dns_checks".into(), json!(requests));
        out.insert(
            "canary_requests".into(),
            json!(i64::from(available.is_some())),
        );
        out.insert(
            "reason".into(),
            json!("Verification applies only to this page; unchecked rows are not authenticity verdicts"),
        );
        out
    }
}

fn lookup(resolver: &dyn Resolver, address: IpAddr, ip: &str, suffixes: &[String]) -> Evidence {
    let mut result = base("");
    result.remove("reason");
    let hostname = match resolver.reverse(address) {
        Ok(name) => name.to_lowercase().trim_end_matches('.').to_string(),
        Err(failure) => {
            let missing = matches!(failure, DnsFailure::NotFound | DnsFailure::NoData);
            match failure {
                DnsFailure::NotFound | DnsFailure::NoData | DnsFailure::TryAgain => {
                    result.insert(
                        "verified".into(),
                        if missing { json!(false) } else { Value::Null },
                    );
                    result.insert(
                        "reason".into(),
                        json!(if missing {
                            "No PTR record was found"
                        } else {
                            "DNS is unavailable"
                        }),
                    );
                }
                DnsFailure::Other(text) => {
                    result.insert(
                        "reason".into(),
                        json!(format!("DNS is unavailable: {text}")),
                    );
                }
            }
            return result;
        }
    };
    result.insert("hostname".into(), json!(hostname));
    if !suffixes
        .iter()
        .any(|s| hostname.ends_with(&s.to_lowercase()))
    {
        let reason = if hostname.ends_with(".bc.googleusercontent.com") {
            "PTR hostname is a Google Cloud VM (bc.googleusercontent.com), which no official Google crawler uses"
        } else {
            "PTR hostname is outside the official domains"
        };
        result.insert("verified".into(), json!(false));
        result.insert("reason".into(), json!(reason));
        return result;
    }
    match resolver.forward(&hostname, address.is_ipv6()) {
        Err(failure) => {
            let text = match failure {
                DnsFailure::Other(text) => text,
                other => format!("{other:?}"),
            };
            result.insert(
                "reason".into(),
                json!(format!("Forward DNS is unavailable: {text}")),
            );
        }
        Ok(found) => {
            let mut addrs: Vec<String> = found
                .iter()
                .filter_map(|a| parse_ip(a).map(|p| p.to_string()))
                .collect();
            addrs.sort();
            addrs.dedup();
            let matched = addrs.iter().any(|a| *a == address.to_string());
            let reason = if matched {
                "PTR and forward resolution confirm the same IP".to_string()
            } else {
                format!("PTR hostname {hostname} does not resolve back to {ip}")
            };
            result.insert("verified".into(), json!(matched));
            result.insert("forward_match".into(), json!(matched));
            result.insert("forward_addresses".into(), json!(addrs));
            result.insert("reason".into(), json!(reason));
        }
    }
    result
}
