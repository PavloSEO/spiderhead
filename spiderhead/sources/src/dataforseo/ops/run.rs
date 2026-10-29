//! Split out of `ops`.

use super::*;

/// `_run` outcome: flattened items, error lines, provider cost, all-failed flag.
#[derive(Debug)]
pub struct RunOutcome {
    /// `task_items(body)`.
    pub items: Vec<Value>,
    /// `task_errors(body)`.
    pub errors: Vec<String>,
    /// `body["cost"]` when present, finite and non-negative.
    pub cost: Option<f64>,
    /// `tasks_all_failed(body)`.
    pub failed: bool,
}

/// `_run(client, kind, payload, items_count)` — post, spend-record, normalize.
pub async fn run<T: Transport>(
    client: &Client<'_, T>,
    kind: &str,
    payload: &Value,
    items_count: i64,
) -> Result<RunOutcome, DfsError> {
    let endpoint = match endpoints(kind) {
        Some(e) => e,
        None => return Err(pyerr("KeyError", pystr::repr_str(kind))),
    };
    let label = format!("{kind}.{}", client.env_name());
    let body = client.post(endpoint, payload, Some(&json!(label))).await?;
    let env_name = client.env_name().to_owned();
    if !body.is_object() {
        client.env.record(&Record {
            source: SOURCE.to_owned(),
            operation: label,
            cost: Some(0.0),
            unit: "usd".to_owned(),
            task_id: None,
            items: Some(items_count),
            extra: Some(Map::from_iter([
                ("env".to_owned(), json!(env_name)),
                ("response_received".to_owned(), json!(true)),
                ("response_malformed".to_owned(), json!(true)),
                ("cost_unknown".to_owned(), json!(true)),
                ("charge_status".to_owned(), json!("unknown")),
            ])),
            at: None,
        })?;
        return Err(DfsError::Provider {
            status: 0,
            message: "provider response must be an object; charge is unknown".to_owned(),
        });
    }
    let cost = body
        .get("cost")
        .and_then(Value::as_f64)
        .filter(|c| c.is_finite() && *c >= 0.0);
    client.env.record(&Record {
        source: SOURCE.to_owned(),
        operation: label,
        cost: Some(cost.unwrap_or(0.0)),
        unit: "usd".to_owned(),
        task_id: None,
        items: Some(items_count),
        extra: Some(Map::from_iter([
            ("env".to_owned(), json!(env_name)),
            ("cost_unknown".to_owned(), json!(cost.is_none())),
            (
                "charge_status".to_owned(),
                json!(if cost.is_none() {
                    "unknown"
                } else {
                    "measured"
                }),
            ),
        ])),
        at: None,
    })?;
    Ok(RunOutcome {
        items: task_items(&body)?,
        errors: task_errors(&body)?,
        cost,
        failed: tasks_all_failed(&body)?,
    })
}

/// `_call` outcome: either a refusal/result envelope or a completed run.
#[derive(Debug)]
pub enum Called {
    /// A dict envelope (`confirmation_required`, `not_configured`, `failed`, `partial`,
    /// ceiling refusals) that wrappers return unchanged.
    Envelope(Value),
    /// `(client.env, items, errors, cost, failed)` — the client env name plus the outcome.
    Ran {
        /// `client.env`.
        env_name: String,
        /// The `_run` tuple.
        outcome: RunOutcome,
    },
}

/// `_call` keyword gate parameters shared by the wrappers.
pub(crate) struct GateArgs<'a> {
    /// `env` selector.
    pub env: Option<&'a Value>,
    /// `confirm_paid`.
    pub confirm_paid: Option<&'a Value>,
    /// `spend_ceiling_usd`.
    pub spend_ceiling_usd: Option<&'a Value>,
    /// `prices` snapshot.
    pub prices: Option<&'a Value>,
    /// `row_limit` estimate input.
    pub row_limit: Option<&'a Value>,
}

/// `_call` — gate before constructing a client; production never calls implicitly.
pub(crate) async fn call<T: Transport>(
    env: &DfsEnv<'_, T>,
    kind: &str,
    payload: Vec<Value>,
    items_count: i64,
    args: &GateArgs<'_>,
) -> Result<Called, DfsError> {
    let query = if kind == "serp" {
        payload
            .first()
            .and_then(|p| p.as_object())
            .and_then(|m| m.get("keyword"))
    } else {
        None
    };
    let (quote, refusal) = preflight(
        env,
        kind,
        &PreflightArgs {
            env: args.env,
            confirm_paid: args.confirm_paid,
            spend_ceiling_usd: args.spend_ceiling_usd,
            prices: args.prices,
            row_limit: args.row_limit,
            require_confirmation: false,
            query,
        },
    )?;
    if let Some(refusal) = refusal {
        return Ok(Called::Envelope(refusal));
    }
    let client = Client::new(
        env,
        &ClientArgs {
            login: None,
            password: None,
            env_name: quote.get("env"),
        },
    )?;
    let outcome = match run(&client, kind, &Value::Array(payload), items_count).await {
        Err(DfsError::MissingCredential(m)) => {
            return Ok(Called::Envelope(
                json!({"ok": false, "state": "not_configured", "error": m}),
            ));
        }
        Err(DfsError::Provider { status, message }) => {
            return Ok(Called::Envelope(
                json!({"ok": false, "state": "failed", "error": message, "status": status}),
            ));
        }
        Err(e) => return Err(e),
        Ok(o) => o,
    };
    if let Some(ceiling) = ceiling_value(args.spend_ceiling_usd) {
        if outcome.cost.is_some_and(|cost| cost > ceiling) {
            return Ok(Called::Envelope(json!({
                "ok": false,
                "state": "partial",
                "error": "provider-reported cost exceeds declared ceiling",
                "cost_usd": outcome.cost,
                "estimate": quote,
            })));
        }
    }
    Ok(Called::Ran {
        env_name: client.env_name().to_owned(),
        outcome,
    })
}
