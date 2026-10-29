//! Shared helpers for the log-analysis tests: fixtures, fakes and diffing.

#![allow(dead_code)]

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use spiderhead_log_analyzer::dns::{Clock, DnsFailure, Resolver, SystemClock, Verifier};
use spiderhead_log_analyzer::import::{ProjectManifest, ProjectManifestLoader};
use spiderhead_oracle_diff::{diff, report, Options};

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn load(name: &str) -> Value {
    let text = std::fs::read_to_string(fixtures().join(name)).expect("fixture exists");
    serde_json::from_str(&text).expect("fixture is JSON")
}

pub fn log_path(name: &str) -> String {
    fixtures()
        .join("logs")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// Fail with a readable report unless `actual` equals `expected` outside `ignore`.
pub fn assert_same(label: &str, expected: &Value, actual: &Value, ignore: &[&str]) {
    let options = Options {
        ignore_paths: ignore.iter().map(|s| s.to_string()).collect(),
        ..Options::default()
    };
    let divergences = diff(expected, actual, &options);
    assert!(
        divergences.is_empty(),
        "{label}\n{}",
        report(&divergences, 12)
    );
}

/// Scripted name resolution mirroring the generator's patched socket functions.
#[derive(Default)]
pub struct FakeResolver {
    pub ptr: HashMap<String, String>,
    pub forward: HashMap<String, Vec<String>>,
    pub failures: HashMap<String, DnsFailure>,
    pub blocked: bool,
    pub break_forward: bool,
    pub calls: Mutex<Vec<String>>,
}

impl FakeResolver {
    /// The table the generator's offline DNS answers from.
    pub fn standard() -> Self {
        let mut r = Self::default();
        for (ip, host) in [
            ("8.8.8.8", "dns.google"),
            ("66.249.66.1", "crawl-66-249-66-1.googlebot.com"),
            ("66.249.66.2", "crawl-66-249-66-2.googlebot.com"),
            ("66.249.66.3", "evilgooglebot.com"),
            ("66.249.66.17", "rate-limited-proxy-66-249-66-17.google.com"),
            ("157.55.39.9", "msnbot-157-55-39-9.search.msn.com"),
            ("5.255.253.1", "5-255-253-1.spider.yandex.com"),
            ("192.0.2.1", "crawl.googlebot.com"),
            ("192.0.2.2", "crawl.googlebot.com"),
            ("2001:db8::1", "crawl.googlebot.com"),
        ] {
            r.ptr.insert(ip.into(), host.into());
        }
        for (host, addrs) in [
            ("crawl-66-249-66-1.googlebot.com", vec!["66.249.66.1"]),
            ("crawl-66-249-66-2.googlebot.com", vec!["9.9.9.9"]),
            ("msnbot-157-55-39-9.search.msn.com", vec!["157.55.39.9"]),
            ("5-255-253-1.spider.yandex.com", vec!["5.255.253.1"]),
            ("crawl.googlebot.com", vec!["192.0.2.1", "2001:db8::1"]),
        ] {
            r.forward
                .insert(host.into(), addrs.into_iter().map(String::from).collect());
        }
        r
    }

    pub fn lookups(&self) -> Vec<String> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }
}

impl Resolver for FakeResolver {
    fn reverse(&self, ip: IpAddr) -> Result<String, DnsFailure> {
        let text = ip.to_string();
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(text.clone());
        }
        if self.blocked {
            return Err(DnsFailure::TryAgain);
        }
        if let Some(failure) = self.failures.get(&text) {
            return Err(failure.clone());
        }
        if let Some(host) = self.ptr.get(&text) {
            return Ok(host.clone());
        }
        if text.ends_with(".7") {
            return Err(DnsFailure::TryAgain);
        }
        Err(DnsFailure::NotFound)
    }

    fn forward(&self, host: &str, ipv6: bool) -> Result<Vec<String>, DnsFailure> {
        if self.break_forward {
            return Err(DnsFailure::Other("Network is unreachable".into()));
        }
        match self.forward.get(host) {
            Some(addrs) => Ok(addrs
                .iter()
                .filter(|a| a.contains(':') == ipv6)
                .cloned()
                .collect()),
            None => Err(DnsFailure::Other(
                "[Errno 8] nodename nor servname provided".into(),
            )),
        }
    }
}

/// Real time, no pauses.
pub struct QuickClock;

impl Clock for QuickClock {
    fn monotonic(&self) -> f64 {
        SystemClock.monotonic()
    }
    fn wall(&self) -> f64 {
        SystemClock.wall()
    }
    fn sleep(&self, _duration: Duration) {}
}

pub fn verifier(resolver: Arc<FakeResolver>) -> Verifier {
    Verifier::new(resolver, Arc::new(QuickClock))
}

/// Loader for directories holding a `project.json` with a `target`.
pub struct DirLoader;

impl ProjectManifestLoader for DirLoader {
    fn load(&self, project: &str) -> Result<ProjectManifest, String> {
        let root = PathBuf::from(project);
        let text = std::fs::read_to_string(root.join("project.json"))
            .map_err(|_| "not a project: project.json is missing".to_string())?;
        let doc: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        Ok(ProjectManifest {
            root,
            project_uuid: "00000000-0000-0000-0000-000000000000".into(),
            target: format!(
                "{}/",
                doc["target"].as_str().unwrap_or("").trim_end_matches('/')
            ),
        })
    }
}

pub fn make_project(root: &Path, target: &str) {
    std::fs::create_dir_all(root).expect("project dir");
    std::fs::write(
        root.join("project.json"),
        format!("{{\"target\": \"{target}\"}}"),
    )
    .expect("manifest");
}
