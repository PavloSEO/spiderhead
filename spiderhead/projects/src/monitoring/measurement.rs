use super::*;

/// `_measurement`: derive the tracked-field measurement from a fetched record.
pub(super) fn measurement(record: &Value, source: &Value) -> (Value, &'static str, Option<String>) {
    let error = record.get("error").cloned().unwrap_or(Value::Null);
    let status_code = record.get("status_code").cloned().unwrap_or(Value::Null);
    if py_truthy(&error) || status_code.is_null() {
        let reason = if py_truthy(&error) {
            error.as_str().map(str::to_string)
        } else {
            None
        }
        .unwrap_or_else(|| "collector produced no HTTP status".to_string());
        return (json!({}), "failed", Some(reason));
    }
    if record.get("cache_status").and_then(Value::as_str) == Some("hit") {
        return (
            json!({"status": status_code}),
            "stale",
            Some("fresh cache evidence was reused without a network revalidation".to_string()),
        );
    }
    let body_unavailable = record
        .get("body_unavailable")
        .cloned()
        .unwrap_or(Value::Null);
    if py_truthy(&body_unavailable) || source["body_sha256"].is_null() {
        let reason = if py_truthy(&body_unavailable) {
            body_unavailable.as_str().map(str::to_string)
        } else {
            None
        }
        .unwrap_or_else(|| "body unavailable".to_string());
        return (json!({"status": status_code}), "unavailable", Some(reason));
    }
    let meta = record.get("meta_robots").and_then(Value::as_str);
    let x_robots = record.get("x_robots").and_then(Value::as_str);
    let mut directives = spiderhead_parse::parser::meta::robots_directives(&[meta, x_robots]);
    directives.sort();
    (
        json!({
            "status": status_code,
            "indexability": if directives.iter().any(|d| d == "noindex") {
                "noindex"
            } else {
                "indexable"
            },
            "canonical": record.get("canonical").cloned().unwrap_or(Value::Null),
            "robots": directives.join(","),
            "metadata": {
                "title": record.get("title").cloned().unwrap_or(Value::Null),
                "description": record.get("meta_description").cloned().unwrap_or(Value::Null),
            },
            "content": source["body_sha256"],
            "links": {
                "internal": (record.get("outlinks").and_then(Value::as_i64).unwrap_or_default()
                    - record
                        .get("external_outlinks")
                        .and_then(Value::as_i64)
                        .unwrap_or_default())
                .max(0),
                "sha256": source["links_sha256"],
            },
        }),
        if record.get("cache_status").and_then(Value::as_str) == Some("revalidated") {
            "revalidated"
        } else {
            "fresh"
        },
        None,
    )
}

