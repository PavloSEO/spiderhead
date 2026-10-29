//! Oracle-diff test for the CrUX client against `crux_oracle.json`.

mod common;

use common::{assert_scenario, load_fixture, scenario_host, StubTransport};
use serde_json::Value;
use spiderhead_sources::crux;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

fn b64_dec(text: &str) -> Vec<u8> {
    fn v(b: u8) -> u8 {
        match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b + 26 - b'a',
            b'0'..=b'9' => b + 52 - b'0',
            b'+' => 62,
            _ => 63,
        }
    }
    let b: Vec<u8> = text.bytes().filter(|b| *b != b'=').collect();
    let mut out = Vec::new();
    for c in b.chunks(4) {
        let n = c.iter().fold(0u32, |a, b| (a << 6) | v(*b) as u32) << (6 * (4 - c.len()));
        for i in 0..c.len().saturating_sub(1) {
            out.push((n >> (16 - i * 8)) as u8);
        }
    }
    out
}

/// Mirror the reference suite's `cache_seed` handling for the `file`-named entries crux uses.
fn seed_cache(dir: &Path, scenario: &Value) {
    for seed in scenario
        .get("cache_seed")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let file = seed.get("file").and_then(Value::as_str).unwrap_or_default();
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap_or(dir)).unwrap();
        if seed.get("as_dir").and_then(Value::as_bool) == Some(true) {
            std::fs::create_dir_all(&path).unwrap();
        } else if let Some(target) = seed.get("symlink_to").and_then(Value::as_str) {
            let _ = &target;
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &path).unwrap();
            #[cfg(not(unix))]
            panic!("symlink cache seeds need a unix filesystem");
        } else {
            let body = match seed.get("value") {
                Some(Value::Object(m)) if m.contains_key("$b64") => m
                    .get("$b64")
                    .and_then(Value::as_str)
                    .map(b64_dec)
                    .unwrap_or_default(),
                Some(v) => spiderhead_sources::pyjson::dumps_opts(v, true, true).into_bytes(),
                None => Vec::new(),
            };
            let body = if let Some(n) = seed.get("zeros").and_then(Value::as_u64) {
                vec![0u8; n as usize]
            } else if let Some(text) = seed.get("text").and_then(Value::as_str) {
                text.as_bytes().to_vec()
            } else {
                body
            };
            std::fs::write(&path, body).unwrap();
        }
        if seed.get("symlink_to").is_none() {
            if let Some(age) = seed.get("mtime_age_seconds").and_then(Value::as_f64) {
                let stamp = SystemTime::now() - Duration::from_secs_f64(age);
                let file = std::fs::File::open(&path).unwrap();
                file.set_modified(stamp).unwrap();
            }
        }
    }
}

/// A fresh cache dir per scenario (or a symlink to one for `cache_dir_symlink`).
fn cache_dir(scenario: &Value) -> Option<PathBuf> {
    let wants = scenario
        .get("use_cache_dir")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || scenario
            .get("cache_dir_symlink")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if !wants {
        return None;
    }
    let dir = std::env::temp_dir().join(format!(
        "crux-cache-{}-{}",
        std::process::id(),
        scenario.get("name").and_then(Value::as_str).unwrap_or("x")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(dir.with_extension("link"));
    std::fs::create_dir_all(&dir).unwrap();
    seed_cache(&dir, scenario);
    if scenario
        .get("cache_dir_symlink")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let link = dir.with_extension("link");
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        return Some(link);
    }
    Some(dir)
}

fn arg<'a>(args: &'a Value, name: &str) -> Option<&'a Value> {
    args.get(name).filter(|v| !v.is_null())
}

/// The oracle never stores the api key: verify the written cache file after a miss.
fn assert_key_not_cached(cache: &Option<PathBuf>, api_key: Option<&str>) {
    let (Some(dir), Some(key)) = (cache, api_key) else {
        return;
    };
    if key.len() < 4 {
        return;
    }
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_file() {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                !text.contains(key),
                "credential leaked into {}",
                path.display()
            );
        }
    }
}

// + the reference suite — recorded stub-server scenarios.
#[tokio::test(flavor = "current_thread")]
async fn crux_oracle_parity() {
    for scenario in load_fixture("crux_oracle.json") {
        let name = scenario.get("name").and_then(Value::as_str).unwrap_or("?");
        let responses = scenario
            .get("responses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let transport = StubTransport::new(&responses);
        let (host, root) = scenario_host(&scenario);
        let args = scenario.get("args").cloned().unwrap_or_default();
        let now = scenario.get("frozen_now").and_then(Value::as_str);
        let call = scenario
            .get("call")
            .and_then(Value::as_str)
            .unwrap_or("query");
        let dir = cache_dir(&scenario);
        let actual = match call {
            "query" => {
                crux::query(
                    &host,
                    &root,
                    &transport,
                    &crux::Query {
                        url: arg(&args, "url").and_then(Value::as_str),
                        origin: arg(&args, "origin").and_then(Value::as_str),
                        form_factor: arg(&args, "form_factor").and_then(Value::as_str),
                        metrics: arg(&args, "metrics"),
                        api_key: arg(&args, "api_key").and_then(Value::as_str),
                        now,
                    },
                )
                .await
            }
            "sample_urls" => {
                crux::sample(
                    &host,
                    &root,
                    &transport,
                    &crux::Sample {
                        urls: args.get("urls").unwrap_or(&Value::Null),
                        form_factor: arg(&args, "form_factor").and_then(Value::as_str),
                        max_samples: arg(&args, "max_samples"),
                        cache_dir: dir.as_deref(),
                        cache_max_age_hours: arg(&args, "cache_max_age_hours"),
                        api_key: arg(&args, "api_key").and_then(Value::as_str),
                        now,
                    },
                )
                .await
            }
            "history" => {
                crux::history(
                    &host,
                    &root,
                    &transport,
                    &crux::History {
                        url: arg(&args, "url").and_then(Value::as_str),
                        origin: arg(&args, "origin").and_then(Value::as_str),
                        form_factor: arg(&args, "form_factor").and_then(Value::as_str),
                        metrics: arg(&args, "metrics"),
                        api_key: arg(&args, "api_key").and_then(Value::as_str),
                    },
                )
                .await
            }
            other => panic!("{name}: unknown call {other}"),
        }
        .map_err(|e| (e.python_class().to_owned(), e.to_string()));
        assert_scenario(&scenario, &transport, &[], actual);
        assert_key_not_cached(&dir, arg(&args, "api_key").and_then(Value::as_str));
        if let Some(d) = dir {
            let _ = std::fs::remove_dir_all(&d);
            let _ = std::fs::remove_dir_all(d.with_extension("link"));
        }
    }
}
