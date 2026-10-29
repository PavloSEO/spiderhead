//! Release checks and checksummed downloads: the persisted check state, the
//! consent gates and the bounded feed/asset reads. Every network read goes
//! through the injected [`ReconClient`], so the guards (consent, tag and asset
//! binding, size and checksum limits) are exercised without a network.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use spiderhead_core::timeutil::{stamp, Format};
use spiderhead_core::update::{
    asset_of, normalize_releases, state_shape_is_valid, version_of, Asset, Release,
    AUTO_CHECK_SECONDS, DEFAULT_MAX_DOWNLOAD_BYTES, MAX_ASSETS, MAX_CHECKSUM_BYTES,
    MAX_DOWNLOAD_BYTES, MAX_FEED_BYTES, MAX_RELEASES, MAX_STATE_BYTES, REPOSITORY, TOOLKIT_VERSION,
    UPDATE_FORMAT,
};
use spiderhead_recon::net_client::{http_client, ReconClient};

use crate::error::ToolError;

use super::{kwargs, opt, opt_bool, opt_int};

fn failed(message: &str) -> ToolError {
    ToolError::Failed(message.to_owned())
}

// ── Persisted state ──────────────────────────────────────────────────────

/// The state file: `SEOHEAD_UPDATE_STATE_PATH`, else the per-user application directory.
fn state_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SEOHEAD_UPDATE_STATE_PATH").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let base = if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("state"))
    };
    // The legacy directory name is kept only while it is the one that exists.
    let app = if base.join("seohead").exists() && !base.join("spiderhead").exists() {
        "seohead"
    } else {
        "spiderhead"
    };
    base.join(app).join("update-state.json")
}

/// The persisted check, or `None` when absent, oversized, a link or malformed.
fn read_state() -> Option<Value> {
    let path = state_path();
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > MAX_STATE_BYTES as u64 {
        return None;
    }
    let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    state_shape_is_valid(&value).then_some(value)
}

/// Persist the check; the answer is the reason it could not be kept.
fn write_state(value: &Value) -> Option<String> {
    let path = state_path();
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Some("update state path must not be a symbolic link".to_owned());
    }
    let content = format!("{}\n", serde_json::to_string(value).unwrap_or_default());
    if content.len() > MAX_STATE_BYTES {
        return Some("update state exceeds its byte limit".to_owned());
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".update-state-{}", std::process::id()));
        std::fs::write(&temporary, content.as_bytes())?;
        std::fs::rename(&temporary, &path)
    };
    write()
        .err()
        .map(|error| format!("could not persist update state ({:?})", error.kind()))
}

// ── Release documents ────────────────────────────────────────────────────

fn asset_json(asset: &Asset) -> Value {
    json!({
        "name": asset.name,
        "url": asset.url,
        "size": asset.size,
        "sha256": asset.sha256,
    })
}

fn release_json(release: &Release) -> Value {
    json!({
        "tag": release.tag,
        "version": release.version,
        "url": release.url,
        "published_at": release.published_at,
        "changelog": release.changelog,
        "assets": release.assets.iter().map(asset_json).collect::<Vec<_>>(),
    })
}

fn error_state(code: &str, message: &str) -> Value {
    json!({"state": "error", "error": {"code": code, "message": message}})
}

/// `update_check`: read the public release feed only after explicit consent.
pub fn update_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(consent) = opt(&args, "consent") else {
        return Err(super::missing_option(
            "update-check",
            "consent",
            &["consent", "automatic", "max_releases"],
        ));
    };
    let (Value::Bool(consent), automatic) = (consent, opt(&args, "automatic")) else {
        return Err(failed("consent and automatic must be booleans"));
    };
    if automatic.is_some_and(|a| !a.is_boolean()) {
        return Err(failed("consent and automatic must be booleans"));
    }
    let automatic = opt_bool(&args, "automatic").unwrap_or(false);
    let max_releases = match opt(&args, "max_releases") {
        None => 5,
        Some(value) => match value.as_i64() {
            Some(n) if (1..=MAX_RELEASES as i64).contains(&n) => n as usize,
            _ => {
                return Err(ToolError::Failed(format!(
                    "max_releases must be 1..{MAX_RELEASES}"
                )))
            }
        },
    };
    let client = if *consent {
        Some(
            http_client(10.0)
                .map_err(|e| ToolError::Failed(e.to_string()))?
                .0,
        )
    } else {
        None
    };
    check_updates(
        *consent,
        automatic,
        max_releases,
        client.as_ref().map(|c| c as &dyn ReconClient),
    )
}

