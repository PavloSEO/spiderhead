//! Offline cluster and landing read models: the
//! `clusters`, `cluster-show`, `cluster-queries` and `set-landing` stages.
//!
//! Every document is one JSON object with a fixed key set; a
//! missing reference raises [`Error::Config`] with the message text.

use serde_json::{json, Map, Value};

use crate::store::{Row, Store};
use crate::Error;

/// Maximum page size for read models.
const MAX_PAGE_SIZE: i64 = 100;

/// The phrase columns `cluster-queries` projects, in output order.
const QUERY_FIELDS: [&str; 13] = [
    "norm",
    "base",
    "quoted",
    "exact",
    "impr",
    "pos_y",
    "pos_g",
    "status",
    "route",
    "group_name",
    "subgroup",
    "url",
    "comment",
];

fn page(offset: i64, limit: i64) -> Result<(), Error> {
    if offset < 0 {
        return Err(Error::Config(
            "offset must be a non-negative integer".into(),
        ));
    }
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(Error::Config(format!(
            "limit must be between 1 and {MAX_PAGE_SIZE}"
        )));
    }
    Ok(())
}

/// The cluster row by numeric id or exact name.
fn cluster(store: &Store, reference: &str) -> Result<Row, Error> {
    let row = if reference.chars().all(|c| c.is_ascii_digit()) && !reference.is_empty() {
        store.q1(
            "SELECT * FROM clusters WHERE id=?",
            &[json!(reference.parse::<i64>().unwrap_or_default())],
        )?
    } else {
        store.q1("SELECT * FROM clusters WHERE name=?", &[json!(reference)])?
    };
    row.ok_or_else(|| Error::Config(format!("cluster not found: {reference}")))
}

