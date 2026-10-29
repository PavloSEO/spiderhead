//! Cohort and coverage projections: cohort rows, quadrant candidates and coverage rows.

use std::collections::HashMap;

use rusqlite::Connection;
use serde_json::{json, Value};
use unicode_casefold::UnicodeCaseFold;

use crate::bi::compatibility::{evidence_compatibility, NORMALIZED_FORMAT};
use crate::bi::project::{page_row, provider_collection, provider_observations, Observation};
use crate::bi::schema::{
    canonical_json, coverage_row, digest_parts, page_key, page_url, BIExportError, ProviderInput,
    RunInput, MAX_CELL_BYTES,
};
use crate::bi_index::{InlinkIndex, QuadrantHit, QuadrantLookup};
use crate::py;

fn err(msg: impl Into<String>) -> BIExportError {
    BIExportError::new(msg)
}

/// One projected `cohorts` row. Fields are named after the output columns.
/// Numbers stay `Value` so ints and floats keep their original JSON type
/// (an integer stays `3`, never `3.0`).
#[allow(clippy::too_many_arguments)]
pub struct CohortRow<'a> {
    /// `membership` — `member`/`unclassified`.
    pub membership: &'a str,
    /// Row availability `state`.
    pub state: std::borrow::Cow<'a, str>,
    /// `reason`.
    pub reason: Option<String>,
    /// `value_label`.
    pub value_label: Option<String>,
    /// `value_number` — int or float as supplied.
    pub value_number: Option<Value>,
    /// `threshold`.
    pub threshold: Option<Value>,
    /// `numerator`.
    pub numerator: Option<i64>,
    /// `denominator`.
    pub denominator: Option<i64>,
    /// `extraction_coverage_state` (default `not_applicable`).
    pub extraction_state: &'a str,
    /// `extraction_coverage_reason`.
    pub extraction_reason: Option<String>,
    /// `search_metric`.
    pub search_metric: Option<&'a str>,
    /// `search_value` — provider value unchanged.
    pub search_value: Option<Value>,
    /// `search_value_state` (default `not_configured`).
    pub search_state: &'a str,
    /// `sessions_value` — provider value unchanged.
    pub sessions_value: Option<Value>,
    /// `sessions_value_state` (default `not_configured`).
    pub sessions_state: &'a str,
    /// `period_start`.
    pub period_start: Option<Value>,
    /// `period_end`.
    pub period_end: Option<Value>,
    /// `timezone`.
    pub timezone: Option<Value>,
    /// `source_metric_observations_json`.
    pub source_observations: Vec<Value>,
}

impl<'a> Default for CohortRow<'a> {
    fn default() -> Self {
        CohortRow {
            membership: "unclassified",
            state: std::borrow::Cow::Borrowed("unavailable"),
            reason: None,
            value_label: None,
            value_number: None,
            threshold: None,
            numerator: None,
            denominator: None,
            extraction_state: "not_applicable",
            extraction_reason: None,
            search_metric: None,
            search_value: None,
            search_state: "not_configured",
            sessions_value: None,
            sessions_state: "not_configured",
            period_start: None,
            period_end: None,
            timezone: None,
            source_observations: Vec::new(),
        }
    }
}

/// Build one cohort row from a run, page, ordinal, cohort id and definition.
pub fn cohort_row(
    run: &RunInput,
    page: &Value,
    ordinal: usize,
    cohort_id: &str,
    definition: &str,
    fields: CohortRow<'_>,
) -> Value {
    let url = page_url(page);
    let (key, key_state, key_reason) = page_key(url.as_deref());
    json!({
        "run_id": run.run_id,
        "cohort_observation_id": &digest_parts(&[&run.run_id, &ordinal.to_string(), cohort_id])[..32],
        "cohort_id": cohort_id,
        "definition_version": "spiderhead.bi.cohort.v1",
        "definition": definition,
        "url": url,
        "url_key": key,
        "url_key_state": key_state,
        "url_key_reason": key_reason,
        "membership": fields.membership,
        "state": fields.state.as_ref(),
        "reason": fields.reason,
        "value_label": fields.value_label,
        "value_number": fields.value_number,
        "threshold": fields.threshold,
        "numerator": fields.numerator,
        "denominator": fields.denominator,
        "extraction_coverage_state": fields.extraction_state,
        "extraction_coverage_reason": fields.extraction_reason,
        "search_metric": fields.search_metric,
        "search_value": fields.search_value,
        "search_value_state": fields.search_state,
        "sessions_value": fields.sessions_value,
        "sessions_value_state": fields.sessions_state,
        "period_start": fields.period_start,
        "period_end": fields.period_end,
        "timezone": fields.timezone,
        "source_metric_observations_json": fields.source_observations,
    })
}

