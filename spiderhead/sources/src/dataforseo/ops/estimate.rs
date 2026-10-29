//! Pre-flight cost estimates for DataForSEO calls, built from a supplied price snapshot.
use super::*;

/// `estimate(operation, ...)` — a bounded-call quote from a supplied price snapshot.
/// No credential lookup or network request happens here.
pub fn estimate<T: Transport>(
    env: &DfsEnv<'_, T>,
    operation: &Value,
    args: &EstimateArgs<'_>,
) -> Result<Value, DfsError> {
    let Some(op) = operation.as_str().filter(|s| endpoints(s).is_some()) else {
        return Err(value_err("unsupported DataForSEO operation"));
    };
    let d1 = json!(1);
    let d0 = json!(0);
    let query_count = nonneg_int("query_count", args.query_count.unwrap_or(&d1))?;
    let row_limit = nonneg_int("row_limit", args.row_limit.unwrap_or(&d0))?;
    let cache_hits = nonneg_int("cache_hits", args.cache_hits.unwrap_or(&d0))?;
    if cache_hits > query_count {
        return Err(value_err("cache_hits cannot exceed query_count"));
    }
    let requested_env = super::super::resolve_env(args.env, env.host)?;
    let mut result = json!({
        "operation": op,
        "env": requested_env,
        "currency": "USD",
        "query_count": query_count,
        "cache_hits": cache_hits,
        "cache_basis": "caller_declared",
        "billable_queries": query_count - cache_hits,
        "row_limit": row_limit,
        "cost_usd": Value::Null,
        "state": "unavailable",
        "reason": "a current appendix/user_data price snapshot is required",
    });
    if requested_env == "sandbox" || query_count == cache_hits {
        result["cost_usd"] = json!(0.0);
        result["state"] = json!("complete");
        result["reason"] = json!("sandbox or cached data incurs no new charge");
        return Ok(result);
    }
    let pages = if op == "serp" {
        serp_pages(row_limit)
    } else {
        1
    };
    let mut search_multiplier = 1i64;
    if op == "serp" {
        match args.query.filter(|v| !v.is_null()) {
            None => {
                result["reason"] =
                    json!("the SERP query is required to account for search-operator pricing");
                return Ok(result);
            }
            Some(v) if !v.is_string() => return Err(value_err("query must be a string")),
            Some(v) => {
                if search_operator(&unquote_plus(v.as_str().unwrap_or_default())) {
                    search_multiplier = 5;
                }
            }
        }
        result["billable_serp_pages_per_query"] = json!(pages);
        result["search_operator_multiplier"] = json!(search_multiplier);
    }
    let mut node = args.prices.cloned().unwrap_or_else(|| json!({}));
    if let Some(path) = endpoints(op) {
        for part in path.split('/').skip(1) {
            node = if node.is_object() {
                node.get(part).cloned().unwrap_or_else(|| json!({}))
            } else {
                json!({})
            };
        }
    }
    let empty = Vec::new();
    let rates = node
        .get("priority_normal")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if rates.is_empty() {
        return Ok(result);
    }
    let (mut per_request, mut per_result) = (0.0f64, 0.0f64);
    for rate in rates {
        let Some(map) = rate.as_object() else {
            return Ok(result);
        };
        let kind = pystr::repr_value(map.get("cost_type").unwrap_or(&json!(""))).replace('_', "");
        let cost = match map.get("cost") {
            Some(Value::Number(n)) if n.as_f64().is_some_and(|c| c.is_finite() && c >= 0.0) => {
                n.as_f64().unwrap_or_default()
            }
            _ => return Ok(result),
        };
        match kind.as_str() {
            "perrequest" => per_request += cost,
            "perresult" => per_result += cost,
            _ => {
                result["reason"] = json!("the supplied price uses an unsupported billing unit");
                return Ok(result);
            }
        }
    }
    let total = (per_request * pages as f64 + per_result * row_limit as f64)
        * search_multiplier as f64
        * (query_count - cache_hits) as f64;
    if !total.is_finite() {
        result["reason"] = json!("supplied pricing or volume exceeds a finite quote");
        return Ok(result);
    }
    result["cost_usd"] = json!(round10(total));
    result["state"] = json!("complete");
    result["reason"] = json!("upper bound from supplied provider base prices");
    result["pricing_source"] = json!("appendix/user_data");
    Ok(result)
}

/// Preflight arguments; `None` means the default applies.
pub(crate) struct PreflightArgs<'a> {
    /// `env` selector.
    pub env: Option<&'a Value>,
    /// `confirm_paid`.
    pub confirm_paid: Option<&'a Value>,
    /// `spend_ceiling_usd`.
    pub spend_ceiling_usd: Option<&'a Value>,
    /// `prices` snapshot.
    pub prices: Option<&'a Value>,
    /// `row_limit` estimate input (raw value — validation is `estimate`'s job).
    pub row_limit: Option<&'a Value>,
    /// `require_confirmation` — backlink calls always confirm.
    pub require_confirmation: bool,
    /// `query` for SERP operator pricing.
    pub query: Option<&'a Value>,
}

/// `spend_ceiling_usd` when it is a JSON number; an explicit null is skipped.
pub(super) fn ceiling_value(v: Option<&Value>) -> Option<f64> {
    match v.filter(|v| !v.is_null()) {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

/// `spend_ceiling_usd` is a finite non-negative number — `bool` and other types reject.
pub(super) fn ceiling_valid(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Number(n)) if n.as_f64().is_some_and(|f| f.is_finite() && f >= 0.0))
}

/// Estimate the call, then check the ceiling and confirmation.
/// Returns `(quote, refusal)`; a `Some` refusal means the caller returns it unchanged.
pub(crate) fn preflight<T: Transport>(
    env: &DfsEnv<'_, T>,
    operation: &str,
    args: &PreflightArgs<'_>,
) -> Result<(Value, Option<Value>), DfsError> {
    let quote = estimate(
        env,
        &json!(operation),
        &EstimateArgs {
            query_count: None,
            row_limit: args.row_limit,
            cache_hits: None,
            prices: args.prices,
            env: args.env,
            query: args.query,
        },
    )?;
    if args.spend_ceiling_usd.is_some_and(|v| !v.is_null())
        && !ceiling_valid(args.spend_ceiling_usd)
    {
        return Err(value_err(
            "spend_ceiling_usd must be a finite non-negative number",
        ));
    }
    if quote.get("env").and_then(Value::as_str) == Some("prod") || args.require_confirmation {
        if let Some(refusal) = spend_gate::confirmation_required(
            pystr::truthy(args.confirm_paid),
            operation,
            Some(&quote),
        ) {
            return Ok((quote, Some(refusal)));
        }
    }
    if let Some(ceiling) = ceiling_value(args.spend_ceiling_usd) {
        match quote.get("cost_usd").and_then(Value::as_f64) {
            None => {
                return Ok((
                    quote.clone(),
                    Some(json!({
                        "ok": false,
                        "state": "unavailable",
                        "reason": "cannot enforce spend ceiling without current pricing",
                        "estimate": quote,
                    })),
                ));
            }
            Some(cost) if cost > ceiling => {
                return Ok((
                    quote.clone(),
                    Some(json!({
                        "ok": false,
                        "state": "spend_limit_exceeded",
                        "reason": "estimated cost exceeds spend_ceiling_usd before request",
                        "estimate": quote,
                    })),
                ));
            }
            _ => {}
        }
    }
    Ok((quote, None))
}
