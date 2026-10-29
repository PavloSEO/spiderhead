//! Yandex Wordstat, Yandex SERP and Arsenkin exact-frequency handlers
//! (the reference suite).

use serde_json::{json, Map, Value};

use spiderhead_sources::arsenkin::{
    parse_wordstat, sanitize_wordstat_query, wordstat_payload, ArsEnv, ArsError, Client,
    ClientArgs, WORDSTAT_TOOL,
};
use spiderhead_sources::pystr;
use spiderhead_sources::yandex_cloud::{Cloud, CloudArgs, CloudEnv, CloudError};

use crate::error::ToolError;

use super::sources_net::{environment, fail, present, run, str_list, transport};
use super::{kwargs, opt, opt_bool, opt_str};

/// the reference `RuntimeError` family at this boundary: the handler reports these
/// as an `ok: false` document instead of failing the call.
fn is_runtime(error: &CloudError) -> bool {
    matches!(
        error.python_class(),
        "MissingCredential" | "NetworkAmbiguousError" | "RuntimeError"
    )
}

fn regions_or_default(args: &Map<String, Value>) -> Value {
    match str_list(args, "regions").filter(|items| !items.is_empty()) {
        Some(items) => json!(items),
        None => json!(["225"]),
    }
}

/// `keywords_expand`: Wordstat refinements and related queries for a phrase.
pub fn keywords_expand(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(phrase) = present(&args, "phrase") else {
        return Err(fail("phrase required"));
    };
    let (host, root) = environment();
    let transport = transport()?;
    let env = CloudEnv::live(&host, &root, &transport);
    let client = match Cloud::new(&env, &CloudArgs::default()) {
        Ok(client) => client,
        Err(error) if is_runtime(&error) => {
            return Ok(json!({"ok": false, "error": error.to_string()}))
        }
        Err(error) => return Err(fail(error)),
    };
    let limit = opt(&args, "limit").cloned().unwrap_or(json!(300));
    let expanded = run(client.wordstat_expand(
        &json!(phrase),
        &limit,
        &regions_or_default(&args),
        &json!(["DEVICE_ALL"]),
    ))?;
    let pair = match expanded {
        Ok(pair) => pair,
        Err(error) if is_runtime(&error) => {
            return Ok(json!({"ok": false, "error": error.to_string()}))
        }
        Err(error) => return Err(fail(error)),
    };
    let pool = pair[0].as_object().cloned().unwrap_or_default();
    let meta = &pair[1];
    let mut ranked: Vec<(&String, i64)> = pool
        .iter()
        .map(|(phrase, count)| (phrase, count.as_i64().unwrap_or(0)))
        .collect();
    ranked.sort_by_key(|(_, count)| -count);
    Ok(json!({
        "ok": true,
        "phrase": phrase,
        "found": ranked.len(),
        "total_count": meta["totalCount"],
        "from_results": meta["results"],
        "from_associations": meta["associations"],
        "keywords": ranked.iter().map(|(phrase, count)| json!({
            "phrase": phrase,
            "base_frequency": count,
            "origin": meta["origin"].get(phrase.as_str()),
        })).collect::<Vec<_>>(),
    }))
}

/// `keywords_seasonality`: Wordstat demand dynamics over an RFC 3339 range.
pub fn keywords_seasonality(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let (phrase, from_date, to_date) = (
        present(&args, "phrase"),
        present(&args, "from_date"),
        present(&args, "to_date"),
    );
    let (Some(phrase), Some(from_date), Some(to_date)) = (phrase, from_date, to_date) else {
        return Err(fail("phrase, from_date and to_date required"));
    };
    let (host, root) = environment();
    let transport = transport()?;
    let env = CloudEnv::live(&host, &root, &transport);
    let period = opt(&args, "period")
        .cloned()
        .unwrap_or(json!("PERIOD_MONTHLY"));
    let outcome = match Cloud::new(&env, &CloudArgs::default()) {
        Ok(client) => run(client.wordstat_dynamics(
            &json!(phrase),
            &json!(from_date),
            &json!(to_date),
            &period,
            &regions_or_default(&args),
            &json!(["DEVICE_ALL"]),
        ))?,
        Err(error) => Err(error),
    };
    match outcome {
        Ok(body) => Ok(json!({"ok": true, "phrase": phrase, "period": period, "dynamics": body})),
        Err(error) if is_runtime(&error) => Ok(json!({"ok": false, "error": error.to_string()})),
        Err(error) => Err(fail(error)),
    }
}

