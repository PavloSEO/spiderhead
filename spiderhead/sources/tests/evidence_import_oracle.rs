//! Replay `evidence_import` ops against the reference baseline fixture.

use serde_json::{json, Value};
use spiderhead_sources::cloudflare::CfError;
use spiderhead_sources::evidence_import as ei;

fn outcome(result: Result<Value, CfError>) -> Value {
    match result {
        Ok(value) => json!({"result": value}),
        Err(error) => {
            json!({"error": {"class": error.python_class(), "message": error.to_string()}})
        }
    }
}

fn scrub(value: &Value, base: &str) -> Value {
    let text = serde_json::to_string(value).expect("serializes");
    serde_json::from_str(&text.replace(base, "$BASE")).expect("serializes")
}

#[test]
fn oracle_ops_match_python() {
    let text = include_str!("fixtures/evidence_import_oracle.json");
    let fixture: Value = serde_json::from_str(text).expect("fixture");
    let base = std::env::temp_dir()
        .join(format!("sh-evi-{}", std::process::id()))
        .to_string_lossy()
        .into_owned();
    std::fs::create_dir_all(&base).expect("base");
    let real_base = std::fs::canonicalize(&base)
        .unwrap_or_else(|_| base.clone().into())
        .to_string_lossy()
        .into_owned();
    // File ops: (name, bytes, mode)
    let files: [(&str, &str, u32); 7] = [
        ("prov.json", "PLACEHOLDER", 0o600),
        (
            "data.csv",
            "url,clicks\nhttps://ex.com/a,3\nhttps://ex.com/b,\n",
            0o644,
        ),
        ("data.jsonl", "{\"a\":1}\n{\"a\":2}\n", 0o644),
        ("data.txt", "garbage", 0o644),
        ("bad.json", "{nope", 0o644),
        (
            "arr.json",
            "[{\"url\": \"https://ex.com/a\", \"clicks\": \"7\"}]",
            0o644,
        ),
        ("prov644.json", "PLACEHOLDER", 0o644),
    ];
    let prov_doc = json!({
        "evidence": {
            "format": "spiderhead.provider-evidence.v1", "provider": "gsc",
            "operation": "query", "status": "complete",
            "retrieved_at": "2026-01-31T00:00:00Z", "sampling": true,
            "reporting_identity": "site-1", "site_origin": "https://ex.com",
            "period": {"start_date": "2026-01-01", "end_date": "2026-01-31"},
            "artifact_reference": {"sha256": "a".repeat(64)},
        },
        "result": {"rows": [{"keys": ["https://ex.com/a"], "clicks": 3}],
                   "dimensions": ["page"], "metrics": ["clicks"]},
    });
    use spiderhead_core::filesystem::set_permissions_mode;
    for (name, content, mode) in &files {
        let path = std::path::Path::new(&base).join(name);
        let body = if *content == "PLACEHOLDER" {
            spiderhead_sources::pyjson::dumps_opts(&prov_doc, false, true).into_bytes()
        } else {
            content.as_bytes().to_vec()
        };
        std::fs::write(&path, body).expect("write");
        set_permissions_mode(&path, *mode).expect("mode");
    }
    let mut mismatches = 0usize;
    for op in fixture["ops"].as_array().expect("ops") {
        let name = op["name"].as_str().expect("name");
        let spec = &op["spec"];
        let want = &op["outcome"];
        let got: Value = if name.starts_with("manifest-") {
            outcome(ei::validate_manifest(&spec["manifest"]))
        } else if name.starts_with("inline-") {
            let rows = spec["rows"].as_array().cloned().unwrap_or_default();
            let manifest = spec.get("manifest").filter(|m| !m.is_null());
            outcome(ei::normalize_inline(&rows, manifest))
        } else if name == "resolve-envelope" {
            let loaded = spec["loaded"].clone();
            outcome(
                ei::resolve_mapping(&json!({"format": ei::MAPPING_FORMAT}), &loaded)
                    .map(|(r, p)| json!([r, p])),
            )
        } else if name == "public-provenance" {
            let prov = json!({
                "fields": {
                    "reporting_identity": {"value": "site-1", "origin": "envelope"},
                    "site_origin": {"value": "https://ex.com", "origin": "declared"},
                    "provider": {"value": "gsc", "origin": "envelope"},
                },
                "file": {"name": "f.csv", "sha256": "x", "kind": "csv"},
                "artifact_reference": {"sha256": "b".repeat(64)},
            });
            json!({"result": ei::public_provenance(&prov)})
        } else {
            // file-based ops
            let rel = spec["path"].as_str().expect("path");
            let file = if rel == "prov.json" && (name == "provider-evidence-not-private") {
                std::path::Path::new(&base).join("prov644.json")
            } else {
                std::path::Path::new(&base).join(rel)
            };
            let manifest = spec.get("manifest").filter(|m| !m.is_null());
            outcome(ei::normalize_file(
                file.to_str().unwrap_or_default(),
                manifest,
                None,
                None,
            ))
        };
        let got = scrub(&scrub(&got, &real_base), &base);
        if &got != want {
            mismatches += 1;
            eprintln!(
                "=== {name}\nwant: {}\ngot : {}",
                serde_json::to_string_pretty(want).unwrap_or_default(),
                serde_json::to_string_pretty(&got).unwrap_or_default()
            );
        }
    }
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(mismatches, 0, "{mismatches} oracle mismatches");
}