fn row_value(row: &Row) -> Value {
    Value::Object(row.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// One page of cluster aggregates with landing-candidate scores.
pub fn clusters(store: &Store, offset: i64, limit: i64) -> Result<Value, Error> {
    page(offset, limit)?;
    let available: Vec<String> = store.columns("clusters")?;
    let unavailable_cluster_fields: Vec<Value> = ["intent", "source", "coherence", "landing_url"]
        .iter()
        .filter(|field| !available.contains(&(*field).to_string()))
        .map(|field| json!(field))
        .collect();
    let rows = store.cluster_rows(offset, limit + 1)?;
    let mut items: Vec<Value> = Vec::new();
    for row in rows.iter().take(limit as usize) {
        let mut item: Map<String, Value> =
            row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        item.insert(
            "unavailable_fields".into(),
            json!(unavailable_cluster_fields),
        );
        let landing_url = item
            .get("landing_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let id = item.get("id").cloned().unwrap_or(json!(0));
        let candidate = store
            .q1(
                "SELECT score FROM landing_candidates WHERE cluster_id=? AND url=?",
                &[id, json!(landing_url)],
            )
            .ok()
            .flatten();
        item.insert(
            "landing_score".into(),
            candidate
                .as_ref()
                .and_then(|c| c.get("score").cloned())
                .unwrap_or(Value::Null),
        );
        item.insert(
            "landing_score_unavailable_reason".into(),
            if candidate.is_some() {
                Value::Null
            } else {
                json!("no stored score for the selected landing URL")
            },
        );
        items.push(Value::Object(item));
    }
    let total = store
        .q1("SELECT COUNT(*) n FROM clusters", &[])?
        .and_then(|r| r["n"].as_i64())
        .unwrap_or(0);
    Ok(json!({
        "format": "spiderhead.semantics.clusters.v1",
        "items": items,
        "offset": offset,
        "limit": limit,
        "total": total,
        "has_more": rows.len() > limit as usize,
    }))
}

/// One cluster with its aggregate and landing candidates.
pub fn cluster_show(store: &Store, reference: &str) -> Result<Value, Error> {
    let cluster = cluster(store, reference)?;
    let id = cluster.get("id").cloned().unwrap_or(json!(0));
    let aggregate = store.cluster_row(id.as_i64().unwrap_or(0))?;
    let candidates: Vec<Value> = match store.q(
        "SELECT url,score,evidence_json,computed_at FROM landing_candidates \
         WHERE cluster_id=? ORDER BY score DESC, url LIMIT 25",
        &[id],
    ) {
        Ok(rows) => rows
            .iter()
            .map(|row| {
                let mut item: Map<String, Value> =
                    row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                let evidence = item
                    .get("evidence_json")
                    .and_then(Value::as_str)
                    .and_then(|text| serde_json::from_str::<Value>(text).ok())
                    .unwrap_or(Value::Null);
                item.insert("evidence".into(), evidence);
                Value::Object(item)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    let mut cluster_data: Map<String, Value> = cluster
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    cluster_data
        .entry("landing_url".to_owned())
        .or_insert(Value::Null);
    let has_landing_column = store
        .columns("clusters")?
        .iter()
        .any(|c| c == "landing_url");
    Ok(json!({
        "format": "spiderhead.semantics.cluster.v1",
        "cluster": Value::Object(cluster_data),
        "landing_url_unavailable_reason": if has_landing_column { Value::Null } else {
            json!("landing URLs are unavailable in this legacy database")
        },
        "aggregate": aggregate.map(|a| row_value(&a)),
        "landing_candidates": candidates,
        "landing_candidates_unavailable_reason": if candidates.is_empty() {
            json!("candidate scores have not been computed from retained SERP evidence")
        } else {
            Value::Null
        },
    }))
}

/// One page of a cluster's phrases, with the retained TOP-10 results.
pub fn cluster_queries(
    store: &Store,
    reference: &str,
    offset: i64,
    limit: i64,
    project: Option<&str>,
) -> Result<Value, Error> {
    page(offset, limit)?;
    let cluster = cluster(store, reference)?;
    let id = cluster.get("id").cloned().unwrap_or(json!(0));
    let total = store
        .q1(
            "SELECT COUNT(*) n FROM phrases WHERE cluster_id=? AND status IN ('kept','review')",
            std::slice::from_ref(&id),
        )?
        .and_then(|r| r["n"].as_i64())
        .unwrap_or(0);
    let available: Vec<String> = store.columns("phrases")?;
    let present: Vec<&str> = QUERY_FIELDS
        .iter()
        .copied()
        .filter(|field| available.contains(&(*field).to_string()))
        .collect();
    let sql = format!(
        "SELECT {} FROM phrases WHERE cluster_id=? AND status IN ('kept','review') \
         ORDER BY base DESC,id LIMIT ? OFFSET ?",
        present.join(",")
    );
    let rows = store.q(&sql, &[id.clone(), json!(limit + 1), json!(offset)])?;
    let has_receipts = !store
        .q(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='serp_fetches'",
            &[],
        )?
        .is_empty();
    let receipt_has_time = has_receipts
        && store
            .columns("serp_fetches")?
            .iter()
            .any(|column| column == "fetched_at");
    let receipt_time_expr = if receipt_has_time {
        "fetched_at"
    } else {
        "NULL AS fetched_at"
    };
    let configured_region: Option<String> = match project {
        Some(project) => Some(
            crate::config::load(project)?
                .get("regions")
                .and_then(Value::as_array)
                .and_then(|regions| regions.first())
                .map(|first| match first {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_else(|| "225".to_owned()),
        ),
        None => None,
    };
    let mut items: Vec<Value> = Vec::new();
    for row in rows.iter().take(limit as usize) {
        let mut item: Map<String, Value> = QUERY_FIELDS
            .iter()
            .map(|field| (field.to_string(), Value::Null))
            .collect();
        for (key, value) in row {
            item.insert(key.clone(), value.clone());
        }
        let norm = item.get("norm").and_then(Value::as_str).unwrap_or_default();
        let mut receipt: Option<Row> = None;
        let mut any_receipt = false;
        if has_receipts {
            any_receipt = !store
                .q(
                    "SELECT 1 FROM serp_fetches WHERE phrase_norm=? AND top=10 LIMIT 1",
                    &[json!(norm)],
                )?
                .is_empty();
            receipt = match &configured_region {
                Some(region) => store.q1(
                    &format!(
                        "SELECT region,top,state,raw_json,{receipt_time_expr} FROM serp_fetches \
                         WHERE phrase_norm=? AND region=? AND top=10"
                    ),
                    &[json!(norm), json!(region)],
                )?,
                None => store.q1(
                    &format!(
                        "SELECT region,top,state,raw_json,{receipt_time_expr} FROM serp_fetches \
                         WHERE phrase_norm=? AND top=10 ORDER BY region LIMIT 1"
                    ),
                    &[json!(norm)],
                )?,
            };
        }
        let mut legacy_fetched = Value::Null;
        if !any_receipt {
            legacy_fetched = store
                .q1(
                    "SELECT MAX(fetched_at) AS fetched_at FROM serp WHERE phrase_norm=?",
                    &[json!(norm)],
                )?
                .and_then(|r| r.get("fetched_at").cloned())
                .unwrap_or(Value::Null);
        }
        let mut results: Vec<Value> = Vec::new();
        let receipt_state_complete = receipt
            .as_ref()
            .and_then(|r| r.get("state").and_then(Value::as_str))
            == Some("complete");
        if receipt_state_complete {
            if let Some(text) = receipt
                .as_ref()
                .and_then(|r| r.get("raw_json"))
                .and_then(Value::as_str)
            {
                if let Ok(docs) = serde_json::from_str::<Value>(text) {
                    results = docs
                        .get("docs")
                        .and_then(Value::as_array)
                        .map(|docs| docs.iter().take(10).cloned().collect())
                        .unwrap_or_default();
                }
            }
        } else if receipt.is_none() && !any_receipt {
            results = store.q(
                "SELECT pos,url,domain,title FROM serp WHERE phrase_norm=? ORDER BY pos LIMIT 10",
                &[json!(norm)],
            )?
            .iter()
            .map(row_value)
            .collect();
        }
        let region = receipt
            .as_ref()
            .and_then(|r| r.get("region").cloned())
            .unwrap_or(Value::Null);
        let fetched_at = receipt
            .as_ref()
            .and_then(|r| r.get("fetched_at").cloned())
            .filter(|v| !v.is_null())
            .unwrap_or(legacy_fetched);
        let state = match &receipt {
            Some(r) => r.get("state").cloned().unwrap_or(Value::Null),
            None => json!(if results.is_empty() {
                "unavailable"
            } else {
                "legacy_cache"
            }),
        };
        let unavailable_reason = if !results.is_empty() {
            Value::Null
        } else if any_receipt && receipt.is_none() {
            json!(format!(
                "no retained SERP receipt for configured region {}",
                configured_region.clone().unwrap_or_default()
            ))
        } else if let Some(r) = &receipt {
            json!(format!(
                "SERP receipt state is {}",
                r.get("state").and_then(Value::as_str).unwrap_or_default()
            ))
        } else {
            json!("no retained Yandex TOP-10 evidence")
        };
        let region_unavailable_reason = if !results.is_empty() && receipt.is_none() {
            json!("legacy SERP rows do not retain region")
        } else {
            Value::Null
        };
        let fetch_date_unavailable_reason = match &receipt {
            Some(r) if r.get("fetched_at").map(Value::is_null).unwrap_or(false) => {
                json!("this retained receipt predates fetch timestamps")
            }
            _ => Value::Null,
        };
        item.insert(
            "yandex_top10".into(),
            json!({
                "results": results,
                "region": region,
                "fetched_at": fetched_at,
                "state": state,
                "unavailable_reason": unavailable_reason,
                "region_unavailable_reason": region_unavailable_reason,
                "fetch_date_unavailable_reason": fetch_date_unavailable_reason,
            }),
        );
        items.push(Value::Object(item));
    }
    Ok(json!({
        "format": "spiderhead.semantics.cluster-queries.v1",
        "cluster": {"id": cluster.get("id").cloned().unwrap_or(Value::Null),
                    "name": cluster.get("name").cloned().unwrap_or(Value::Null)},
        "items": items,
        "unavailable_fields": QUERY_FIELDS.iter()
            .filter(|field| !available.contains(&(**field).to_string()))
            .map(|field| json!(field))
            .collect::<Vec<Value>>(),
        "offset": offset,
        "limit": limit,
        "total": total,
        "has_more": rows.len() > limit as usize,
    }))
}

/// `set-landing`: set or clear a cluster's landing URL (a journaled write).
pub fn set_landing(store: &Store, reference: &str, url: &str) -> Result<Value, Error> {
    let cluster = cluster(store, reference)?;
    let id = cluster.get("id").cloned().unwrap_or(json!(0));
    let trimmed = url.trim();
    let landing: Option<String> = if trimmed.is_empty() {
        None
    } else {
        if trimmed.contains(['\n', '\r', '\0']) {
            return Err(Error::Config(
                "landing URL must not contain control characters".into(),
            ));
        }
        Some(trimmed.to_owned())
    };
    store.set_landing(id.as_i64().unwrap_or(0), landing.as_deref())?;
    store.commit()?;
    Ok(json!({
        "format": "spiderhead.semantics.landing-update.v1",
        "cluster_id": id,
        "landing_url": landing,
        "audit_recorded": true,
    }))
}