/// `serp_fetch`: Yandex results through the asynchronous API for one or more queries.
pub fn serp_fetch(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let mut targets: Vec<String> = Vec::new();
    if let Some(query) = present(&args, "query") {
        targets.push(query.to_owned());
    }
    targets.extend(str_list(&args, "queries").unwrap_or_default());
    targets.retain(|query| !query.is_empty());
    if targets.is_empty() {
        return Err(fail("query or queries required"));
    }
    let mut unique: Vec<String> = Vec::new();
    for target in targets {
        if !unique.contains(&target) {
            unique.push(target);
        }
    }
    let region = opt_str(&args, "region").unwrap_or("225");
    let top = opt(&args, "top").cloned().unwrap_or(json!(10));
    let (host, root) = environment();
    let transport = transport()?;
    let env = CloudEnv::live(&host, &root, &transport);
    let outcome = match Cloud::new(&env, &CloudArgs::default()) {
        Ok(client) => run(client.search_batch(
            &json!(unique),
            &json!(region),
            &json!("SEARCH_TYPE_RU"),
            &top,
            &json!(1),
            &json!("FAMILY_MODE_NONE"),
            &json!(300),
            &json!(4),
        ))?,
        Err(error) => Err(error),
    };
    let raw = match outcome {
        Ok(raw) => raw,
        Err(error) if is_runtime(&error) => {
            return Ok(json!({"ok": false, "error": error.to_string()}))
        }
        Err(error) => return Err(fail(error)),
    };
    let raw = raw.as_object().cloned().unwrap_or_default();
    let mut results = Map::new();
    for (query, value) in &raw {
        let mut entry = Map::new();
        entry.insert(
            "docs".to_owned(),
            value
                .get("docs")
                .cloned()
                .filter(|v| pystr::truthy(Some(v)))
                .unwrap_or(json!([])),
        );
        entry.insert(
            "error".to_owned(),
            value.get("error").cloned().unwrap_or(Value::Null),
        );
        entry.insert(
            "status".to_owned(),
            value.get("status").cloned().unwrap_or(Value::Null),
        );
        for key in ["operation_id", "http_status"] {
            if let Some(found) = value.get(key).filter(|v| !v.is_null()) {
                entry.insert(key.to_owned(), found.clone());
            }
        }
        results.insert(query.clone(), Value::Object(entry));
    }
    let missing: Vec<&String> = unique
        .iter()
        .filter(|q| !results.contains_key(*q))
        .collect();
    Ok(json!({
        "ok": true,
        "region": region,
        "requested": unique.len(),
        "returned": results.len(),
        "results": results,
        "not_returned": missing,
        "note": if missing.is_empty() {
            Value::Null
        } else {
            json!("queries not returned before timeout were already billed; their operations are recorded in the spend journal")
        },
    }))
}

/// `keywords_exact`: exact `!W` frequency through Arsenkin's wordstat tool.
pub fn keywords_exact(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let keywords: Vec<Value> = opt(&args, "keywords")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if keywords.is_empty() {
        return Err(fail("keywords required"));
    }
    let region_raw = opt(&args, "region").cloned().unwrap_or(json!(225));
    let wait = opt_bool(&args, "wait").unwrap_or(true);
    let region = match pystr::to_python_int(&region_raw) {
        Ok(region) => region,
        Err(_) => {
            return Ok(json!({
                "ok": false,
                "error": format!("invalid region {}", pystr::repr(&region_raw)),
                "code": "INVALID_REGION",
            }))
        }
    };
    let mut cleaned = Map::new();
    for original in &keywords {
        let sanitized = sanitize_wordstat_query(original);
        if sanitized != pystr::repr_value(original) {
            cleaned.insert(pystr::repr_value(original), json!(sanitized));
        }
    }
    let payload = match wordstat_payload(&Value::Array(keywords.clone()), &json!(region)) {
        Ok(payload) => payload,
        Err(ArsError::Python { class, message }) if class == "ValueError" => {
            return Ok(json!({
                "ok": false, "error": message, "code": "EMPTY_QUERIES", "cleaned": cleaned,
            }))
        }
        Err(error) => return Err(fail(error)),
    };
    let (host, root) = environment();
    let transport = transport()?;
    let env = ArsEnv::live(&host, &root, &transport);
    let client = match Client::new(&env, ClientArgs::default()) {
        Ok(client) => client,
        Err(error) => return arsenkin_failure(error, None),
    };
    let task = match run(client.set_task(&json!(WORDSTAT_TOOL), &payload))? {
        Ok(task) => task,
        Err(error) => return arsenkin_failure(error, None),
    };
    if !wait {
        return Ok(json!({
            "ok": true,
            "task_id": task["task_id"],
            "cost": task["cost"],
            "region": region,
            "cleaned": cleaned,
            "note": "task created and billed; retrieve the result later by task_id",
        }));
    }
    let outcome = run(async {
        let result = client.wait(&task["task_id"], None, None).await?;
        let payload_result = result.get("result").cloned().unwrap_or(result);
        let parsed = parse_wordstat(&payload_result, &json!(region))?;
        Ok::<_, ArsError>((payload_result, parsed))
    })?;
    match outcome {
        Ok((payload_result, parsed)) => {
            let mut response = json!({
                "ok": true,
                "task_id": task["task_id"],
                "cost": task["cost"],
                "region": region,
                "cleaned": cleaned,
                "frequencies": parsed["frequencies"],
                "result": payload_result,
            });
            if pystr::truthy(parsed.get("warnings")) {
                response["warnings"] = parsed["warnings"].clone();
            }
            Ok(response)
        }
        Err(error) => arsenkin_failure(error, Some(&task)),
    }
}

/// the reference `except MissingCredential` / `except ArsenkinError` arms; any other
/// failure propagates to the CLI boundary.
fn arsenkin_failure(error: ArsError, task: Option<&Value>) -> Result<Value, ToolError> {
    match &error {
        ArsError::MissingCredential(message) => Ok(json!({"ok": false, "error": message})),
        ArsError::Provider(_) => {
            let mut document = json!({
                "ok": false,
                "error": error.to_string(),
                "code": error.code(),
            });
            if let Some(task) = task {
                document["task_id"] = task["task_id"].clone();
                document["cost"] = task["cost"].clone();
            }
            Ok(document)
        }
        ArsError::Python { .. } => Err(fail(error)),
    }
}