/// `_claim_preview`: read and validate the active claim; nothing is collected.
pub(super) fn claim_preview(document: &Value, stamp: &str) -> ProjectsResult<Value> {
    let runner = &document["runner"];
    let plan = runner.get("plan").cloned().unwrap_or(Value::Null);
    if runner.get("state").and_then(Value::as_str) != Some("running") || !plan.is_object() {
        return Err(value("an explicit active monitor claim is required"));
    }
    let expiry_at = runner.get("lease_expires_at").and_then(Value::as_str);
    let Some(expiry_at) = expiry_at else {
        return Err(value("monitor claim lacks a lease"));
    };
    let claim_id = runner.get("claim_id").cloned().unwrap_or(Value::Null);
    let claim_text = claim_id
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| crate::bindings::py_str(&claim_id));
    let valid_claim = claim_text
        .strip_prefix("monitor:")
        .and_then(|rest| uuid::Uuid::parse_str(rest).ok())
        .map(|id| format!("monitor:{id}") == claim_text)
        .unwrap_or(false);
    if !valid_claim {
        return Err(value("monitor claim id is invalid"));
    }
    let policy = monitor_policy(document.get("policy").unwrap_or(&Value::Null))?;
    let planned = plan.get("planned_urls").cloned().unwrap_or(Value::Null);
    let dispatched = runner.get("dispatched_urls").cloned().unwrap_or(json!([]));
    let plan_ok = plan.get("state").and_then(Value::as_str) == Some("due")
        && ["full", "incremental"]
            .contains(&plan.get("mode").and_then(Value::as_str).unwrap_or_default())
        && planned
            .as_array()
            .map(|items| {
                !items.is_empty()
                    && items
                        .iter()
                        .map(dumps_compact)
                        .collect::<HashSet<_>>()
                        .len()
                        == items.len()
                    && items.iter().all(|u| {
                        policy["urls"]
                            .as_array()
                            .map(|urls| urls.contains(u))
                            .unwrap_or(false)
                    })
                    && plan["url_limit"] == json!(items.len())
                    && plan["request_budget"] == policy["max_requests"]
                    && plan["render_request_budget"] == policy["max_render_requests"]
                    && (plan["mode"].as_str() != Some("full") || planned == policy["urls"])
                    && (plan["mode"].as_str() != Some("incremental")
                        || items.len() as i64 <= policy["max_urls"].as_i64().unwrap_or_default())
            })
            .unwrap_or(false);
    let dispatch_ok = dispatched
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(dumps_compact)
                .collect::<HashSet<_>>()
                .len()
                == items.len()
                && items
                    .iter()
                    .all(|u| planned.as_array().map(|p| p.contains(u)).unwrap_or(false))
        })
        .unwrap_or(false);
    if !plan_ok || !dispatch_ok {
        return Err(value("monitor claim plan is invalid"));
    }
    let expired = iso_ge(stamp, expiry_at)?;
    Ok(json!({
        "claim_id": claim_id,
        "lease_expires_at": expiry_at,
        "expired": expired,
        "plan": plan,
        "dispatched_urls": dispatched,
        "remaining_urls": planned
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|url| {
                !dispatched
                    .as_array()
                    .map(|d| d.contains(url))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>(),
    }))
}

/// One injected collection step for [`collect_once`].
///
/// Returns `(record, parsed, transport, requests_used_total)` where `record` is the
/// full page-record dict, `parsed` the parse result (or `None`), `transport` the
/// captured response facts, and `requests_used_total` the fetcher's cumulative count.
pub type MonitorFetch<'a> =
    &'a mut dyn FnMut(&str) -> ProjectsResult<(Value, Option<Value>, Value, u64)>;

