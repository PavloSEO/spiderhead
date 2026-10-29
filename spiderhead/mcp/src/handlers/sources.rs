//! Handlers of the sources group (the reference suite) that run fully
//! offline: the provider spend journal report. The networked provider commands
//! stay unwired (`NotYetPorted` by absence from the registry).

use std::path::Path;

use serde_json::{json, Map, Value};

use spiderhead_core::host::{Host, SystemHost};

use crate::error::ToolError;

use super::{kwargs, need_str, opt_int, opt_str};

/// `spend_report`: summarize recorded provider charges, or write one CSV row
/// per journal entry (`data_sources.spend`).
pub fn spend_report(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let host = SystemHost;
    let since = args
        .get("since")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    // The CLI flag's dest is `csv`; the MCP tool argument is `csv_path`.
    let csv_path = args
        .get("csv")
        .or_else(|| args.get("csv_path"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    if let Some(csv_path) = csv_path {
        let count = spiderhead_sources::spend::write_csv(&host, Path::new(csv_path), since)
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        return Ok(json!({
            "ok": true,
            "csv": expanduser(&host, csv_path),
            "rows": count,
            "since": since,
        }));
    }
    Ok(spiderhead_sources::spend::report(&host, since))
}

/// reference `Path.expanduser` for the returned CSV location.
fn expanduser(host: &SystemHost, value: &str) -> String {
    if let Some(rest) = value.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with('/') {
            let home = host
                .var("HOME")
                .filter(|value| !value.is_empty())
                .unwrap_or_default();
            return format!("{home}{rest}");
        }
    }
    value.to_owned()
}

/// `credentials.gsc_service_account_document` — the status half only: coarse
/// filesystem/validation state, never key material.
pub(super) fn gsc_service_account_status(host: &SystemHost, root: &Path) -> String {
    const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
    const MAX_BYTES: u64 = 65536;
    let path = match host
        .var("GSC_SERVICE_ACCOUNT_FILE")
        .filter(|v| !v.is_empty())
    {
        Some(configured) => {
            let expanded = if let Some(rest) = configured.strip_prefix('~') {
                if rest.is_empty() || rest.starts_with('/') {
                    format!(
                        "{}{rest}",
                        host.var("HOME")
                            .filter(|v| !v.is_empty())
                            .unwrap_or_default()
                    )
                } else {
                    configured.clone()
                }
            } else {
                configured
            };
            std::path::PathBuf::from(expanded)
        }
        None => root.join("gsc/service-account.json"),
    };
    if path.is_symlink() {
        return "unsafe_file".to_owned();
    }
    let Ok(meta) = std::fs::metadata(&path) else {
        return "missing".to_owned();
    };
    let private = spiderhead_sources::credentials::is_private_mode(
        spiderhead_core::filesystem::StatCompat::mode(&meta),
    );
    if !meta.is_file() || !private {
        return "unsafe_file".to_owned();
    }
    if meta.len() > MAX_BYTES {
        return "too_large".to_owned();
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return "unreadable".to_owned();
    };
    let Ok(document) = serde_json::from_slice::<Value>(&bytes) else {
        return "malformed_json".to_owned();
    };
    let Some(map) = document.as_object() else {
        return "unsupported_shape".to_owned();
    };
    let valid = map.get("type").and_then(Value::as_str) == Some("service_account")
        && map.get("token_uri").and_then(Value::as_str) == Some(GOOGLE_TOKEN_URI)
        && ["client_email", "private_key", "token_uri"]
            .iter()
            .all(|field| {
                map.get(*field)
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty())
            });
    if valid {
        "configured_unverified".to_owned()
    } else {
        "unsupported_shape".to_owned()
    }
}

/// Runs `f` with the process provider environment behind
/// `sources_doctor`/`provider_readiness`: the real `$HOME/.config` root, the
/// real gsc service-account and oauth-grant state, and inert seams for the
/// collect/dispatch paths these handlers never reach.
fn with_provider_env<T>(f: impl FnOnce(&spiderhead_sources::providers::ProviderEnv) -> T) -> T {
    let host = SystemHost;
    let root = spiderhead_sources::credentials::default_config_root(&host);
    let service_account_available =
        || gsc_service_account_status(&host, &root) == "configured_unverified";
    let service_account_status = || gsc_service_account_status(&host, &root);
    let grant_available =
        |provider: &str| spiderhead_sources::oauth::grant_available(&root, provider);
    let now = || String::new();
    let monotonic = || 0.0;
    let dispatch = |_route: &str, _request: Option<&Value>| {
        Err(spiderhead_sources::providers::DispatchError::plain(
            "RuntimeError",
            "provider dispatch is not available in this handler",
        ))
    };
    let open_scan = |_path: &str| {
        Err(spiderhead_sources::cloudflare::CfError::Os(
            "open_scan is not available in this handler".to_owned(),
        ))
    };
    let env = spiderhead_sources::providers::ProviderEnv {
        host: &host,
        root: &root,
        service_account_available: &service_account_available,
        service_account_status: &service_account_status,
        grant_available: &grant_available,
        now: &now,
        monotonic: &monotonic,
        dispatch: &dispatch,
        open_scan: &open_scan,
    };
    f(&env)
}

