//! Split out of `ops`.

use super::*;

/// `backlink_call` keyword arguments.
pub struct BacklinkArgs<'a, T: Transport> {
    /// `limit` (`100`, bounded 1–1000).
    pub limit: Option<&'a Value>,
    /// `offset` (`0`, non-negative).
    pub offset: Option<&'a Value>,
    /// `enabled` — the adapter is disabled by default.
    pub enabled: Option<&'a Value>,
    /// `account_eligible` — backlink calls are opt-in per account.
    pub account_eligible: Option<&'a Value>,
    /// `production_approved` — required for `prod`.
    pub production_approved: Option<&'a Value>,
    /// `cost_approved` — required for `prod`.
    pub cost_approved: Option<&'a Value>,
    /// `confirm_paid` — always required for backlink calls.
    pub confirm_paid: Option<&'a Value>,
    /// `spend_ceiling_usd`.
    pub spend_ceiling_usd: Option<&'a Value>,
    /// `cache_key` — an explicit local cache key is required.
    pub cache_key: Option<&'a Value>,
    /// `env`.
    pub env: Option<&'a Value>,
    /// `prices` snapshot.
    pub prices: Option<&'a Value>,
    /// `client` — an injected client whose env must match the requested one.
    pub client: Option<&'a Client<'a, T>>,
    /// `date_from` (`backlinks_history` only).
    pub date_from: Option<&'a Value>,
    /// `date_to` (`backlinks_history` only).
    pub date_to: Option<&'a Value>,
}

impl<'a, T: Transport> Default for BacklinkArgs<'a, T> {
    fn default() -> Self {
        BacklinkArgs {
            limit: None,
            offset: None,
            enabled: None,
            account_eligible: None,
            production_approved: None,
            cost_approved: None,
            confirm_paid: None,
            spend_ceiling_usd: None,
            cache_key: None,
            env: None,
            prices: None,
            client: None,
            date_from: None,
            date_to: None,
        }
    }
}

impl<'a, T: Transport> std::fmt::Debug for BacklinkArgs<'a, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BacklinkArgs").finish_non_exhaustive()
    }
}

/// `date.fromisoformat(value)` when truthy — `TypeError` for non-str, `ValueError` text
/// for malformed ISO dates.
pub(super) fn history_date(v: Option<&Value>) -> Result<Option<i64>, DfsError> {
    match v.filter(|v| pystr::truthy(Some(v))) {
        None => Ok(None),
        Some(v) => match v.as_str() {
            None => Err(type_err("fromisoformat: argument must be str")),
            Some(text) => pytime::date_fromisoformat_err(text)
                .map(Some)
                .map_err(|m| value_err(&m)),
        },
    }
}