/// `collect_once`: preview or execute one claimed bounded HTTP/cache pass.
pub fn collect_once(
    directory: &Path,
    expected_revision: i64,
    apply: bool,
    now: Option<&dyn Fn() -> String>,
    fetch: Option<MonitorFetch<'_>>,
) -> ProjectsResult<Value> {
    let (root, mut document) = monitor_load(directory)?;
    if document["revision"] != json!(expected_revision) {
        return Err(value("monitor revision conflict"));
    }
    let clock = || -> String { now.map(|n| n()).unwrap_or_else(zulu) };
    let stamp = clock();
    let preview = claim_preview(&document, &stamp)?;
    if !apply {
        return Ok(json!({
            "ok": true,
            "applied": false,
            "revision": document["revision"],
            "preview": preview,
        }));
    }
    if preview["expired"].as_bool().unwrap_or(false) {
        let mut runner = document["runner"].clone();
        runner["state"] = json!("interrupted");
        runner["interrupted_at"] = json!(stamp);
        runner["reason"] = json!("monitor claim lease expired before collection");
        document["runner"] = runner;
        document["revision"] = json!(expected_revision + 1);
        write_document(&root, NAME, &document, Some(expected_revision))?;
        return Err(value(
            "monitor claim lease expired; explicitly start a new claim",
        ));
    }
    if preview["dispatched_urls"]
        .as_array()
        .map(|d| !d.is_empty())
        .unwrap_or(false)
    {
        return Err(value(
            "monitor claim was interrupted after dispatch; explicitly start a new claim",
        ));
    }
    let remaining = preview["remaining_urls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if remaining.is_empty() {
        return Err(value(
            "monitor claim has no undispatched URLs; explicitly start a new claim",
        ));
    }
    let fetch = match fetch {
        Some(f) => f,
        None => return Err(value("monitor collection requires an injected fetch tool")),
    };
    let mut expected_revision = expected_revision;
    let mut requests_used = 0u64;
    let mut observations = Vec::new();
    for url in &remaining {
        // Write before dispatch: a crashed process leaves unknown work instead of replaying it.
        let (_, reloaded) = monitor_load(directory)?;
        document = reloaded;
        if document["revision"] != json!(expected_revision) {
            return Err(value("monitor revision conflict"));
        }
        let current = claim_preview(&document, &clock())?;
        if current["claim_id"] != preview["claim_id"] {
            return Err(value("monitor claim changed before collection"));
        }
        if current["expired"].as_bool().unwrap_or(false) {
            let mut runner = document["runner"].clone();
            runner["state"] = json!("interrupted");
            runner["interrupted_at"] = json!(clock());
            runner["reason"] = json!("monitor claim lease expired during collection");
            document["runner"] = runner;
            document["revision"] = json!(expected_revision + 1);
            write_document(&root, NAME, &document, Some(expected_revision))?;
            return Err(value(
                "monitor claim lease expired; explicitly start a new claim",
            ));
        }
        {
            let runner = &mut document["runner"];
            let mut dispatched = runner
                .get("dispatched_urls")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            dispatched.push(url.clone());
            runner["dispatched_urls"] = json!(dispatched);
        }
        document["revision"] = json!(expected_revision + 1);
        write_document(&root, NAME, &document, Some(expected_revision))?;
        expected_revision = document["revision"].as_i64().unwrap_or_default();
        let requests_before = requests_used;
        let (record, parsed, transport, used) = fetch(url.as_str().unwrap_or_default())?;
        requests_used = used;
        let src = source_artifacts(
            &root,
            preview["claim_id"].as_str().unwrap_or_default(),
            &record,
            parsed.as_ref(),
            &transport,
        )?;
        let (measure, qualifier, failure_reason) = measurement(&record, &src);
        let cache_state = match qualifier {
            "revalidated" => "revalidated",
            "fresh" => "fresh",
            "stale" => "cached",
            _ => "unavailable",
        };
        observations.push(json!({
            "url": url,
            "changes": [],
            "qualifier": qualifier,
            "measurement": if measure.as_object().map(|m| m.is_empty()).unwrap_or(false) { Value::Null } else { measure },
            "cache_state": cache_state,
            "request_count": (requests_used - requests_before) as i64,
            "render_request_count": 0,
            "failure_reason": failure_reason,
            "source": src,
        }));
    }
    let (_, reloaded) = monitor_load(directory)?;
    document = reloaded;
    if document["revision"] != json!(expected_revision) {
        return Err(value("monitor revision conflict"));
    }
    let current = claim_preview(&document, &clock())?;
    if current["claim_id"] != preview["claim_id"] {
        return Err(value("monitor claim changed before completion"));
    }
    if current["expired"].as_bool().unwrap_or(false) {
        let mut runner = document["runner"].clone();
        runner["state"] = json!("interrupted");
        runner["interrupted_at"] = json!(clock());
        runner["reason"] = json!("monitor claim lease expired before completion");
        document["runner"] = runner;
        document["revision"] = json!(expected_revision + 1);
        write_document(&root, NAME, &document, Some(expected_revision))?;
        return Err(value(
            "monitor claim lease expired; explicitly start a new claim",
        ));
    }
    let retained = run(
        directory,
        preview["claim_id"].as_str().unwrap_or_default(),
        &json!(observations),
        expected_revision,
    )?;
    let mut result = json!({"ok": true, "applied": true, "preview": preview});
    for (key, val) in retained.as_object().cloned().unwrap_or_default() {
        result[key] = val;
    }
    Ok(result)
}

