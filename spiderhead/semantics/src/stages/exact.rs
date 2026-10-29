//! `exact` — paid exact frequency (!W) via Arsenkin with durable task
//! receipts (`wordstat_payload` and `parse_wordstat` helpers).

use indexmap::IndexMap;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::LazyLock;

use crate::config::Cfg;
use crate::demand::has_observed_demand;
use crate::norm::normalize;
use crate::providers::ArsenkinProvider;
use crate::pyfmt::{dumps, py_int, py_repr_str, py_str};
use crate::store::{Row, Store};

const DEFAULT_REGION: i64 = 225;

/// `provider_tasks` DDL — a submission is reserved before the paid call.
const TASK_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS provider_tasks (\
    id INTEGER PRIMARY KEY, provider TEXT NOT NULL, stage TEXT NOT NULL,\
    region INTEGER NOT NULL, payload_json TEXT NOT NULL, state TEXT NOT NULL,\
    task_id INTEGER, cost REAL, result_json TEXT, error TEXT)";

/// Characters the provider rejects; punctuation is stripped.
static DISALLOWED: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[^0-9A-Za-z\u{400}-\u{4ff}\s-]+").unwrap());
/// Whitespace characters normalized to a single space.
static SPACES: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\s+").unwrap());

/// Strip characters the provider refuses.
fn sanitize_wordstat_query(phrase: &str) -> String {
    SPACES
        .replace_all(&DISALLOWED.replace_all(phrase, " "), " ")
        .trim()
        .to_string()
}

/// Request payload for a batch of Wordstat queries.
fn wordstat_payload(queries: &[String], region: i64) -> Result<Value, crate::Error> {
    let kept: Vec<Value> = queries
        .iter()
        .map(|q| sanitize_wordstat_query(q))
        .filter(|q| !q.is_empty())
        .map(Value::String)
        .collect();
    if kept.is_empty() {
        return Err(crate::Error::Config(
            "no usable queries after sanitizing".into(),
        ));
    }
    Ok(json!({
        "type": 1,
        "regions": [region],
        "ws": ["base", "overal"],
        "queries": kept,
    }))
}