/// The consent gate, the daily reuse and the feed read behind `update_check`.
pub fn check_updates(
    consent: bool,
    automatic: bool,
    max_releases: usize,
    client: Option<&dyn ReconClient>,
) -> Result<Value, ToolError> {
    let cached = read_state();
    if !consent {
        return Ok(json!({
            "ok": true,
            "format": UPDATE_FORMAT,
            "state": "consent_required",
            "checked_at": cached.as_ref().map(|c| c["checked_at"].clone()),
            "reason": "release checks require explicit consent",
            "result": cached.as_ref().map(|c| c["result"].clone()),
        }));
    }
    if let (true, Some(cached)) = (automatic, &cached) {
        let age = cached["checked_at"]
            .as_str()
            .and_then(age_seconds)
            .unwrap_or(AUTO_CHECK_SECONDS as f64 + 1.0);
        if (0.0..AUTO_CHECK_SECONDS as f64).contains(&age) {
            return Ok(json!({
                "ok": true,
                "format": UPDATE_FORMAT,
                "state": "not_due",
                "checked_at": cached["checked_at"],
                "result": cached["result"],
                "reason": "the consented daily check was completed within the last 24 hours",
            }));
        }
    }
    let checked_at = stamp(Format::ZuluSeconds);
    let result = match client {
        Some(client) => read_feed(client, max_releases),
        None => error_state(
            "release_feed_unreachable",
            "release feed could not be read (NoClient)",
        ),
    };
    let persisted = write_state(&json!({
        "format": UPDATE_FORMAT,
        "checked_at": checked_at,
        "result": result,
    }));
    Ok(json!({
        "ok": result["state"] != "error",
        "format": UPDATE_FORMAT,
        "state": result["state"],
        "checked_at": checked_at,
        "persisted": persisted.is_none(),
        "persistence_reason": persisted,
        "result": result,
        "reason": result.get("reason").cloned().unwrap_or(Value::Null),
    }))
}

/// Seconds since an RFC 3339 `Z` timestamp, when it parses.
fn age_seconds(text: &str) -> Option<f64> {
    let body = text.strip_suffix('Z')?;
    let (date, time) = body.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = time.split('.').next()?;
    let mut t = time.split(':').map(|p| p.parse::<i64>().ok());
    let (h, mi, s) = (t.next()??, t.next()??, t.next()??);
    // Days from civil date (proleptic Gregorian).
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let then = days * 86_400 + h * 3600 + mi * 60 + s;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some((now - then) as f64)
}

fn read_feed(client: &dyn ReconClient, max_releases: usize) -> Value {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases");
    let headers = [
        (
            "Accept".to_owned(),
            "application/vnd.github+json".to_owned(),
        ),
        ("User-Agent".to_owned(), "SpiderHead".to_owned()),
    ];
    let params = [("per_page".to_owned(), max_releases.to_string())];
    let response = match client.get(&url, &headers, &params) {
        Ok(response) => response,
        Err(_) => {
            return error_state(
                "release_feed_unreachable",
                "release feed could not be read (TransportError)",
            )
        }
    };
    if response.status_code != 200 {
        return error_state(
            "release_feed_http_error",
            &format!("release feed returned HTTP {}", response.status_code),
        );
    }
    if response.content.len() > MAX_FEED_BYTES {
        return error_state(
            "release_feed_too_large",
            "release feed exceeded its response byte limit",
        );
    }
    let releases = serde_json::from_slice::<Value>(&response.content)
        .ok()
        .and_then(|payload| normalize_releases(&payload, max_releases).ok());
    let Some(releases) = releases else {
        return error_state(
            "invalid_release_feed",
            "release feed was not valid bounded JSON",
        );
    };
    let Some(latest) = releases.first() else {
        return json!({
            "state": "unavailable",
            "reason": "the repository has no usable stable release metadata",
            "latest": null,
            "releases": [],
        });
    };
    let current = version_of(TOOLKIT_VERSION);
    let released = version_of(&latest.version);
    json!({
        "state": "available",
        "current_version": TOOLKIT_VERSION,
        "latest": release_json(latest),
        "update_available": matches!((current, released), (Some(c), Some(r)) if r > c),
        "releases": releases.iter().map(release_json).collect::<Vec<_>>(),
    })
}

