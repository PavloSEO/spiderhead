//! Oracle-diff test for the PageSpeed client against `pagespeed_oracle.json`.

mod common;

use common::{assert_scenario, load_fixture, scenario_host, StubTransport};
use serde_json::Value;
use sha2::{Digest, Sha256};
use spiderhead_sources::pagespeed;

fn seed_cache(dir: &std::path::Path, scenario: &Value) {
    for seed in scenario
        .get("cache_seed")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let url = seed.get("url").and_then(Value::as_str).unwrap_or_default();
        let strategy = seed
            .get("strategy")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let digest = Sha256::digest(format!("{url}\0{strategy}").as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let path = dir.join(format!("psi-v5-{hex}.json"));
        let body = match seed.get("value") {
            Some(Value::Object(m)) if m.contains_key("$b64") => m
                .get("$b64")
                .and_then(Value::as_str)
                .map(b64_dec)
                .unwrap_or_default(),
            Some(v) => serde_json::to_string(v).unwrap_or_default().into_bytes(),
            None => Vec::new(),
        };
        std::fs::write(path, body).unwrap();
    }
}

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

#[tokio::test(flavor = "current_thread")]
async fn pagespeed_oracle_parity() {
    for scenario in load_fixture("pagespeed_oracle.json") {
        let responses = scenario
            .get("responses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let transport = StubTransport::new(&responses);
        let (host, root) = scenario_host(&scenario);
        let args = scenario.get("args").cloned().unwrap_or_default();
        let urls = args
            .get("urls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let cache_dir = if scenario
            .get("use_cache_dir")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let dir = std::env::temp_dir().join(format!(
                "psi-cache-{}-{}",
                std::process::id(),
                scenario.get("name").and_then(Value::as_str).unwrap_or("x")
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            seed_cache(&dir, &scenario);
            Some(dir)
        } else {
            None
        };
        let actual = pagespeed::sample(
            &transport,
            &host,
            &root,
            &urls,
            args.get("desktop")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            args.get("api_key").and_then(Value::as_str),
            cache_dir.as_deref(),
        )
        .await
        .map_err(|e| (e.python_class().to_owned(), e.to_string()));
        assert_scenario(&scenario, &transport, &[], actual);
        if let Some(d) = cache_dir {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