/// `sources_doctor`: redacted credential references and readiness; offline.
/// Composes the credential checks, the dataforseo/gsc component states and the
/// provider doctor's table — reference the reference suite.
pub fn sources_doctor(_args: &Value) -> Result<Value, ToolError> {
    use spiderhead_sources::credentials;
    let host = SystemHost;
    let root = credentials::default_config_root(&host);
    let checks: [(&str, &str, &str); 9] = [
        ("arsenkin", "arsenkin/token", "ARSENKIN_TOKEN"),
        (
            "yandex_cloud_api_key",
            "yandex-wordstat/api_key",
            "YANDEX_CLOUD_API_KEY",
        ),
        (
            "yandex_cloud_folder",
            "yandex-wordstat/folder_id",
            "YANDEX_CLOUD_FOLDER_ID",
        ),
        (
            "yandex_metrika",
            "yandex-metrika/token",
            "YANDEX_METRIKA_TOKEN",
        ),
        ("dataforseo", "dataforseo/login", "DATAFORSEO_LOGIN"),
        ("gsc", "gsc/access_token", "GSC_ACCESS_TOKEN"),
        ("crux", "crux/api_key", "CRUX_API_KEY"),
        ("indexnow", "indexnow/key", "INDEXNOW_KEY"),
        ("miratext", "miratext/api_key", "MIRATEXT_API_KEY"),
    ];
    let mut sources = Map::new();
    for (name, path, env_var) in checks {
        sources.insert(
            name.to_owned(),
            json!({
                "ready": credentials::available(&host, &root, path, env_var),
                "file": root.join(path).to_string_lossy(),
                "env": env_var,
            }),
        );
    }
    let (dataforseo_ok, dataforseo_components) = credentials::dataforseo_ready(&host, &root);
    if let Some(entry) = sources.get_mut("dataforseo") {
        entry["ready"] = json!(dataforseo_ok);
        entry["components"] = dataforseo_components;
    }
    let provider_status = with_provider_env(spiderhead_sources::providers::sources_doctor);
    let gsc_components = provider_status["providers"]["gsc"]["credential_components"].clone();
    if let Some(entry) = sources.get_mut("gsc") {
        let any_ready = gsc_components
            .as_object()
            .map(|map| map.values().filter_map(Value::as_bool).any(|ready| ready))
            .unwrap_or(false);
        entry["ready"] = json!(any_ready);
        entry["components"] = gsc_components;
        entry["service_account_status"] =
            provider_status["providers"]["gsc"]["service_account_status"].clone();
    }
    Ok(json!({
        "ok": true,
        "sources": sources,
        "provider_status": provider_status["providers"],
        "spend_log": spiderhead_sources::spend::log_path(&host).to_string_lossy(),
    }))
}

/// `provider_registry`: the immutable-by-convention provider metadata.
pub fn provider_registry(_args: &Value) -> Result<Value, ToolError> {
    Ok(spiderhead_sources::providers::provider_registry())
}

/// `provider_readiness`: credential state and operation support; offline.
pub fn provider_readiness(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    Ok(with_provider_env(|env| {
        spiderhead_sources::providers::provider_readiness(
            env,
            args.get("provider"),
            args.get("operation"),
        )
    }))
}

/// `sources_status`: requested/complete/empty/partial/failed day coverage.
pub fn sources_status(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let path =
        spiderhead_sources::sources_db::db_path(opt_str(&args, "db"), opt_str(&args, "project"))
            .map_err(|error| ToolError::Failed(error.to_string()))?;
    spiderhead_sources::sources_db::status(&path)
        .map_err(|error| ToolError::Failed(error.to_string()))
}

/// `sources_export`: at most `limit` ordered provider rows, or a private CSV.
pub fn sources_export(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 9] = [
        "source",
        "resource",
        "start_date",
        "end_date",
        "match",
        "limit",
        "out",
        "db",
        "project",
    ];
    let source = need_str(&args, "source", "sources-export", &ACCEPTED)?;
    if source.is_empty() {
        // the reference handler raises before any database work.
        let mut names: Vec<&str> = spiderhead_sources::sources_db::SOURCES
            .iter()
            .map(|(name, _)| *name)
            .collect();
        names.sort_unstable();
        let listed = names
            .iter()
            .map(|name| format!("'{name}'"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ToolError::Failed(format!(
            "source required: one of [{listed}]"
        )));
    }
    let path =
        spiderhead_sources::sources_db::db_path(opt_str(&args, "db"), opt_str(&args, "project"))
            .map_err(|error| ToolError::Failed(error.to_string()))?;
    let query_args = spiderhead_sources::sources_db::QueryArgs {
        resource: opt_str(&args, "resource"),
        start_date: opt_str(&args, "start_date"),
        end_date: opt_str(&args, "end_date"),
        match_text: opt_str(&args, "match"),
        limit: opt_int(&args, "limit").unwrap_or(1000),
        out: opt_str(&args, "out"),
    };
    spiderhead_sources::sources_db::query(&path, source, &query_args)
        .map_err(|error| ToolError::Failed(error.to_string()))
}