/// Parse a task result into `{phrase: {base, overal}}` plus warnings.
fn parse_wordstat(result: &Value, region: i64) -> (IndexMap<String, (Value, Value)>, Vec<String>) {
    let mut frequencies: IndexMap<String, (Value, Value)> = IndexMap::new();
    let mut warnings = Vec::new();
    let mut node = result;
    if node.is_object() && node.get("result").is_some() && node.get("data").is_none() {
        node = &node["result"];
    }
    let rows = node
        .get("data")
        .and_then(|d| d.get("result"))
        .and_then(|r| r.as_object());
    let Some(rows) = rows else {
        warnings.push("wordstat result has no data.result mapping to parse".to_string());
        return (frequencies, warnings);
    };
    let wanted = region.to_string();
    for (phrase, by_region) in rows {
        let Some(by_region) = by_region.as_object() else {
            warnings.push(format!("{}: skipped a non-dict row", py_repr_str(phrase)));
            continue;
        };
        let values = by_region.get(&wanted).filter(|v| !v.is_null());
        let Some(values) = values else {
            let mut available: Vec<&String> = by_region.keys().collect();
            available.sort();
            warnings.push(format!(
                "{}: no data for requested region {wanted} \
                 (provider returned regions: {})",
                py_repr_str(phrase),
                if available.is_empty() {
                    "none".to_string()
                } else {
                    available
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ));
            continue;
        };
        let Some(values) = values.as_object() else {
            warnings.push(format!(
                "{}: region {wanted} data is not a mapping",
                py_repr_str(phrase)
            ));
            continue;
        };
        frequencies.insert(
            phrase.clone(),
            (
                values.get("base").cloned().unwrap_or(Value::Null),
                values.get("overal").cloned().unwrap_or(Value::Null),
            ),
        );
    }
    (frequencies, warnings)
}

/// Frequency value: zero is a measurement; missing, malformed or negative is not.
fn frequency(value: &Value) -> Option<i64> {
    let number = match value {
        Value::Null | Value::Bool(_) => return None,
        Value::Number(n) => match n.as_i64() {
            Some(i) => i,
            None => n.as_f64().unwrap_or(0.0).trunc() as i64,
        },
        Value::String(s) => py_int(s).ok()?,
        _ => return None,
    };
    (number >= 0).then_some(number)
}

/// `x` if truthy, otherwise 0.
fn or_zero(v: Option<&Value>) -> Value {
    match v {
        Some(Value::Null) | None => json!(0),
        Some(Value::Bool(false)) => json!(0),
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => json!(0),
        Some(Value::String(s)) if s.is_empty() => json!(0),
        Some(Value::Array(a)) if a.is_empty() => json!(0),
        Some(Value::Object(o)) if o.is_empty() => json!(0),
        Some(v) => v.clone(),
    }
}

/// Fold a received task result into phrase fields.
fn apply_result(
    store: &Store,
    task: &Row,
    by_group: &IndexMap<String, Vec<String>>,
    cut_b: i64,
    cut_e: i64,
) -> Result<(i64, i64), crate::Error> {
    let region = task["region"].as_i64().unwrap_or(0);
    let raw: Value =
        serde_json::from_str(task["result_json"].as_str().unwrap_or("null")).unwrap_or(Value::Null);
    let (frequencies, mut warnings) = parse_wordstat(&raw, region);
    // quoted is deliberately separate: the parser promises base/overal only.
    let node = if raw.is_object() {
        raw.get("result").unwrap_or(&raw)
    } else {
        &raw
    };
    let raw_rows = node
        .get("data")
        .and_then(|d| d.as_object())
        .and_then(|d| d.get("result"))
        .and_then(|r| r.as_object());
    let payload: Value =
        serde_json::from_str(task["payload_json"].as_str().unwrap_or("{}")).unwrap_or(Value::Null);
    let wanted: HashSet<String> = payload["queries"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|q| q.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let mut missing: Vec<String> = wanted
        .iter()
        .filter(|q| !frequencies.contains_key(*q))
        .cloned()
        .collect();
    missing.sort();
    for m in missing {
        warnings.push(format!("{m}: frequency row missing"));
    }
    let mut snapped = 0i64;
    let mut dead = 0i64;
    for (raw_phrase, values) in &frequencies {
        if !wanted.contains(raw_phrase) {
            continue;
        }
        let p = normalize(raw_phrase);
        let Some(row) = store.q1("SELECT * FROM phrases WHERE norm=?", &[json!(p)])? else {
            continue;
        };
        let key = if !row["lemma_group"].is_null() {
            py_str(&row["lemma_group"])
        } else {
            format!("solo:{p}")
        };
        let cell = raw_rows
            .and_then(|r| r.get(raw_phrase))
            .and_then(|v| v.as_object())
            .and_then(|o| o.get(&region.to_string()))
            .and_then(|v| v.as_object());
        let quoted = cell.and_then(|o| o.get("quoted")).and_then(frequency);
        let freq = frequency(&values.1);
        if freq.is_none() {
            warnings.push(format!("{p}: !W missing or invalid"));
        }
        for member in by_group.get(&key).cloned().unwrap_or_default() {
            // Read current values so a second recovered task cannot overwrite the first.
            let Some(m) = store.q1("SELECT * FROM phrases WHERE norm=?", &[json!(member)])? else {
                continue;
            };
            let mut fields: Vec<(String, Value)> = Vec::new();
            if m["quoted"].is_null() {
                if let Some(q) = quoted {
                    fields.push(("quoted".to_string(), json!(q)));
                }
            }
            if m["exact"].is_null() {
                if let Some(f) = freq {
                    fields.push(("exact".to_string(), json!(f)));
                    snapped += 1;
                }
            }
            if !fields.is_empty() {
                store.set_fields(&member, &fields)?;
            }
            let real_demand =
                has_observed_demand(m["impr"].as_i64(), m["pos_y"].as_i64(), m["pos_g"].as_i64());
            let effective_exact = m["exact"].as_i64().or(freq);
            if !real_demand
                && m["base"].as_i64().is_some_and(|b| b < cut_b)
                && effective_exact.is_some_and(|e| e < cut_e)
            {
                store.set_status(
                    &member,
                    "dead",
                    Some(&format!("base<{cut_b} and exact<{cut_e}")),
                    Some("exact"),
                )?;
                dead += 1;
            }
        }
    }
    store.exec(
        "UPDATE provider_tasks SET state=?,error=? WHERE id=?",
        &[
            json!(if warnings.is_empty() {
                "complete"
            } else {
                "incomplete"
            }),
            if warnings.is_empty() {
                Value::Null
            } else {
                json!(warnings.join("; "))
            },
            task["id"].clone(),
        ],
    )?;
    store.commit()?;
    for w in &warnings {
        println!(
            "  ! exact task {}: {w}",
            task["task_id"].as_i64().unwrap_or(0)
        );
    }
    Ok((snapped, dead))
}

/// Run the paid exact-frequency stage through `client`; `yes` confirms the spend and `limit` caps the phrases queried.
pub fn run(
    store: &Store,
    cfg: &Cfg,
    yes: bool,
    limit: i64,
    client: &mut dyn ArsenkinProvider,
) -> Result<Value, crate::Error> {
    let region = match cfg.get("regions") {
        Some(Value::Array(items)) if !items.is_empty() => py_int(&py_str(&items[0]))
            .map_err(|e| crate::Error::Config(format!("invalid region: {}", e.0)))?,
        _ => DEFAULT_REGION,
    };
    let cut_b = crate::config::cfg_i64(cfg, "exact_cut_base", 20);
    let cut_e = crate::config::cfg_i64(cfg, "exact_cut_exact", 10);
    store.exec(TASK_SCHEMA, &[])?;
    store.commit()?;
    let kept = store.phrases(Some("kept"), false, None)?;
    let mut by_group: IndexMap<String, Vec<String>> = IndexMap::new();
    let mut reps: IndexMap<String, (String, i64)> = IndexMap::new();
    for row in &kept {
        let norm = row["norm"].as_str().unwrap_or_default().to_string();
        let key = if !row["lemma_group"].is_null() {
            py_str(&row["lemma_group"])
        } else {
            format!("solo:{norm}")
        };
        by_group.entry(key.clone()).or_default().push(norm.clone());
        let base = row["base"].as_i64().unwrap_or(0);
        if row["exact"].is_null() && (!reps.contains_key(&key) || base > reps[&key].1) {
            reps.insert(key, (norm, base));
        }
    }
    let tasks = store.q(
        "SELECT * FROM provider_tasks WHERE provider='arsenkin' AND stage='exact' \
         AND region=? ORDER BY id",
        &[json!(region)],
    )?;
    let mut claimed: HashSet<String> = HashSet::new();
    for task in &tasks {
        let payload: Value = serde_json::from_str(task["payload_json"].as_str().unwrap_or("{}"))
            .unwrap_or(Value::Null);
        if let Some(queries) = payload["queries"].as_array() {
            for q in queries {
                if let Some(s) = q.as_str() {
                    claimed.insert(normalize(s));
                }
            }
        }
    }
    // An existing paid representative also covers new members of its lemma group.
    let mut covered = claimed.clone();
    for members in by_group.values() {
        if members.iter().any(|m| claimed.contains(m)) {
            covered.extend(members.iter().cloned());
        }
    }
    let mut rep_list: Vec<(String, i64)> = reps.into_values().collect();
    rep_list.sort_by_key(|item| std::cmp::Reverse(item.1));
    let mut todo: Vec<String> = rep_list
        .into_iter()
        .map(|(p, _)| p)
        .filter(|p| !covered.contains(p))
        .collect();
    if limit != 0 {
        todo.truncate(limit.max(0) as usize);
    }
    if todo.len() as i64 > crate::config::cfg_i64(cfg, "budget_arsenkin_gate", 2000) && !yes {
        println!(
            "[GATE] exact: {} new phrases exceed budget_arsenkin_gate; pass --yes",
            todo.len()
        );
        return Ok(json!({"gated": true}));
    }

    // Reserve each batch just before submission, so unsubmitted later batches stay eligible.
    let mut snapped = 0i64;
    let mut dead = 0i64;
    let mut pending = 0i64;
    let mut blocked = 0i64;
    let mut queue: Vec<Option<Row>> = tasks.into_iter().map(Some).collect();
    for _ in 0..todo.len().div_ceil(100) {
        queue.push(None);
    }
    for task_opt in queue {
        let task = match task_opt {
            Some(t) => t,
            None => {
                let batch: Vec<String> = todo.drain(..todo.len().min(100)).collect();
                let mut payload = wordstat_payload(&batch, region)?;
                let ws = payload["ws"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .chain([json!("quoted")]);
                let mut seen: Vec<Value> = Vec::new();
                payload["ws"] = Value::Array(
                    ws.filter(|v| {
                        if seen.contains(v) {
                            false
                        } else {
                            seen.push(v.clone());
                            true
                        }
                    })
                    .collect(),
                );
                store.exec(
                    "INSERT INTO provider_tasks(provider,stage,region,payload_json,state) \
                     VALUES(?,?,?,?,?)",
                    &[
                        json!("arsenkin"),
                        json!("exact"),
                        json!(region),
                        json!(dumps(&payload)),
                        json!("submitting"),
                    ],
                )?;
                let task_row_id = store.conn.last_insert_rowid();
                store.commit()?;
                let receipt = match client.set_task("wordstat", &payload) {
                    Ok(r) => r,
                    Err(exc) => {
                        let charged = exc
                            .payload
                            .as_ref()
                            .and_then(|p| p.as_object())
                            .and_then(|o| o.get("cost"))
                            .filter(|v| !v.is_null())
                            .cloned();
                        store.exec(
                            "UPDATE provider_tasks SET state='unknown',cost=?,error=? \
                             WHERE id=?",
                            &[
                                charged.clone().unwrap_or(Value::Null),
                                json!(exc.message),
                                json!(task_row_id),
                            ],
                        )?;
                        if let Some(c) = charged {
                            store.add_run_value(
                                "exact",
                                batch.len() as i64,
                                &c,
                                &format!("unknown task, submission={task_row_id}"),
                            )?;
                        } else {
                            store.commit()?;
                        }
                        blocked += 1;
                        println!(
                            "  ! exact submission {task_row_id}: {}; \
                             automatic resubmission blocked",
                            exc.message
                        );
                        continue;
                    }
                };
                let task_id = receipt["task_id"].as_i64();
                match task_id {
                    Some(id) if id > 0 => {
                        store.exec(
                            "UPDATE provider_tasks SET task_id=?,cost=?,state='pending' \
                             WHERE id=?",
                            &[
                                json!(id),
                                receipt.get("cost").cloned().unwrap_or(Value::Null),
                                json!(task_row_id),
                            ],
                        )?;
                        store.add_run_value(
                            "exact",
                            batch.len() as i64,
                            &or_zero(receipt.get("cost")),
                            &format!("task_id={id}"),
                        )?;
                    }
                    _ => {
                        // The toolkit validates task IDs; retain uncertainty if an
                        // adapter violates that contract.
                        store.exec(
                            "UPDATE provider_tasks SET state='unknown',cost=?,error=? \
                             WHERE id=?",
                            &[
                                receipt.get("cost").cloned().unwrap_or(Value::Null),
                                json!("provider returned no usable task_id"),
                                json!(task_row_id),
                            ],
                        )?;
                        store.add_run_value(
                            "exact",
                            batch.len() as i64,
                            &or_zero(receipt.get("cost")),
                            &format!("unknown task, submission={task_row_id}"),
                        )?;
                        blocked += 1;
                        continue;
                    }
                }
                store
                    .q1(
                        "SELECT * FROM provider_tasks WHERE id=?",
                        &[json!(task_row_id)],
                    )?
                    .ok_or_else(|| crate::Error::Config("provider_tasks row vanished".into()))?
            }
        };
        let state = task["state"].as_str().unwrap_or_default().to_string();
        if state == "submitting" || state == "unknown" {
            blocked += 1;
            continue;
        }
        let mut task = task;
        if task["result_json"].is_null() {
            let task_id = task["task_id"].as_i64().unwrap_or(0);
            match client.wait(task_id) {
                Err(exc) => {
                    store.exec(
                        "UPDATE provider_tasks SET state='pending',error=? WHERE id=?",
                        &[json!(exc.message), task["id"].clone()],
                    )?;
                    store.commit()?;
                    pending += 1;
                    println!(
                        "  ! exact task {task_id}: {}; the result can be fetched later",
                        exc.message
                    );
                    continue;
                }
                Ok(raw) => {
                    store.exec(
                        "UPDATE provider_tasks SET result_json=?,state='received',\
                         error=NULL WHERE id=?",
                        &[json!(dumps(&raw)), task["id"].clone()],
                    )?;
                    store.commit()?;
                    task = store
                        .q1(
                            "SELECT * FROM provider_tasks WHERE id=?",
                            &[task["id"].clone()],
                        )?
                        .ok_or_else(|| {
                            crate::Error::Config("provider_tasks row vanished".into())
                        })?;
                }
            }
        }
        let applied = apply_result(store, &task, &by_group, cut_b, cut_e);
        match applied {
            Ok((n, d)) => {
                snapped += n;
                dead += d;
            }
            Err(e) => {
                // Receipt/raw response/cost already committed; retry parsing safely.
                let _ = store.rollback();
                return Err(e);
            }
        }
    }
    let incomplete = store
        .q1(
            "SELECT COUNT(*) c FROM provider_tasks WHERE provider='arsenkin' \
             AND stage='exact' AND region=? AND state='incomplete'",
            &[json!(region)],
        )?
        .map(|r| r["c"].as_i64().unwrap_or(0))
        .unwrap_or(0);
    println!(
        "exact: snapped={snapped}, dead={dead}, pending={pending}, \
         blocked={blocked}, incomplete={incomplete}"
    );
    Ok(json!({
        "snapped": snapped,
        "dead": dead,
        "pending": pending,
        "blocked": blocked,
        "incomplete": incomplete,
    }))
}
