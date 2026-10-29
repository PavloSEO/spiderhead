//! Split out of `providers`.

use super::*;

/// `_CREDENTIAL_SOURCES` — `(config path, env var)` per component.
pub fn credential_sources(provider: &str) -> &'static [(&'static str, &'static str, &'static str)] {
    match provider {
        "arsenkin" => &[("api_token", "arsenkin/token", "ARSENKIN_TOKEN")],
        "yandex_cloud" => &[
            ("api_key", "yandex-wordstat/api_key", "YANDEX_CLOUD_API_KEY"),
            (
                "folder_id",
                "yandex-wordstat/folder_id",
                "YANDEX_CLOUD_FOLDER_ID",
            ),
        ],
        "gsc" => &[("oauth_bearer", "gsc/access_token", "GSC_ACCESS_TOKEN")],
        "crux" => &[("api_key", "crux/api_key", "CRUX_API_KEY")],
        "pagespeed" => &[("api_key", "pagespeed/api_key", "PAGESPEED_API_KEY")],
        "ga4" => &[("oauth_bearer", "ga4/access_token", "GA4_ACCESS_TOKEN")],
        "metrika" => &[(
            "oauth_bearer",
            "yandex-metrika/token",
            "YANDEX_METRIKA_TOKEN",
        )],
        "yandex_webmaster" => &[(
            "oauth_bearer",
            "yandex-webmaster/access_token",
            "YANDEX_WEBMASTER_TOKEN",
        )],
        "miratext" => &[("api_key", "miratext/api_key", "MIRATEXT_API_KEY")],
        "bing_webmaster" => &[(
            "api_key",
            "bing-webmaster/api_key",
            "BING_WEBMASTER_API_KEY",
        )],
        "dataforseo_backlinks" => &[
            ("login", "dataforseo/login", "DATAFORSEO_LOGIN"),
            ("password", "dataforseo/password", "DATAFORSEO_PASSWORD"),
        ],
        "moz" => &[("api_token", "moz/api_token", "MOZ_API_TOKEN")],
        "cloudflare" => &[("api_token", "cloudflare/api-token", "CLOUDFLARE_API_TOKEN")],
        "indexnow" => &[("submission_key", "indexnow/key", "INDEXNOW_KEY")],
        _ => &[],
    }
}

pub(super) fn collectable(provider: &str, operation: &str) -> bool {
    match provider {
        "gsc" => matches!(
            operation,
            "verify" | "properties" | "search_analytics" | "inspection" | "sitemaps"
        ),
        "crux" => matches!(operation, "current" | "history"),
        "pagespeed" => matches!(operation, "mobile_samples" | "desktop_samples"),
        "ga4" => matches!(operation, "landing_pages" | "page_views"),
        "metrika" => matches!(operation, "counters" | "aggregate_report"),
        "yandex_webmaster" => WEBMASTER_OPS.contains(&operation),
        "bing_webmaster" => matches!(
            operation,
            "sites" | "crawl" | "links" | "url_links" | "keywords" | "search_performance"
        ),
        "cloudflare" => matches!(operation, "traffic" | "logpull" | "history"),
        "dataforseo_backlinks" => matches!(
            operation,
            "backlinks_summary"
                | "backlinks"
                | "referring_domains"
                | "backlinks_history"
                | "domain_pages_summary"
        ),
        "wayback" => operation == "history",
        "crtsh" => operation == "subdomains",
        _ => false,
    }
}

pub(super) fn dedicated(provider: &str, operation: &str) -> Option<&'static str> {
    match provider {
        "arsenkin" if operation == "keyword_frequency" => Some("keywords-exact"),
        "yandex_cloud" if operation == "wordstat" => Some("keywords-expand / keywords-seasonality"),
        "yandex_cloud" if operation == "web_search" => Some("serp-fetch"),
        "indexnow" if operation == "submit" => Some("indexnow-submit"),
        "miratext" if operation == "seo_analysis" => Some("miratext-analyze"),
        "cloudflare" if operation == "token_verify" => Some("provider-verify"),
        _ => None,
    }
}

pub(crate) fn registry_entry(provider: &str) -> Option<&'static RegistryEntry> {
    REGISTRY
        .iter()
        .find(|(name, _)| *name == provider)
        .map(|(_, e)| e)
}

/// `provider_registry()` — a JSON-safe copy of the declaration table.
pub fn provider_registry() -> Value {
    let mut providers = Map::new();
    for (name, (components, access, operations, quota, privacy)) in REGISTRY {
        let mut entry = Map::new();
        let insert = |entry: &mut Map<String, Value>, key: &str, value: Value| {
            entry.insert(key.to_owned(), value);
        };
        insert(&mut entry, "credential_components", json!(components));
        insert(&mut entry, "access", json!(access));
        insert(&mut entry, "operations", json!(operations));
        if *name == "crux" {
            // The oracle's literal places cost_mode before quota_mode.
            insert(&mut entry, "cost_mode", json!("free_within_quota"));
        }
        insert(&mut entry, "quota_mode", json!(quota));
        insert(&mut entry, "privacy_class", json!(privacy));
        if *name == "metrika" {
            insert(&mut entry, "excluded_operations", json!(EXCLUDED_METRIKA));
        }
        if DEFAULT_DISABLED.contains(name) {
            insert(&mut entry, "default_enabled", json!(false));
        }
        providers.insert(name.to_string(), Value::Object(entry));
    }
    json!({"format": "spiderhead.provider-registry.v1", "providers": providers})
}

pub(super) fn credential_source_status(
    env: &ProviderEnv,
    provider: &str,
    component: &str,
) -> Value {
    let (_, path, env_var) = credential_sources(provider)
        .iter()
        .find(|(name, _, _)| *name == component)
        .copied()
        .unwrap_or(("", "", ""));
    let file_env_var = (provider == "cloudflare" && component == "api_token")
        .then_some("SEOHEAD_CLOUDFLARE_TOKEN_FILE");
    credentials::source_status(env.host, env.root, path, env_var, file_env_var)
}