// ── Download ─────────────────────────────────────────────────────────────

fn refusal(state: &str, reason: &str) -> Value {
    json!({"ok": false, "state": state, "reason": reason})
}

/// `update_download`: fetch one checksummed asset of the last checked release.
pub fn update_download(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 5] = ["consent", "tag", "asset_name", "out", "max_bytes"];
    let Some(consent) = opt(&args, "consent") else {
        return Err(super::missing_option(
            "update-download",
            "consent",
            &ACCEPTED,
        ));
    };
    let tag = super::need_str(&args, "tag", "update-download", &ACCEPTED)?;
    let asset_name = super::need_str(&args, "asset_name", "update-download", &ACCEPTED)?;
    let out = super::need_str(&args, "out", "update-download", &ACCEPTED)?;
    if consent != &Value::Bool(true) {
        return Ok(refusal(
            "consent_required",
            "download requires explicit consent",
        ));
    }
    let max_bytes = opt_int(&args, "max_bytes").unwrap_or(DEFAULT_MAX_DOWNLOAD_BYTES as i64);
    let client = http_client(30.0)
        .map_err(|e| ToolError::Failed(e.to_string()))?
        .0;
    download_update(tag, asset_name, out, max_bytes, &client)
}

/// Validation, cached-release lookup and the bounded asset read behind `update_download`.
pub fn download_update(
    tag: &str,
    asset_name: &str,
    out: &str,
    max_bytes: i64,
    client: &dyn ReconClient,
) -> Result<Value, ToolError> {
    if version_of(tag).is_none() {
        return Err(failed("tag must be a bounded stable release tag"));
    }
    if asset_name.is_empty()
        || asset_name.chars().count() > 255
        || asset_name.contains('/')
        || asset_name.contains('\\')
    {
        return Err(failed("asset_name must be a bounded filename"));
    }
    if !(1..=MAX_DOWNLOAD_BYTES as i64).contains(&max_bytes) {
        return Err(ToolError::Failed(format!(
            "max_bytes must be 1..{MAX_DOWNLOAD_BYTES}"
        )));
    }
    let state = read_state();
    let releases = state
        .as_ref()
        .and_then(|s| s["result"].get("releases"))
        .and_then(Value::as_array);
    let well_formed = |item: &Value| {
        item.get("tag").is_some_and(Value::is_string)
            && item.get("assets").is_some_and(Value::is_array)
    };
    let Some(releases) = releases.filter(|list| list.iter().all(well_formed)) else {
        return Ok(refusal(
            "unavailable",
            "checked release metadata is malformed",
        ));
    };
    let Some(release) = releases.iter().find(|item| item["tag"] == tag) else {
        return Ok(refusal(
            "unavailable",
            "run an update check for this release first",
        ));
    };
    let assets: Vec<Asset> = release["assets"]
        .as_array()
        .map(|list| {
            list.iter()
                .take(MAX_ASSETS)
                .filter_map(|item| {
                    let digest = item.get("sha256").and_then(Value::as_str);
                    asset_of(
                        &json!({
                            "name": item.get("name"),
                            "size": item.get("size"),
                            "browser_download_url": item.get("url"),
                            "digest": digest.map(|d| format!("sha256:{d}")),
                        }),
                        tag,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let Some(asset) = assets.iter().find(|item| item.name == asset_name) else {
        return Ok(refusal(
            "unavailable",
            "asset is not in the checked release metadata",
        ));
    };
    if !(asset_name.ends_with(".whl") || asset_name.ends_with(".tar.gz")) {
        return Ok(refusal(
            "unsupported_asset",
            "only Python wheel and source archive assets are supported",
        ));
    }
    if out.is_empty() || out.chars().count() > 4096 {
        return Err(failed("out must be a bounded output path"));
    }
    let destination = PathBuf::from(out);
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name_ok = destination
        .file_name()
        .is_some_and(|n| n != "." && n != "..");
    if !name_ok || !parent.is_dir() || destination.symlink_metadata().is_ok() {
        return Err(failed("out must be a new file in an existing directory"));
    }
    let headers = [
        ("Accept".to_owned(), "application/octet-stream".to_owned()),
        ("User-Agent".to_owned(), "SpiderHead".to_owned()),
    ];
    let expected = match checksum(client, release, asset, &assets, &headers) {
        Ok(digest) => digest,
        Err(reason) => return Ok(refusal("checksum_unavailable", &reason)),
    };
    let response = match client.get(&asset.url, &headers, &[]) {
        Ok(response) => response,
        Err(_) => {
            return Ok(json!({
                "ok": false,
                "state": "error",
                "error": {"code": "download_failed", "message": "asset download failed (TransportError)"},
            }))
        }
    };
    if response.status_code != 200 {
        return Ok(json!({
            "ok": false,
            "state": "error",
            "error": {
                "code": "download_http_error",
                "message": format!("asset returned HTTP {}", response.status_code),
            },
        }));
    }
    if response.content.len() as i64 > max_bytes {
        return Ok(refusal(
            "size_limit",
            "asset exceeds the configured byte limit",
        ));
    }
    let digest = format!("{:x}", Sha256::digest(&response.content));
    if digest != expected {
        return Ok(refusal(
            "checksum_mismatch",
            "downloaded asset did not match the release SHA-256",
        ));
    }
    let temporary = parent.join(format!(".seohead-update-{}", std::process::id()));
    let publish = std::fs::write(&temporary, &response.content)
        .and_then(|()| std::fs::hard_link(&temporary, &destination));
    let _ = std::fs::remove_file(&temporary);
    if publish.is_err() {
        return Ok(json!({
            "ok": false,
            "state": "error",
            "error": {"code": "download_failed", "message": "asset download failed (OSError)"},
        }));
    }
    let mut done = Map::new();
    for (key, value) in [
        ("ok", json!(true)),
        ("state", json!("downloaded")),
        ("tag", json!(tag)),
        ("asset", json!(asset_name)),
        ("path", json!(destination.to_string_lossy())),
        ("bytes", json!(response.content.len())),
        ("sha256", json!(expected)),
        ("installed", json!(false)),
    ] {
        done.insert(key.to_owned(), value);
    }
    Ok(Value::Object(done))
}

/// The release's SHA-256 for `asset`: its declared digest, else the `SHA256SUMS.txt` entry.
fn checksum(
    client: &dyn ReconClient,
    _release: &Value,
    asset: &Asset,
    assets: &[Asset],
    headers: &[(String, String)],
) -> Result<String, String> {
    if let Some(digest) = &asset.sha256 {
        return Ok(digest.clone());
    }
    let Some(sums) = assets.iter().find(|item| item.name == "SHA256SUMS.txt") else {
        return Err("the release does not provide a SHA-256 checksum for this asset".to_owned());
    };
    let unavailable =
        || "the release checksum file was unavailable or exceeded its byte limit".to_owned();
    let response = client
        .get(&sums.url, headers, &[])
        .map_err(|_| unavailable())?;
    if response.status_code != 200 || response.content.len() > MAX_CHECKSUM_BYTES {
        return Err(unavailable());
    }
    let text = String::from_utf8(response.content).map_err(|_| unavailable())?;
    for line in text.lines() {
        let line = line.trim();
        let Some((digest, name)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let name = name.trim_start().trim_start_matches('*');
        if digest.len() == 64
            && digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            && name == asset.name
        {
            return Ok(digest.to_owned());
        }
    }
    Err("the release checksum file has no entry for this asset".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use spiderhead_recon::net_client::{FakeClient, Response};

    fn response(status: u16, body: &[u8]) -> Response {
        Response {
            status_code: status,
            url: String::new(),
            headers: Vec::new(),
            content: body.to_vec(),
            cookies: Vec::new(),
            http_version: "HTTP/1.1".to_owned(),
            elapsed: std::time::Duration::ZERO,
        }
    }

    const BASE: &str = "https://github.com/PavloSEO/spiderhead/releases";

    fn feed(digest: Option<&str>) -> Value {
        let mut asset = json!({
            "name": "spiderhead-9.9.9-py3-none-any.whl",
            "size": 5,
            "browser_download_url": format!("{BASE}/download/v9.9.9/spiderhead-9.9.9-py3-none-any.whl"),
        });
        if let Some(digest) = digest {
            asset["digest"] = json!(format!("sha256:{digest}"));
        }
        json!([{
            "tag_name": "v9.9.9",
            "html_url": format!("{BASE}/tag/v9.9.9"),
            "body": "notes",
            "assets": [asset],
        }])
    }

    #[test]
    fn check_and_download_run_over_an_injected_client() {
        let directory = std::env::temp_dir().join(format!("sh-update-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("dir");
        std::env::set_var("SEOHEAD_UPDATE_STATE_PATH", directory.join("state.json"));

        // Without consent nothing is read and nothing is stored.
        let refused = check_updates(false, false, 5, None).expect("answer");
        assert_eq!(refused["state"], "consent_required");
        assert!(read_state().is_none());

        // A consented check reads the feed and persists it.
        let body = b"hello";
        let digest = format!("{:x}", Sha256::digest(body));
        let feed_url = format!("https://api.github.com/repos/{REPOSITORY}/releases");
        let mut client = FakeClient::default();
        client.responses.insert(
            feed_url,
            response(200, feed(Some(&digest)).to_string().as_bytes()),
        );
        let asset_url = format!("{BASE}/download/v9.9.9/spiderhead-9.9.9-py3-none-any.whl");
        client
            .responses
            .insert(asset_url.clone(), response(200, body));
        let checked = check_updates(true, false, 5, Some(&client)).expect("check");
        assert_eq!(checked["state"], "available");
        assert_eq!(checked["result"]["update_available"], true);
        assert_eq!(checked["persisted"], true);

        // A fresh automatic check is not due again.
        let again = check_updates(true, true, 5, Some(&client)).expect("check");
        assert_eq!(again["state"], "not_due");

        // The download verifies the checksum and publishes a new file only.
        let out = directory.join("pkg.whl");
        let done = download_update(
            "v9.9.9",
            "spiderhead-9.9.9-py3-none-any.whl",
            &out.to_string_lossy(),
            1024,
            &client,
        )
        .expect("download");
        assert_eq!(done["state"], "downloaded");
        assert_eq!(std::fs::read(&out).expect("file"), body);
        let exists = download_update(
            "v9.9.9",
            "spiderhead-9.9.9-py3-none-any.whl",
            &out.to_string_lossy(),
            1024,
            &client,
        );
        assert!(exists.is_err());

        // A tampered asset is refused and leaves no file behind.
        client.responses.insert(asset_url, response(200, b"HELLO"));
        let tampered = directory.join("bad.whl");
        let refused = download_update(
            "v9.9.9",
            "spiderhead-9.9.9-py3-none-any.whl",
            &tampered.to_string_lossy(),
            1024,
            &client,
        )
        .expect("answer");
        assert_eq!(refused["state"], "checksum_mismatch");
        assert!(!tampered.exists());

        // The byte limit is checked before the digest.
        let limited = download_update(
            "v9.9.9",
            "spiderhead-9.9.9-py3-none-any.whl",
            &directory.join("big.whl").to_string_lossy(),
            2,
            &client,
        )
        .expect("answer");
        assert_eq!(limited["state"], "size_limit");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_checksum_file_supplies_a_missing_digest() {
        let body = b"hello";
        let digest = format!("{:x}", Sha256::digest(body));
        let assets = vec![
            Asset {
                name: "a.whl".to_owned(),
                url: format!("{BASE}/download/v1.0.0/a.whl"),
                size: None,
                sha256: None,
            },
            Asset {
                name: "SHA256SUMS.txt".to_owned(),
                url: format!("{BASE}/download/v1.0.0/SHA256SUMS.txt"),
                size: None,
                sha256: None,
            },
        ];
        let mut client = FakeClient::default();
        client.responses.insert(
            assets[1].url.clone(),
            response(200, format!("{digest}  a.whl\n").as_bytes()),
        );
        let found = checksum(&client, &Value::Null, &assets[0], &assets, &[]);
        assert_eq!(found, Ok(digest));
        client
            .responses
            .insert(assets[1].url.clone(), response(200, b"nothing\n"));
        assert!(checksum(&client, &Value::Null, &assets[0], &assets, &[]).is_err());
    }
}