/// `local_deliver`: record one authorized local receipt without a destination.
pub fn local_deliver(
    directory: &Path,
    scan_id: &str,
    expected_revision: i64,
) -> ProjectsResult<Value> {
    let (root, project) = workspace_load(directory)?;
    let reports = reports_root(&root)?;
    let receipt_path = reports.join("monitor-delivery-receipts.sqlite");
    let parent_ok = resolve_lenient(receipt_path.parent().unwrap_or(&reports))
        .starts_with(resolve_lenient(&reports));
    if !parent_ok
        || (receipt_path.symlink_metadata().is_ok()
            && (receipt_path.is_symlink() || !receipt_path.is_file()))
    {
        return Err(value(
            "monitor report target is unsafe: delivery receipt store",
        ));
    }
    let receipts = DeliveryReceipts::new(&receipt_path)?;
    let send_noop: Box<crate::service_delivery::SendFn> = Box::new(|_, _, _| Ok(()));
    let service = MonitorServiceDelivery::new(
        project["project_uuid"].as_str().unwrap_or_default(),
        &["local:receipt".to_string()],
        &receipts,
        send_noop.as_ref(),
        true,
        false,
    )?;
    deliver(
        directory,
        scan_id,
        "local:receipt",
        &service,
        expected_revision,
    )
}

/// `deliver`: hand one retained complete run to an injected authorized service.
pub fn deliver(
    directory: &Path,
    scan_id: &str,
    destination: &str,
    service: &MonitorServiceDelivery<'_>,
    expected_revision: i64,
) -> ProjectsResult<Value> {
    let (root, mut document) = monitor_load(directory)?;
    if document["revision"] != json!(expected_revision) {
        return Err(value("monitor revision conflict"));
    }
    if service.project_uuid != document["project_uuid"].as_str().unwrap_or_default() {
        return Err(py_named(
            "PermissionError",
            "monitor service is not authorized for this project",
        ));
    }
    let position = document["runs"].as_array().and_then(|runs| {
        runs.iter()
            .rposition(|item| item.get("scan_id").and_then(Value::as_str) == Some(scan_id))
    });
    let Some(index) = position else {
        return Err(value("retained monitor scan_id is unknown"));
    };
    let run_doc = document["runs"][index].clone();
    let result = service.deliver(&run_doc, destination)?;
    // Evidence stays immutable; the receipt projection only records that an
    // authorized destination accepted the already-retained event.
    let newly_sent: Vec<Value> = result
        .get("receipts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|receipt| receipt.get("state").and_then(Value::as_str) == Some("sent"))
        .collect();
    if !newly_sent.is_empty() {
        let mut delivery = match document["runs"][index].get("delivery") {
            None => Vec::new(),
            Some(Value::Array(items)) => items.clone(),
            Some(other) => {
                return Err(py_named(
                    "AttributeError",
                    format!("'{}' object has no attribute 'extend'", py_typename(other)),
                ))
            }
        };
        for receipt in &newly_sent {
            let mut entry = json!({"destination": destination});
            for (key, val) in receipt.as_object().cloned().unwrap_or_default() {
                entry[key] = val;
            }
            delivery.push(entry);
        }
        document["runs"][index]["delivery"] = json!(delivery);
        document["revision"] = json!(expected_revision + 1);
        write_document(&root, NAME, &document, Some(expected_revision))?;
    }
    let mut out = json!({"ok": true, "revision": document["revision"]});
    for (key, val) in result.as_object().cloned().unwrap_or_default() {
        out[key] = val;
    }
    Ok(out)
}

/// `status`: read the monitor document without changing anything.
pub fn status(directory: &Path) -> ProjectsResult<Value> {
    let (_, document) = monitor_load(directory)?;
    Ok(json!({
        "ok": true,
        "revision": document["revision"],
        "policy": document["policy"],
        "runner": document["runner"],
        "last_run": document["runs"].as_array().and_then(|r| r.last()).cloned().unwrap_or(Value::Null),
        "due": due(&document)?,
    }))
}
