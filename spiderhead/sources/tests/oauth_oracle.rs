//! Oracle-diff test for the OAuth grant lifecycle against `oauth_oracle.json`.
//!
//! Each scenario runs `manage_grant` over a seeded config root with frozen `datetime.now`
//! and deterministic `secrets.token_urlsafe` output; the generator also snapshots the config
//! root afterwards, so grant/flow file bytes and modes are diffed, not just the envelope.

mod common;

use common::{assert_scenario, load_fixture, StubTransport};
use serde_json::{json, Value};
use spiderhead_core::filesystem::StatCompat;
use spiderhead_core::host::FakeHost;
use spiderhead_sources::oauth::{self, Manage};
use spiderhead_sources::oauth_flow::{FlowError, Tokens};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The the reference suite `_tokens()` sequence: `deterministic-<nbytes>-<counter>-abcdef`.
struct FixedTokens(u32);

impl Tokens for FixedTokens {
    fn urlsafe(&mut self, nbytes: usize) -> Result<String, FlowError> {
        self.0 += 1;
        Ok(format!("deterministic-{nbytes}-{:04}-abcdef", self.0))
    }
}

fn write_private(path: &Path, text: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        std::io::Write::write_all(&mut file, text.as_bytes()).unwrap();
    }
    #[cfg(not(unix))]
    std::fs::write(path, text).unwrap();
}

/// Build the scenario's config root and the external-file directory for `grant_file` args.
fn seed(scenario: &Value) -> (PathBuf, PathBuf) {
    let tag = scenario.get("name").and_then(Value::as_str).unwrap_or("x");
    let root = std::env::temp_dir().join(format!("oauth-root-{}-{tag}", std::process::id()));
    let external = std::env::temp_dir().join(format!("oauth-ext-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&external);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&external).unwrap();
    if let Some(map) = scenario.get("seed").and_then(Value::as_object) {
        for (rel, content) in map {
            let target = root.join(rel);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            #[cfg(unix)]
            std::fs::set_permissions(
                target.parent().unwrap(),
                std::os::unix::fs::PermissionsExt::from_mode(0o700),
            )
            .unwrap();
            if let Some(link) = content.as_str().and_then(|s| s.strip_prefix("$symlink:")) {
                let _ = &link;
                #[cfg(unix)]
                std::os::unix::fs::symlink(link, &target).unwrap();
            } else {
                let text = match content {
                    Value::String(s) => s.clone(),
                    other => spiderhead_sources::pyjson::dumps(other),
                };
                write_private(&target, &text);
            }
        }
    }
    (root, external)
}

/// Resolve `$ext:` argument markers into real paths under the external dir.
fn resolve_ext(external: &Path, args: &Value) -> Value {
    let mut args = args.clone();
    if let Some(map) = args.as_object_mut() {
        for value in map.values_mut() {
            let Some(text) = value.as_str().and_then(|s| s.strip_prefix("$ext:")) else {
                continue;
            };
            let target = external.join("grant-file.json");
            if let Some(payload) = text.strip_prefix("symlink=") {
                let real = external.join("grant-file-real.json");
                write_private(&real, payload);
                #[cfg(unix)]
                std::os::unix::fs::symlink(&real, &target).unwrap();
            } else {
                write_private(&target, text);
            }
            *value = json!(target.to_string_lossy());
        }
    }
    args
}

/// Mirror the generator's post-call `snapshot(root)`.
fn snapshot(root: &Path) -> Value {
    let mut files = serde_json::Map::new();
    let mut stack = vec![root.to_path_buf()];
    let mut entries: Vec<PathBuf> = Vec::new();
    while let Some(dir) = stack.pop() {
        if let Ok(read) = std::fs::read_dir(&dir) {
            for entry in read.flatten() {
                let path = entry.path();
                entries.push(path.clone());
                if path.is_dir() && !path.is_symlink() {
                    stack.push(path);
                }
            }
        }
    }
    entries.sort();
    for path in entries {
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&path).unwrap_or_default();
            files.insert(rel, json!({"symlink": target.to_string_lossy()}));
        } else if meta.is_file() {
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            files.insert(
                rel,
                json!({"content": content, "mode": format!("0o{:o}", meta.mode() & 0o7777)}),
            );
        } else if meta.is_dir() {
            files.insert(
                format!("{rel}/"),
                json!({"mode": format!("0o{:o}", meta.mode() & 0o7777)}),
            );
        }
    }
    Value::Object(files)
}

// the reference suite — recorded with a seeded config root.
#[tokio::test(flavor = "current_thread")]
async fn oauth_oracle_parity() {
    // The generator's token stub is a class-level counter shared by all scenarios.
    let mut tokens = FixedTokens(0);
    for scenario in load_fixture("oauth_oracle.json") {
        let name = scenario.get("name").and_then(Value::as_str).unwrap_or("?");
        let (root, external) = seed(&scenario);
        let args = resolve_ext(
            &external,
            &scenario.get("args").cloned().unwrap_or_default(),
        );
        let responses = scenario
            .get("responses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let transport = StubTransport::new(&responses);
        let mut env = BTreeMap::new();
        if let Some(vars) = scenario.get("env").and_then(Value::as_object) {
            for (key, value) in vars {
                env.insert(key.clone(), value.as_str().unwrap_or_default().to_owned());
            }
        }
        let host = FakeHost {
            env,
            ..Default::default()
        };
        let now = scenario.get("frozen_now_micros").and_then(Value::as_i64);
        let actual = oauth::manage_grant_with(
            &host,
            &root,
            scenario
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("gsc"),
            &Manage {
                action: args
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or("status"),
                grant_file: args.get("grant_file").and_then(Value::as_str),
                confirm: args
                    .get("confirm")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                state: args.get("state").and_then(Value::as_str),
                code: args.get("code").and_then(Value::as_str),
                now,
            },
            &transport,
            &mut tokens,
            None,
        )
        .await
        .map_err(|e| (e.python_class().to_owned(), e.to_string()));
        assert_scenario(&scenario, &transport, &[], actual);
        // The post-call config-root snapshot is part of the oracle.
        let want_files = scenario
            .get("expect")
            .and_then(|e| e.get("files"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let got_files = snapshot(&root);
        let canonical = |v: &Value| -> Value {
            match v.as_object().and_then(|m| m.get("content")) {
                Some(Value::String(text)) => {
                    let mut out = v.clone();
                    let parsed = serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
                    out.as_object_mut()
                        .unwrap()
                        .insert("content".to_owned(), parsed);
                    out
                }
                _ => v.clone(),
            }
        };
        let norm = |v: &Value| -> Value {
            json!(v
                .as_object()
                .map(|m| m
                    .iter()
                    .map(|(k, f)| (k.clone(), canonical(f)))
                    .collect::<BTreeMap<_, _>>())
                .unwrap_or_default())
        };
        assert_eq!(
            norm(&got_files),
            norm(&want_files),
            "{name}: config-root file snapshot differs"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&external);
    }
}
