//! Replay `evidence_join` + `evidence_join_store` ops against the reference
//! oracle fixture (the reference suite).

use serde_json::{json, Value};
use spiderhead_sources::cloudflare::CfError;
use spiderhead_sources::{evidence_join, evidence_join_store};
use std::path::PathBuf;

fn fixture() -> Value {
    let text = include_str!("fixtures/evidence_join_oracle.json");
    serde_json::from_str(text).expect("fixture parses")
}

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
    let fixture = fixture();
    let ops = fixture["ops"].as_array().expect("ops");
    let base = std::env::temp_dir()
        .join(format!("sh-evj-{}", std::process::id()))
        .to_string_lossy()
        .into_owned();
    let store_dir = PathBuf::from(&base).join("store");
    std::fs::create_dir_all(&store_dir).expect("store dir");
    let real_base = std::fs::canonicalize(&base)
        .unwrap_or_else(|_| PathBuf::from(&base))
        .to_string_lossy()
        .into_owned();
    let mut mismatches = 0usize;
    for op in ops {
        let name = op["name"].as_str().expect("name");
        let spec = &op["spec"];
        let want = &op["outcome"];
        let got: Value = if let Some(stripped) = name.strip_prefix("store-write").or_else(|| {
            [
                "store-exists",
                "store-bad-doc",
                "store-bad-policy",
                "store-nonbool-policy",
                "store-bad-row",
                "store-nonobject-row",
                "store-no-uuid",
                "store-relaxed",
            ]
            .iter()
            .find(|p| name == **p)
            .map(|_| name)
        }) {
            let _ = stripped;
            let uuid = spec["uuid"].as_str().unwrap_or_default().to_owned();
            let pages: Vec<(i64, String)> = spec["pages"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .map(|r| {
                            (
                                r[0].as_i64().expect("ordinal"),
                                r[1].as_str().unwrap_or_default().to_owned(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let out = store_dir
                .join(spec["out"].as_str().expect("out"))
                .to_string_lossy()
                .into_owned();
            let doc = spec.get("document").cloned().unwrap_or(Value::Null);
            let policy = spec.get("policy").cloned().unwrap_or(Value::Null);
            let scan_rows = move |_: &str| -> Result<(String, Vec<(i64, String)>), CfError> {
                Ok((uuid.clone(), pages.clone()))
            };
            let result = evidence_join_store::write(
                &scan_rows,
                "scan.db",
                &doc,
                &out,
                if policy.is_null() {
                    None
                } else {
                    Some(&policy)
                },
            );
            outcome(result)
        } else {
            match name {
                "store-is" | "store-is-missing" => {
                    let path = store_dir.join(spec["out"].as_str().expect("out"));
                    json!({"result": evidence_join_store::is_store(path.to_str().unwrap_or_default())})
                }
                "store-open-missing" => {
                    let path = store_dir.join(spec["out"].as_str().expect("out"));
                    let result = evidence_join_store::open_store(path.to_str().unwrap_or_default())
                        .map(|store| store.metadata);
                    outcome(result)
                }
                "store-obs" | "store-matches" | "store-pages" | "store-pages-matched" => {
                    let path = store_dir.join(spec["out"].as_str().expect("out"));
                    let result = evidence_join_store::open_store(path.to_str().unwrap_or_default());
                    match result {
                        Err(error) => outcome(Err(error)),
                        Ok(store) => {
                            let iter_result = match name {
                                "store-obs" => evidence_join_store::iter_observations(&store.path),
                                "store-matches" => evidence_join_store::iter_matches(&store.path),
                                _ => evidence_join_store::iter_pages(
                                    &store.path,
                                    spec.get("population").and_then(Value::as_str),
                                ),
                            };
                            outcome(iter_result.map(Value::Array))
                        }
                    }
                }
                "public-redacts" => {
                    let result = evidence_join::evidence_compatibility(
                        &spec["left"],
                        &spec["right"],
                        spec.get("policy").filter(|p| !p.is_null()),
                    );
                    outcome(result.map(|r| evidence_join::public_compatibility(&r)))
                }
                _ if name.starts_with("compat-") => {
                    let result = evidence_join::evidence_compatibility(
                        &spec["left"],
                        &spec["right"],
                        spec.get("policy").filter(|p| !p.is_null()),
                    );
                    outcome(result)
                }
                _ if name.starts_with("join-") => {
                    let pages = spec["pages"].as_array().cloned().unwrap_or_default();
                    let result = evidence_join::join_evidence(
                        &pages,
                        &spec["document"],
                        spec.get("url_policy").filter(|p| !p.is_null()),
                        spec.get("crawl").filter(|c| !c.is_null()),
                    );
                    outcome(result)
                }
                _ => panic!("unknown op {name}"),
            }
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
    assert_eq!(mismatches, 0, "{} oracle mismatches", mismatches);
}