/// Only one-to-one, complete, same-window search/session pairs are returned.
/// Errors map to `BIExportError`; SQLite work stays in the shared bounded projection index.
pub fn quadrant_candidates<'a>(
    sources: &'a [(&'a ProviderInput, &'a Vec<Observation>, &'a Value)],
    search_metric: Option<&str>,
    con: &'a Connection,
) -> Result<(QuadrantLookup<'a>, Option<String>, QuadrantLookup<'a>), BIExportError> {
    if !["clicks", "impressions"].contains(&search_metric.unwrap_or("")) {
        return Ok((
            QuadrantLookup::new(con, false),
            Some("choose search_metric 'clicks' or 'impressions' to enable quadrants".into()),
            QuadrantLookup::new(con, true),
        ));
    }
    let search_metric = search_metric.unwrap_or("clicks");
    con.execute_batch(
        "CREATE TABLE IF NOT EXISTS candidates (url TEXT, start TEXT, end TEXT, timezone TEXT, \
         search_count INTEGER DEFAULT 0, sessions_count INTEGER DEFAULT 0, \
         search_json TEXT, sessions_json TEXT, PRIMARY KEY(url,start,end,timezone));\
         CREATE TABLE IF NOT EXISTS blocked (url TEXT PRIMARY KEY, reason TEXT);",
    )
    .map_err(crate::bi_index::map_sqlite)?;

    let block = |key: &str, reason: &str| -> Result<(), BIExportError> {
        con.execute(
            "INSERT OR IGNORE INTO blocked VALUES (?,?)",
            rusqlite::params![key, reason],
        )
        .map(|_| ())
        .map_err(crate::bi_index::map_sqlite)
    };

    let mut source_contexts: HashMap<String, Value> = HashMap::new();
    for (provider, observations, info) in sources {
        let header = info.get("evidence").cloned().unwrap_or(Value::Null);
        let provider_name: String = py::str_value(header.get("provider").unwrap_or(&Value::Null))
            .case_fold()
            .collect();
        let collection = provider_collection(&header)?;
        let period = header
            .get("period")
            .filter(|p| py::truthy(Some(p)))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let timezone = header.get("timezone").cloned().unwrap_or(Value::Null);
        if ![
            "gsc",
            "google_search_console",
            "search_console",
            "ga4",
            "google_analytics_4",
            "google_analytics",
        ]
        .contains(&provider_name.as_str())
        {
            continue;
        }
        let origins = header
            .get("field_origins")
            .filter(|o| py::truthy(Some(o)))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut fields = serde_json::Map::new();
        for (name, value) in py::map(&header).cloned().unwrap_or_default() {
            let declared = ["declared", "envelope"]
                .contains(&origins.get(&name).and_then(Value::as_str).unwrap_or(""));
            fields.insert(
                name,
                json!({"value": if declared { value } else { Value::Null }}),
            );
        }
        source_contexts.insert(
            provider.source_id.clone(),
            json!({
                "format": NORMALIZED_FORMAT,
                "mapping": {
                    "source": header,
                    "period": period,
                    "collection": collection,
                    "dimensions": header.get("dimensions").cloned().unwrap_or_else(|| json!([])),
                    "metrics": header.get("metrics").cloned().unwrap_or_else(|| json!([])),
                },
                "provenance": {"fields": fields},
                "rows": [],
            }),
        );
        let mut collection_reason: Option<String> = None;
        if collection.get("state").and_then(Value::as_str) != Some("complete") {
            collection_reason = Some(
                collection
                    .get("reason")
                    .and_then(Value::as_str)
                    .filter(|r| !r.is_empty())
                    .unwrap_or("provider collection is not complete")
                    .to_owned(),
            );
        }
        for observation in observations.iter() {
            let row = &observation.row;
            let url = &observation.url;
            let entry = &observation.entry;
            let metric = &observation.metric;
            if observation.population != "matched"
                || url.get("state").and_then(Value::as_str) != Some("keyed")
                || py::truthy(row.get("dimensions"))
            {
                continue;
            }
            let Some(key) = url.get("normalized").and_then(Value::as_str) else {
                continue;
            };
            let matched_page_count = observation.matched_page_count;
            if matched_page_count < 1 {
                return Err(err(
                    "matched provider observation has an invalid page-match count",
                ));
            }
            if matched_page_count != 1 {
                block(
                    key,
                    "normalized URL key matches multiple retained crawl URLs; \
                     provider traffic cannot be attributed to one URL observation",
                )?;
                continue;
            }
            if py::truthy(row.get("ambiguous")) {
                block(
                    key,
                    "provider source marks this normalized URL row as ambiguous",
                )?;
                continue;
            }
            let axis = if ["gsc", "google_search_console", "search_console"]
                .contains(&provider_name.as_str())
                && metric.get("name").and_then(Value::as_str) == Some(search_metric)
            {
                "search"
            } else if ["ga4", "google_analytics_4", "google_analytics"]
                .contains(&provider_name.as_str())
                && metric.get("name").and_then(Value::as_str) == Some("sessions")
            {
                "sessions"
            } else {
                continue;
            };
            if let Some(reason) = &collection_reason {
                block(key, reason)?;
                continue;
            }
            if entry.get("state").and_then(Value::as_str) != Some("measured") {
                block(key, "provider did not measure the selected metric")?;
                continue;
            }
            let unit = metric
                .get("unit")
                .filter(|u| py::truthy(Some(u)))
                .or_else(|| entry.get("unit"))
                .cloned()
                .unwrap_or(Value::Null);
            if unit != json!("count") {
                block(
                    key,
                    "search clicks/impressions and sessions require declared count units",
                )?;
                continue;
            }
            if entry.get("value").and_then(Value::as_f64).unwrap_or(-1.0) < 0.0 {
                block(
                    key,
                    "search clicks/impressions and sessions must be nonnegative counts",
                )?;
                continue;
            }
            if !timezone.is_string()
                || !period.get("start_date").is_some_and(Value::is_string)
                || !period.get("end_date").is_some_and(Value::is_string)
            {
                block(key, "provider period and timezone must be established")?;
                continue;
            }
            let source = json!({
                "provider_source_id": provider.source_id,
                "provider": header.get("provider"),
                "operation": header.get("operation"),
                "reporting_identity": header.get("reporting_identity"),
                "metric": metric.get("name"),
                "metric_unit": unit,
                "source_row_index": row.get("row_index"),
                "natural_key_sha256": row.get("natural_key_sha256"),
                "value": entry.get("value"),
                "period_start": period["start_date"],
                "period_end": period["end_date"],
                "timezone": timezone,
                "attribution": header.get("attribution"),
                "search_engine": header.get("search_engine"),
                "search_type": header.get("search_type"),
                "collection": collection,
                "field_origins": origins,
                "boundary_policy": "strict",
                "cross_source_policy": "juxtapose",
            });
            let encoded = canonical_json(&source).map_err(|e| err(e.0))?;
            if encoded.len() > MAX_CELL_BYTES as usize {
                return Err(err("one quadrant observation exceeds the BI cell bound"));
            }
            con.execute(
                &format!(
                    "INSERT INTO candidates(url,start,end,timezone,{axis}_count,{axis}_json) \
                     VALUES (?,?,?,?,1,?) ON CONFLICT(url,start,end,timezone) DO UPDATE SET \
                     {axis}_count={axis}_count+1, \
                     {axis}_json=COALESCE({axis}_json,excluded.{axis}_json)"
                ),
                rusqlite::params![
                    key,
                    period
                        .get("start_date")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    period.get("end_date").and_then(Value::as_str).unwrap_or(""),
                    timezone.as_str().unwrap_or(""),
                    encoded
                ],
            )
            .map_err(crate::bi_index::map_sqlite)?;
        }
    }
    con.execute(
        "INSERT OR IGNORE INTO blocked SELECT url,? FROM candidates \
         WHERE search_count>1 OR sessions_count>1",
        rusqlite::params![
            "more than one complete provider observation shares this normalized URL key and period"
        ],
    )
    .map_err(crate::bi_index::map_sqlite)?;
    con.execute(
        "INSERT OR IGNORE INTO blocked SELECT url,? FROM candidates \
         WHERE search_count=1 AND sessions_count=1 GROUP BY url HAVING COUNT(*)>1",
        rusqlite::params![
            "more than one compatible provider period is retained for this normalized URL key"
        ],
    )
    .map_err(crate::bi_index::map_sqlite)?;
    let mut compatibility_cache: HashMap<(String, String), Value> = HashMap::new();
    let pairs: Vec<(String, String, String)> = {
        let mut stmt = con
            .prepare(
                "SELECT url,search_json,sessions_json FROM candidates \
                 WHERE search_count=1 AND sessions_count=1",
            )
            .map_err(crate::bi_index::map_sqlite)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(crate::bi_index::map_sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(crate::bi_index::map_sqlite)?
    };
    for (key, search_json, sessions_json) in pairs {
        let pair_ids = (
            serde_json::from_str::<Value>(&search_json)
                .ok()
                .and_then(|v| {
                    v.get("provider_source_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default(),
            serde_json::from_str::<Value>(&sessions_json)
                .ok()
                .and_then(|v| {
                    v.get("provider_source_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default(),
        );
        if !compatibility_cache.contains_key(&pair_ids) {
            let (Some(left_doc), Some(right_doc)) = (
                source_contexts.get(&pair_ids.0),
                source_contexts.get(&pair_ids.1),
            ) else {
                continue;
            };
            let decision = evidence_compatibility(
                left_doc,
                right_doc,
                &json!({
                    "boundary_policy": "strict",
                    "cross_source": "juxtapose",
                    "quadrant": {"left_metric": search_metric, "right_metric": "sessions"},
                }),
            )?;
            compatibility_cache.insert(pair_ids.clone(), decision);
        }
        let decision = compatibility_cache
            .get(&pair_ids)
            .cloned()
            .unwrap_or(Value::Null);
        if !py::truthy(decision.get("quadrant").and_then(|q| q.get("eligible"))) {
            let mut block_reasons: Vec<String> = decision
                .get("quadrant")
                .and_then(|q| q.get("reasons"))
                .and_then(Value::as_array)
                .map(|r| {
                    r.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            for aspect in decision
                .get("reasons")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                if py::truthy(aspect.get("unverified"))
                    || aspect.get("verdict").and_then(Value::as_str) != Some("compatible")
                {
                    block_reasons.push(
                        aspect
                            .get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    );
                }
            }
            block(
                &key,
                &format!(
                    "provider scope is not qualified: {}",
                    block_reasons.join(", ")
                ),
            )?;
        }
    }
    Ok((
        QuadrantLookup::new(con, false),
        None,
        QuadrantLookup::new(con, true),
    ))
}

/// Transparent technical cohorts plus provider quadrants.
pub fn cohort_rows<'a>(
    run: &'a mut RunInput,
    sources: &'a [(&'a ProviderInput, &'a Vec<Observation>, &'a Value)],
    search_metric: Option<&'a str>,
    index_con: Option<&'a Connection>,
    inlink_index: Option<&'a InlinkIndex<'a>>,
) -> CohortIter<'a> {
    let definition_status =
        "Observed HTTP status group for this retained crawl URL; no Google-crawl claim.";
    let definition_indexability =
        "Captured indexability/directive state from the selected retained run.";
    let definition_depth = "Crawl-relative depth band; deep means retained crawl depth >= 3.";
    let definition_inlinks = "Observed eligible-page share: numerator is unique retained in-scope sources linking to this target; denominator is in-scope pages with completed HTML link extraction. Repeated occurrences count once per source. Partial scope is not the whole site.";
    let definition_quadrant = "Search Console {metric} and GA sessions are separate axes. A quadrant needs one measured dimensionless URL value from each complete source for the same inclusive local-date window and timezone, with explicit unsampled/unthresholded/untruncated collection and known attribution. Nonnegative counts and source scopes remain separately labelled under strict calendar and cross-source juxtaposition policies; values are never summed.";
    let (pairs, pair_reason, blocked_keys) = if search_metric.is_none() {
        (
            None,
            Some("choose search_metric 'clicks' or 'impressions' to enable quadrants".to_owned()),
            None,
        )
    } else {
        match index_con {
            None => {
                return CohortIter::failed(err(
                    "provider quadrant projection requires a bounded disk index",
                ))
            }
            Some(con) => match quadrant_candidates(sources, search_metric, con) {
                Ok((p, r, b)) => (Some(p), r, Some(b)),
                Err(e) => return CohortIter::failed(e),
            },
        }
    };
    CohortIter {
        run: Some(run),
        pages: None,
        ordinal: 0,
        pairs,
        pair_reason,
        blocked_keys,
        inlink_index,
        search_metric,
        definitions: [
            definition_status,
            definition_indexability,
            definition_depth,
            definition_inlinks,
            definition_quadrant,
        ],
        queue: std::collections::VecDeque::new(),
    }
}

/// Pull iterator over cohort rows, five per page, evaluated one page at a time.
pub struct CohortIter<'a> {
    run: Option<&'a mut RunInput>,
    pages: Option<Box<dyn Iterator<Item = Value> + Send>>,
    ordinal: usize,
    pairs: Option<QuadrantLookup<'a>>,
    pair_reason: Option<String>,
    blocked_keys: Option<QuadrantLookup<'a>>,
    inlink_index: Option<&'a InlinkIndex<'a>>,
    search_metric: Option<&'a str>,
    definitions: [&'static str; 5],
    queue: std::collections::VecDeque<Result<Value, BIExportError>>,
}

impl<'a> CohortIter<'a> {
    fn failed(error: BIExportError) -> Self {
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(Err(error));
        CohortIter {
            run: None,
            pages: None,
            ordinal: 0,
            pairs: None,
            pair_reason: None,
            blocked_keys: None,
            inlink_index: None,
            search_metric: None,
            definitions: ["", "", "", "", ""],
            queue,
        }
    }

    fn fill(&mut self, page: &Value, ordinal: usize) {
        let Some(run) = self.run.as_deref() else {
            return;
        };
        let [def_status, def_index, def_depth, def_inlinks, def_quadrant] = self.definitions;
        let projected = match page_row(run, page, ordinal) {
            Ok(p) => p,
            Err(e) => {
                self.queue.push_back(Err(e));
                return;
            }
        };
        let status = projected.get("status_code").cloned().unwrap_or(Value::Null);
        if projected.get("status_code_state").and_then(Value::as_str) == Some("measured") {
            let label = if status.as_i64().is_some_and(|s| (200..300).contains(&s)) {
                "successful_html"
            } else if status.as_i64().is_some_and(|s| (300..400).contains(&s)) {
                "redirect"
            } else if status.as_i64().is_some_and(|s| (400..500).contains(&s)) {
                "client_error"
            } else if status.as_i64().is_some_and(|s| (500..600).contains(&s)) {
                "server_error"
            } else {
                "other_status"
            };
            self.queue.push_back(Ok(cohort_row(
                run,
                page,
                ordinal,
                "observed_status",
                def_status,
                CohortRow {
                    membership: "member",
                    state: "available".into(),
                    value_label: Some(label.into()),
                    value_number: Some(status.clone()),
                    ..Default::default()
                },
            )));
        } else {
            self.queue.push_back(Ok(cohort_row(
                run,
                page,
                ordinal,
                "observed_status",
                def_status,
                CohortRow {
                    membership: "unclassified",
                    state: "unavailable".into(),
                    reason: projected
                        .get("status_code_reason")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    ..Default::default()
                },
            )));
        }
        let indexability = projected
            .get("indexability")
            .cloned()
            .unwrap_or(Value::Null);
        let index_state = projected
            .get("indexability_state")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        self.queue.push_back(Ok(cohort_row(
            run,
            page,
            ordinal,
            "captured_indexability",
            def_index,
            CohortRow {
                membership: if index_state == "measured" {
                    "member"
                } else {
                    "unclassified"
                },
                state: if index_state == "measured" {
                    "available".into()
                } else {
                    index_state.clone().into()
                },
                reason: projected
                    .get("indexability_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                value_label: if indexability.is_null() {
                    None
                } else {
                    Some(py::str_value(&indexability))
                },
                ..Default::default()
            },
        )));
        let depth = projected.get("crawl_depth").cloned().unwrap_or(Value::Null);
        if projected.get("crawl_depth_state").and_then(Value::as_str) == Some("measured") {
            self.queue.push_back(Ok(cohort_row(
                run,
                page,
                ordinal,
                "crawl_relative_depth",
                def_depth,
                CohortRow {
                    membership: "member",
                    state: "available".into(),
                    value_label: Some(
                        if depth.as_i64().unwrap_or(0) >= 3 {
                            "deep"
                        } else {
                            "shallow"
                        }
                        .into(),
                    ),
                    value_number: Some(depth.clone()),
                    threshold: Some(json!(3)),
                    ..Default::default()
                },
            )));
        } else {
            self.queue.push_back(Ok(cohort_row(
                run,
                page,
                ordinal,
                "crawl_relative_depth",
                def_depth,
                CohortRow {
                    membership: "unclassified",
                    state: "unavailable".into(),
                    reason: projected
                        .get("crawl_depth_reason")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    threshold: Some(json!(3)),
                    ..Default::default()
                },
            )));
        }
        if let Some(inlink) = self.inlink_index {
            if inlink.denominator != 0 {
                let numerator = inlink
                    .numerator(py::get_str(&projected, "url").as_str())
                    .unwrap_or(0);
                let denominator = inlink.denominator;
                let complete = denominator == run.page_count as i64
                    && run.run_metadata.get("crawl_state").and_then(Value::as_str)
                        == Some("complete");
                self.queue.push_back(Ok(cohort_row(
                    run,
                    page,
                    ordinal,
                    "observed_unique_inlink_share",
                    def_inlinks,
                    CohortRow {
                        membership: "member",
                        state: if complete {
                            "available".into()
                        } else {
                            "partial".into()
                        },
                        reason: if complete {
                            None
                        } else {
                            Some(
                                "share of observed eligible pages; crawl or extraction coverage is partial"
                                    .into(),
                            )
                        },
                        value_number: Some(json!(numerator as f64 / denominator as f64)),
                        numerator: Some(numerator),
                        denominator: Some(denominator),
                        extraction_state: if denominator == run.page_count as i64 {
                            "complete"
                        } else {
                            "partial"
                        },
                        extraction_reason: if denominator == run.page_count as i64 {
                            None
                        } else {
                            Some("failed, skipped or non-HTML sources excluded".into())
                        },
                        ..Default::default()
                    },
                )));
            } else {
                self.queue.push_back(Ok(cohort_row(
                    run,
                    page,
                    ordinal,
                    "observed_unique_inlink_share",
                    def_inlinks,
                    CohortRow {
                        membership: "unclassified",
                        state: "unavailable".into(),
                        reason: Some(
                            "retained occurrence evidence and completed source extraction are required"
                                .into(),
                        ),
                        extraction_state: "unavailable",
                        ..Default::default()
                    },
                )));
            }
        } else {
            self.queue.push_back(Ok(cohort_row(
                run,
                page,
                ordinal,
                "observed_unique_inlink_share",
                def_inlinks,
                CohortRow {
                    membership: "unclassified",
                    state: "unavailable".into(),
                    reason: Some(
                        "retained occurrence evidence and completed source extraction are required"
                            .into(),
                    ),
                    extraction_state: "unavailable",
                    ..Default::default()
                },
            )));
        }
        let key = projected.get("url_key").cloned().unwrap_or(Value::Null);
        let pair = key.as_str().and_then(|k| {
            self.pairs
                .as_ref()
                .and_then(|p| p.get(k).ok())
                .and_then(|hit| match hit {
                    QuadrantHit::Pair(s, t) => Some((s, t)),
                    _ => None,
                })
        });
        let blocked_reason = key.as_str().and_then(|k| {
            self.blocked_keys
                .as_ref()
                .and_then(|b| b.get(k).ok())
                .and_then(|hit| match hit {
                    QuadrantHit::Reason(r) => Some(r),
                    _ => None,
                })
        });
        if let Some((search, sessions)) = pair {
            let search_value = search.get("value").cloned().unwrap_or(Value::Null);
            let sessions_value = sessions.get("value").cloned().unwrap_or(Value::Null);
            let search_num = search_value.as_f64().unwrap_or(0.0);
            let sessions_num = sessions_value.as_f64().unwrap_or(0.0);
            let label = if search_num > 0.0 && sessions_num > 0.0 {
                "positive_search_positive_sessions"
            } else if search_num > 0.0 {
                "positive_search_zero_sessions"
            } else if sessions_num > 0.0 {
                "zero_search_positive_sessions"
            } else {
                "zero_search_zero_sessions"
            };
            self.queue.push_back(Ok(cohort_row(
                run,
                page,
                ordinal,
                "search_visibility_vs_sessions",
                &def_quadrant.replace("{metric}", self.search_metric.unwrap_or("")),
                CohortRow {
                    membership: "member",
                    state: "available".into(),
                    value_label: Some(label.into()),
                    search_metric: self.search_metric,
                    search_value: Some(search_value),
                    search_state: "measured",
                    sessions_value: Some(sessions_value),
                    sessions_state: "measured",
                    period_start: search.get("period_start").cloned(),
                    period_end: search.get("period_end").cloned(),
                    timezone: search.get("timezone").cloned(),
                    source_observations: vec![search, sessions],
                    ..Default::default()
                },
            )));
        } else {
            self.queue.push_back(Ok(cohort_row(
                run,
                page,
                ordinal,
                "search_visibility_vs_sessions",
                &def_quadrant.replace(
                    "{metric}",
                    self.search_metric
                        .unwrap_or("selected Search Console metric"),
                ),
                CohortRow {
                    membership: "unclassified",
                    state: if self.pair_reason.is_some() {
                        "not_configured".into()
                    } else {
                        "incomplete".into()
                    },
                    reason: Some(self.pair_reason.clone().or(blocked_reason).unwrap_or_else(
                        || "no one-to-one complete compatible URL metric pair is retained".into(),
                    )),
                    search_metric: self.search_metric,
                    search_state: if self.pair_reason.is_some() {
                        "not_configured"
                    } else {
                        "unavailable"
                    },
                    sessions_state: if self.pair_reason.is_some() {
                        "not_configured"
                    } else {
                        "unavailable"
                    },
                    ..Default::default()
                },
            )));
        }
    }
}

impl<'a> Iterator for CohortIter<'a> {
    type Item = Result<Value, BIExportError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.queue.pop_front() {
                return Some(item);
            }
            if self.pages.is_none() {
                let run = self.run.as_deref_mut()?;
                self.pages = Some((run.pages_factory)());
            }
            let pages = self.pages.as_mut()?;
            let page = pages.next()?;
            let ordinal = self.ordinal;
            self.ordinal += 1;
            self.fill(&page, ordinal);
        }
    }
}

pub fn coverage_rows(
    run: &RunInput,
    providers: &[ProviderInput],
    dataset_results: &HashMap<String, Value>,
    group_index: &crate::bi_index::GroupIndex<'_>,
) -> Result<Vec<Value>, BIExportError> {
    let mut out = Vec::new();
    let get = |name: &str| {
        dataset_results
            .get(name)
            .cloned()
            .unwrap_or_else(|| json!({}))
    };
    let int_opt = |v: Option<i64>| v.map(|n| json!(n));
    let pages_result = get("pages");
    let findings_result = get("findings");
    let links_result = get("link_occurrences");
    let metrics_result = get("metrics");
    let cohorts_result = get("cohorts");
    let crawl_state_v = run
        .run_metadata
        .get("crawl_state")
        .cloned()
        .unwrap_or(Value::Null);
    let crawl_state = crawl_state_v.as_str().unwrap_or("");
    let page_count_v = json!(run.page_count);
    let unavail = if crawl_state == "complete" {
        json!(0)
    } else {
        page_count_v.clone()
    };
    out.push(coverage_row(
        &run.run_id,
        "run",
        &run.run_id,
        "crawl",
        crawl_state,
        run.run_metadata.get("crawl_reason"),
        Some(&page_count_v),
        Some(&page_count_v),
        Some(&unavail),
        Some(&json!({"source_kind": run.source_kind, "source_schema": run.source_schema})),
    ));
    for (dataset, result, source_count) in [
        ("pages", pages_result.clone(), Some(run.page_count as i64)),
        (
            "findings",
            findings_result.clone(),
            Some(run.finding_count as i64),
        ),
        (
            "link_occurrences",
            links_result.clone(),
            run.link_count.map(|c| c as i64),
        ),
        ("metrics", metrics_result.clone(), None),
        (
            "cohorts",
            cohorts_result.clone(),
            Some(run.page_count as i64 * 5),
        ),
    ] {
        let source_v = source_count.map(|n| json!(n));
        let coverage_v = result.get("coverage").cloned().unwrap_or_else(|| json!({}));
        out.push(coverage_row(
            &run.run_id,
            dataset,
            &run.run_id,
            "dataset",
            result.get("state").and_then(Value::as_str).unwrap_or(""),
            result.get("reason"),
            source_v.as_ref(),
            result.get("row_count"),
            result.get("unavailable_rows"),
            Some(&coverage_v),
        ));
    }
    out.extend(run.coverage_rows.iter().cloned());
    if group_index.source_count > 0 {
        let (total, linked) = group_index.coverage()?;
        let reason_v = if total != linked {
            json!("some source groups are not referenced by a finding row")
        } else {
            Value::Null
        };
        out.push(coverage_row(
            &run.run_id,
            "findings",
            "groups",
            "finding_groups",
            if total != linked {
                "partial"
            } else {
                "represented"
            },
            if total != linked {
                Some(&reason_v)
            } else {
                None
            },
            Some(&json!(group_index.source_count)),
            Some(&json!(linked)),
            Some(&json!(total - linked)),
            Some(
                &json!({"unlinked_ids": "one exact finding_group row per unlinked group follows"}),
            ),
        ));
        for group_id in group_index.unlinked()? {
            let reason_v = json!("source group is not referenced by a finding row");
            out.push(coverage_row(
                &run.run_id,
                "findings",
                &group_id,
                "finding_group",
                "unrepresented",
                Some(&reason_v),
                Some(&json!(1)),
                Some(&json!(0)),
                Some(&json!(1)),
                Some(&json!({"group_id": group_id})),
            ));
        }
    }
    if providers.is_empty() {
        let reason_v = json!("no provider join artifact was supplied");
        out.push(coverage_row(
            &run.run_id,
            "metrics",
            "provider_sources",
            "provider_source",
            "not_configured",
            Some(&reason_v),
            Some(&json!(0)),
            Some(&json!(0)),
            None,
            None,
        ));
        return Ok(out);
    }
    for provider in providers {
        if provider.error_state.is_some() {
            let error_reason = provider.error_reason.as_ref().map(|r| json!(r));
            out.push(coverage_row(
                &run.run_id,
                "metrics",
                &provider.source_id,
                "provider_source",
                provider.error_state.as_deref().unwrap_or(""),
                error_reason.as_ref(),
                int_opt(provider.reported_rows).as_ref(),
                Some(&json!(0)),
                int_opt(provider.reported_rows).as_ref(),
                Some(&json!({
                    "file": provider.name,
                    "sha256": provider.sha256,
                    "bytes": provider.byte_count,
                })),
            ));
            continue;
        }
        let Some(join) = &provider.join else {
            let reason_v = json!("normalized evidence was accepted but no crawl join was produced");
            out.push(coverage_row(
                &run.run_id,
                "metrics",
                &provider.source_id,
                "provider_source",
                "not_joined",
                Some(&reason_v),
                None,
                Some(&json!(0)),
                None,
                Some(&json!({"file": provider.name, "sha256": provider.sha256})),
            ));
            continue;
        };
        let summary = join.get("summary").cloned().unwrap_or_else(|| json!({}));
        let evidence = join.get("evidence").cloned().unwrap_or_else(|| json!({}));
        let collection = provider_collection(&evidence)?;
        let collection_state_v = collection.get("state").cloned().unwrap_or(Value::Null);
        let collection_state = collection_state_v.as_str().unwrap_or("");
        let collection_reason = collection.get("reason").filter(|r| py::truthy(Some(r)));
        for (population, key, state, reason) in [
            ("matched", "matched_rows", "measured", None),
            (
                "crawl_only",
                "crawl_only",
                "unavailable",
                Some("no provider row matched this crawl URL"),
            ),
            ("external_only", "external_only", "measured", None),
            (
                "unkeyable_rows",
                "unkeyable_rows",
                "unavailable",
                Some("provider row has no usable URL key"),
            ),
            (
                "unkeyable_pages",
                "unkeyable_pages",
                "unavailable",
                Some("crawl URL has no usable join key"),
            ),
        ] {
            let count = summary
                .get(key)
                .filter(|c| py::is_int(c))
                .and_then(py::to_i64)
                .unwrap_or(0);
            let count_v = json!(count);
            let reason_v = collection_reason
                .cloned()
                .or_else(|| reason.map(str::to_owned).map(Value::String))
                .unwrap_or(Value::Null);
            out.push(coverage_row(
                &run.run_id,
                "metrics",
                &provider.source_id,
                population,
                if collection_state != "complete" {
                    collection_state
                } else {
                    state
                },
                if reason_v.is_null() {
                    None
                } else {
                    Some(&reason_v)
                },
                Some(&count_v),
                Some(&count_v),
                Some(&json!(if state == "unavailable" { count } else { 0 })),
                Some(&json!({
                    "provider": evidence.get("provider"),
                    "operation": evidence.get("operation"),
                })),
            ));
        }
        let (observations, _info) = provider_observations(provider, join)?;
        let unavailable_metrics = observations
            .iter()
            .filter(|o| o.entry.get("state").and_then(Value::as_str) != Some("measured"))
            .count() as i64;
        out.push(coverage_row(
            &run.run_id,
            "metrics",
            &provider.source_id,
            "metric_observations",
            collection_state,
            collection.get("reason"),
            Some(&json!(observations.len())),
            Some(&json!(observations.len())),
            Some(&json!(unavailable_metrics)),
            Some(&json!({
                "provider": evidence.get("provider"),
                "operation": evidence.get("operation"),
                "metrics": evidence
                    .get("metrics")
                    .and_then(Value::as_array)
                    .map(|m| m.iter().map(|x| x.get("name").cloned().unwrap_or(Value::Null)).collect::<Vec<_>>())
                    .unwrap_or_default(),
                "zero_is_measured": true,
            })),
        ));
    }
    Ok(out)
}