/// `backlink_call(operation, target, ...)` — one explicitly selected bounded
/// backlink-index operation, never auto-paginate.
pub async fn backlink_call<T: Transport>(
    env: &DfsEnv<'_, T>,
    operation: &Value,
    target: &Value,
    args: &BacklinkArgs<'_, T>,
) -> Result<Value, DfsError> {
    const OPS: &[&str] = &[
        "backlinks_summary",
        "backlinks",
        "referring_domains",
        "backlinks_history",
        "domain_pages_summary",
    ];
    let Some(op) = operation.as_str().filter(|s| OPS.contains(s)) else {
        return Err(value_err("unsupported backlink operation"));
    };
    if !pystr::truthy(Some(target)) || !target.is_string() {
        return Err(value_err("target required"));
    }
    let default_limit = json!(100);
    let default_offset = json!(0);
    let limit = args.limit.unwrap_or(&default_limit);
    let limit_i = match pystr::as_strict_i64(Some(limit)) {
        Some(v) if (1..=1000).contains(&v) => v,
        _ => return Err(value_err("limit must be between 1 and 1000")),
    };
    let offset = args.offset.unwrap_or(&default_offset);
    let offset_i = match pystr::as_strict_i64(Some(offset)) {
        Some(v) if v >= 0 => v,
        _ => return Err(value_err("offset must be a non-negative integer")),
    };
    if !pystr::truthy(args.enabled) {
        return Ok(json!({
            "ok": false,
            "state": "skipped",
            "reason": "backlink adapter is disabled by default",
        }));
    }
    if !pystr::truthy(args.account_eligible) || !pystr::truthy(args.cache_key) {
        return Ok(json!({
            "ok": false,
            "state": "skipped",
            "reason": "account eligibility and an explicit local cache key are required",
        }));
    }
    let requested_env = super::super::resolve_env(args.env, env.host)?;
    if let Some(client) = args.client {
        if client.env_name() != requested_env {
            return Err(value_err(
                "injected client environment must match requested environment",
            ));
        }
    }
    if requested_env == "prod"
        && (!pystr::truthy(args.production_approved) || !pystr::truthy(args.cost_approved))
    {
        return Ok(json!({
            "ok": false,
            "state": "skipped",
            "reason": "production backlink calls require explicit production and cost approval",
        }));
    }
    let mut history_dates = Value::Null;
    if op == "backlinks_history" {
        let end = match history_date(args.date_to)? {
            Some(d) => d,
            None => env.today(),
        };
        let start = match history_date(args.date_from)? {
            Some(d) => d,
            None => end - 30,
        };
        if start < pytime::days_from_civil(2019, 1, 1) || start > end || end > env.today() {
            return Err(value_err(
                "history dates must be ordered within 2019-01-01 and today",
            ));
        }
        let (sy, sm, _) = pytime::civil_from_days(start);
        let (ey, em, _) = pytime::civil_from_days(end);
        let months = (ey - sy) * 12 + em - sm + 1;
        if months > limit_i {
            return Err(value_err(
                "history date range exceeds the declared row limit",
            ));
        }
        let (_, _, sd) = pytime::civil_from_days(start);
        let (_, _, ed) = pytime::civil_from_days(end);
        history_dates = json!({
            "date_from": format!("{sy:04}-{sm:02}-{sd:02}"),
            "date_to": format!("{ey:04}-{em:02}-{ed:02}"),
        });
    }
    let row_limit = if matches!(op, "backlinks_summary" | "domain_pages_summary") {
        json!(1)
    } else {
        limit.clone()
    };
    let env_v = json!(requested_env);
    let (quote, refusal) = preflight(
        env,
        op,
        &PreflightArgs {
            env: Some(&env_v),
            confirm_paid: args.confirm_paid,
            spend_ceiling_usd: args.spend_ceiling_usd,
            prices: args.prices,
            row_limit: Some(&row_limit),
            require_confirmation: true,
            query: None,
        },
    )?;
    if let Some(refusal) = refusal {
        return Ok(refusal);
    }
    let mut payload = Map::new();
    payload.insert("target".to_owned(), target.clone());
    if let Some(m) = history_dates.as_object() {
        payload.extend(m.clone());
    }
    match op {
        "backlinks" | "referring_domains" => {
            payload.insert("limit".to_owned(), json!(limit_i));
            payload.insert("offset".to_owned(), json!(offset_i));
        }
        "backlinks_summary" => {
            payload.insert("internal_list_limit".to_owned(), json!(0));
            payload.insert("backlinks_limit".to_owned(), json!(0));
        }
        _ => {}
    }
    let owned;
    let source = match args.client {
        Some(client) => client,
        None => {
            owned = Client::new(
                env,
                &ClientArgs {
                    login: None,
                    password: None,
                    env_name: Some(&env_v),
                },
            )?;
            &owned
        }
    };
    let payload_v = Value::Array(vec![Value::Object(payload)]);
    let outcome = match run(source, op, &payload_v, 1).await {
        Err(DfsError::MissingCredential(m)) => {
            return Ok(json!({"ok": false, "state": "not_configured", "error": m}));
        }
        Err(DfsError::Provider { status, message }) => {
            return Ok(json!({"ok": false, "state": "failed", "error": message, "status": status}));
        }
        Err(e) => return Err(e),
        Ok(o) => o,
    };
    if let Some(ceiling) = ceiling_value(args.spend_ceiling_usd) {
        if outcome.cost.is_some_and(|cost| cost > ceiling) {
            return Ok(json!({
                "ok": false,
                "state": "partial",
                "error": "provider-reported cost exceeds the declared spend ceiling",
                "cost_usd": outcome.cost,
                "estimate": quote,
            }));
        }
    }
    Ok(json!({
        "ok": !outcome.failed,
        "state": if outcome.failed { "failed" } else { "complete" },
        "env": source.env_name(),
        "operation": op,
        "api_version": "v3",
        "cache_key": args.cache_key.cloned().unwrap_or(Value::Null),
        "cost_usd": outcome.cost,
        "errors": outcome.errors,
        "estimate": quote,
        "items": outcome.items,
        "summary": if op == "backlinks_summary" {
            json!(outcome.items)
        } else {
            Value::Null
        },
        "coverage": {"bounded": true, "limit": limit_i, "offset": offset_i, "auto_paginated": false},
    }))
}

/// `backlinks_summary(target, **kwargs)`.
pub async fn backlinks_summary<T: Transport>(
    env: &DfsEnv<'_, T>,
    target: &Value,
    args: &BacklinkArgs<'_, T>,
) -> Result<Value, DfsError> {
    backlink_call(env, &json!("backlinks_summary"), target, args).await
}

/// `backlinks(target, **kwargs)`.
pub async fn backlinks<T: Transport>(
    env: &DfsEnv<'_, T>,
    target: &Value,
    args: &BacklinkArgs<'_, T>,
) -> Result<Value, DfsError> {
    backlink_call(env, &json!("backlinks"), target, args).await
}

/// `referring_domains(target, **kwargs)`.
pub async fn referring_domains<T: Transport>(
    env: &DfsEnv<'_, T>,
    target: &Value,
    args: &BacklinkArgs<'_, T>,
) -> Result<Value, DfsError> {
    backlink_call(env, &json!("referring_domains"), target, args).await
}

/// `backlinks_history(target, **kwargs)`.
pub async fn backlinks_history<T: Transport>(
    env: &DfsEnv<'_, T>,
    target: &Value,
    args: &BacklinkArgs<'_, T>,
) -> Result<Value, DfsError> {
    backlink_call(env, &json!("backlinks_history"), target, args).await
}

/// `domain_pages_summary(target, **kwargs)`.
pub async fn domain_pages_summary<T: Transport>(
    env: &DfsEnv<'_, T>,
    target: &Value,
    args: &BacklinkArgs<'_, T>,
) -> Result<Value, DfsError> {
    backlink_call(env, &json!("domain_pages_summary"), target, args).await
}