/// `credential_slot(provider, component)` — never reads the secret value.
pub fn credential_slot(
    env: &ProviderEnv,
    provider: &str,
    component: &str,
) -> Result<Value, CfError> {
    if registry_entry(provider).is_none() {
        return Err(CfError::Value("unknown provider".to_owned()));
    }
    let sources = credential_sources(provider);
    let Some((_, path, env_var)) = sources
        .iter()
        .find(|(name, _, _)| *name == component)
        .copied()
    else {
        return Err(CfError::Value(format!(
            "{} is not a stored credential component of {}",
            pystr::repr_str(component),
            pystr::repr_str(provider),
        )));
    };
    let env_shadowed = env
        .host
        .var(env_var)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    let mut slot = json!({
        "provider": provider,
        "component": component,
        "source_reference": format!("config:{path}"),
        "env_reference": format!("env:{env_var}"),
        "env_shadowed": env_shadowed,
    });
    if provider == "cloudflare" && component == "api_token" {
        slot["accepted_source_references"] = json!([
            format!("env:{env_var}"),
            format!("config:{path}"),
            "env-path:SEOHEAD_CLOUDFLARE_TOKEN_FILE",
        ]);
        slot["env_shadowed"] = json!(
            env_shadowed
                || env
                    .host
                    .var("SEOHEAD_CLOUDFLARE_TOKEN_FILE")
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false)
        );
    }
    Ok(slot)
}

pub(super) fn mtime_iso(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let secs = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(crate::pytime::isoformat(
        (secs.as_secs_f64() * 1_000_000.0) as i64,
    ))
}

/// `manage_credential(provider, component, action, *, value, replace, confirm)`.
pub fn manage_credential(
    env: &ProviderEnv,
    provider: &str,
    component: &str,
    action: &str,
    value: Option<&Value>,
    replace: bool,
    confirm: bool,
) -> Result<Value, CfError> {
    let slot = credential_slot(env, provider, component)?;
    let (_, path, _) = credential_sources(provider)
        .iter()
        .find(|(name, _, _)| *name == component)
        .copied()
        .unwrap_or(("", "", ""));
    if action == "status" {
        let state = credential_source_status(env, provider, component);
        let mut result = json!({
            "ok": true,
            "provider": provider,
            "component": component,
        });
        if let (Value::Object(dst), Value::Object(src)) = (&mut result, &slot) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        result["source_reference"] = state
            .get("source_reference")
            .cloned()
            .filter(|v| !v.is_null())
            .unwrap_or_else(|| slot["source_reference"].clone());
        result["accepted_source_references"] = state
            .get("accepted_source_references")
            .cloned()
            .unwrap_or_else(|| {
                slot.get("accepted_source_references")
                    .cloned()
                    .unwrap_or(json!([]))
            });
        result["state"] = state["state"].clone();
        result["updated_at"] = Value::Null;
        if state.get("state") == Some(&json!("configured_unverified"))
            && state.get("source_reference") == slot.get("source_reference")
        {
            match mtime_iso(&env.root.join(path)) {
                Some(stamp) => result["updated_at"] = json!(stamp),
                None => result["state"] = json!("unavailable"),
            }
        }
        return Ok(result);
    }
    if action == "set" {
        let text = match value.and_then(Value::as_str) {
            Some(t) => t,
            None => {
                return Err(CfError::Value(
                    "credential value must be non-empty text no larger than 65536 bytes".to_owned(),
                ))
            }
        };
        let once = text.strip_suffix('\n').unwrap_or(text);
        let stored = once.strip_suffix('\r').unwrap_or(once);
        if stored.trim().is_empty() || stored.len() > 65536 {
            return Err(CfError::Value(
                "credential value must be non-empty text no larger than 65536 bytes".to_owned(),
            ));
        }
        if stored.chars().any(|c| (c as u32) < 32 || c as u32 == 127) {
            return Err(CfError::Value(
                "credential value contains an unsupported character".to_owned(),
            ));
        }
        credentials::write_secret(env.root, path, stored, replace).map_err(|e| {
            let message = e.to_string();
            let class = if message.contains("already exists") {
                "FileExistsError"
            } else {
                "CredentialError"
            };
            CfError::Python(class, message)
        })?;
        return Ok(json!({
            "ok": true,
            "provider": provider,
            "component": component,
            "source_reference": slot["source_reference"],
            "env_shadowed": slot["env_shadowed"],
            "updated_at": mtime_iso(&env.root.join(path)),
            "replaced": replace,
        }));
    }
    if action == "revoke" {
        if !confirm {
            return Err(CfError::Value(
                "credential revocation requires confirm=true".to_owned(),
            ));
        }
        let removed = credentials::revoke_secret(env.root, path)
            .map_err(|e| CfError::Python("CredentialError", e.to_string()))?;
        return Ok(json!({
            "ok": true,
            "provider": provider,
            "component": component,
            "source_reference": slot["source_reference"],
            "env_shadowed": slot["env_shadowed"],
            "revoked": removed,
        }));
    }
    Err(CfError::Value(
        "action must be status, set, or revoke".to_owned(),
    ))
}

pub(super) fn split_host(raw: &str) -> String {
    let trimmed = raw.trim();
    let rest = if let Some(rest) = trimmed.strip_prefix("//") {
        rest
    } else if let Some((_, rest)) = trimmed.split_once("://") {
        rest
    } else {
        return String::new();
    };
    let host_port = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    let host = host_port.split(':').next().unwrap_or("");
    host.to_lowercase()
}
