//! Bot and human traffic summaries (GraphQL aggregation) and the bounded Enterprise Logpull import.

use std::path::PathBuf;

use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use spiderhead_core::host::Host;

use crate::cloudflare::{
    errors_or_attr, CfError, CfTransport, LiveSend, RawReply, RawTransport, API, GRAPHQL, MAX_DAYS,
    MAX_RESPONSE_BYTES, MAX_TRACKED_PATHS, MAX_URL_FACTS, NOTE, PAGE_SIZE, QUERY, SOURCE,
};
use crate::http::{self, Request, Sleeper, Transport};
use crate::{pyjson, pystr, pytime};

mod bot_patterns;
use bot_patterns::*;
mod detect_and_count;
pub use detect_and_count::*;
mod window_fetch;
pub(crate) use window_fetch::*;
mod findings;
use findings::*;
mod logpull;
pub use logpull::*;

/// Binding list for a project.
pub type BindingsLookup<'a> = dyn Fn(&str) -> Result<Value, CfError> + Send + Sync + 'a;
/// Stores a traffic result as a history snapshot.
pub type HistorySink<'a> = dyn Fn(&Value, &str) -> Result<Value, CfError> + Send + Sync + 'a;
/// Project root lookup; returns the project path.
pub type ProjectOpener<'a> = dyn Fn(&str) -> Result<Value, CfError> + Send + Sync + 'a;
/// Logstore step that imports a temporary file into a project.
pub type LogImport<'a> = dyn Fn(&str, &str, i64) -> Result<Value, CfError> + Send + Sync + 'a;

/// The `traffic` seams that touch project state, history, spend and the wall clock.
pub struct TrafficEnv<'a> {
    /// Env-var/filesystem host for `read_token`.
    pub host: &'a dyn Host,
    /// `~` expansion for the default token path.
    pub home: &'a dyn Fn(&str) -> PathBuf,
    /// Binding list for a project.
    pub bindings: Option<&'a BindingsLookup<'a>>,
    /// Stores a traffic result as a history snapshot.
    pub history: Option<&'a HistorySink<'a>>,
    /// Spend-ledger sink; records the paid operation.
    pub spend: Option<&'a crate::metrika::SpendSink<'a>>,
    /// Override for today's date, as days since the epoch.
    pub today: Option<i64>,
    /// Override for the page size.
    pub page_size: Option<i64>,
    /// `MAX_URL_FACTS` override.
    pub max_url_facts: Option<usize>,
    /// `MAX_TRACKED_PATHS` override.
    pub max_tracked_paths: Option<usize>,
}

struct DaySplit {
    queries: u64,
    truncated_days: Vec<String>,
    sampled: bool,
}

/// Summarise bot and human traffic over an inclusive UTC date range.
#[allow(clippy::too_many_arguments)]
pub async fn traffic<T: Transport>(
    env: &TrafficEnv<'_>,
    transport: &T,
    sleeper: &Sleeper,
    zone: Option<&Value>,
    project: Option<&Value>,
    include_configuration: Option<&Value>,
    since: Option<&Value>,
    until: Option<&Value>,
    send: Option<&CfTransport<'_>>,
    token: Option<&Value>,
) -> Result<Value, CfError> {
    let zone_given = pystr::truthy(zone);
    let project_given = pystr::truthy(project);
    if zone_given == project_given {
        return Err(CfError::Value(
            "pass exactly one of zone or project".to_owned(),
        ));
    }
    let include_cfg = include_configuration.unwrap_or(&Value::Bool(false));
    if !include_cfg.is_boolean() {
        return Err(CfError::Value(
            "include_configuration must be a boolean".to_owned(),
        ));
    }
    let include_cfg = include_cfg.as_bool().unwrap_or(false);
    // Keep the raw zone argument (null on the project path) until the bound zone id replaces it.
    let zone_s = if zone_given {
        pystr::repr_value(zone.unwrap_or(&Value::Null))
    } else {
        String::new()
    };
    let project_s = if project_given {
        pystr::repr_value(project.unwrap_or(&Value::Null))
    } else {
        String::new()
    };
    let mut requested_zone = if zone_given {
        zone.cloned().unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let today = env
        .today
        .unwrap_or_else(|| pytime::now_micros() / 86_400_000_000);
    let end = match until.filter(|v| pystr::truthy(Some(v))) {
        Some(v) => parse_day(v, "until")?,
        None => today,
    };
    let start = match since.filter(|v| pystr::truthy(Some(v))) {
        Some(v) => parse_day(v, "since")?,
        None => end - 6,
    };
    if start > end {
        return Err(CfError::Value(
            "since must not be later than until".to_owned(),
        ));
    }
    if today - start >= MAX_DAYS {
        return Err(CfError::Value(format!(
            "since is older than the {MAX_DAYS}-day history this dataset keeps on the Free plan"
        )));
    }
    if end - start + 1 > MAX_DAYS {
        return Err(CfError::Value(format!(
            "the range may span at most {MAX_DAYS} days"
        )));
    }

    let live = LiveSend { transport, sleeper };
    let secret = match token.filter(|v| pystr::truthy(Some(v))) {
        Some(t) => t
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| pystr::repr_value(t)),
        None => match crate::cloudflare::read_token(env.host, env.home) {
            Ok(t) => t,
            Err(e) => {
                return Ok(
                    json!({"ok": false, "source": SOURCE, "zone": requested_zone, "error": e.to_string()}),
                )
            }
        },
    };
    // Only a `CloudflareError` in the resolve/fetch/aggregate path becomes the `{ok: false}` envelope.
    let info = async {
        if project_given {
            let bindings = match env.bindings {
                Some(b) => b,
                None => {
                    return Err(CfError::Cloudflare(
                        "project bindings are unavailable".to_owned(),
                    ))
                }
            };
            requested_zone = json!(resolve_project_zone(&project_s, bindings)?);
            let zone_id = requested_zone.as_str().unwrap_or_default().to_owned();
            let quoted = http::quote(&zone_id, "");
            let payload = call_send(
                &live,
                send,
                "GET",
                &format!("{API}/zones/{quoted}"),
                None,
                &secret,
            )
            .await?;
            crate::cloudflare::zone_id_lookup(&payload, &zone_id)
        } else {
            let query = http::urlencode(&[("name".to_owned(), zone_s.clone())]);
            let payload = call_send(
                &live,
                send,
                "GET",
                &format!("{API}/zones?{query}"),
                None,
                &secret,
            )
            .await?;
            crate::cloudflare::zone_lookup(&payload, &zone_s)
        }
    };
    let info = match info.await {
        Ok(i) => i,
        Err(e) => {
            return match e {
                CfError::Cloudflare(m) => {
                    Ok(json!({"ok": false, "source": SOURCE, "zone": requested_zone, "error": m}))
                }
                other => Err(other),
            }
        }
    };
    if !pystr::truthy(info.get("account")) {
        return Ok(
            json!({"ok": false, "source": SOURCE, "zone": requested_zone, "error": "the zone has no account id"}),
        );
    }

    // Maps keep first-seen order, because the `*_by_family` order shows up in `findings`
    // and in the `bots` tie-breaks.
    let mut families: IndexMap<String, Counter> = IndexMap::new();
    let mut status: IndexMap<String, Counter> = IndexMap::new();
    let mut sections: IndexMap<String, Counter> = IndexMap::new();
    let mut paths: IndexMap<String, Counter> = IndexMap::new();
    let mut cache: IndexMap<String, Counter> = IndexMap::new();
    let mut edge_vs_origin = Counter::default();
    let mut bot_url_status: IndexMap<String, Counter> = IndexMap::new();
    let mut categories: IndexMap<String, Counter> = IndexMap::new();
    let mut bots_daily: IndexMap<String, Counter> = IndexMap::new();
    let mut hosts = Counter::default();
    let mut daily = Counter::default();
    let mut url_facts: IndexMap<String, (Vec<Value>, i64)> = IndexMap::new();
    let mut url_facts_truncated = false;
    let mut url_facts_omitted = 0i64;
    let mut split = DaySplit {
        queries: 0,
        truncated_days: Vec::new(),
        sampled: false,
    };
    let account = info.get("account").cloned().unwrap_or(Value::Null);
    // The zone id is passed through as the raw value, never forced to a string.
    let zone_id = info.get("id").cloned().unwrap_or(Value::Null);
    let page_size = env.page_size.unwrap_or(PAGE_SIZE);
    let max_url_facts = env.max_url_facts.unwrap_or(MAX_URL_FACTS);
    let max_tracked_paths = env.max_tracked_paths.unwrap_or(MAX_TRACKED_PATHS);

    let mut loop_err: Option<CfError> = None;
    for day in start..=end {
        let frm = midnight_micros(day);
        let to = midnight_micros(day + 1);
        let rows = match fetch_window(
            &live,
            send,
            &secret,
            &account,
            &zone_id,
            frm,
            to,
            &stamp_day(day),
            &mut split,
            page_size,
        )
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                loop_err = Some(e);
                break;
            }
        };
        static EMPTY: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| json!({}));
        for row in &rows {
            // A falsy dimensions value counts as empty; a truthy non-object is an error.
            let dims = py_get_or_obj(row, "dimensions", &EMPTY)?;
            let count = py_int_or_zero(crate::cloudflare::obj_get(row, "count")?)?;
            // Sample interval 1 or missing counts as unsampled; `true` and `1.0` also equal 1.
            let avg = py_get_or_obj(row, "avg", &EMPTY)?;
            let default_interval = json!(1);
            let interval = avg.get("sampleInterval").unwrap_or(&default_interval);
            if !interval_is_default(interval) {
                split.sampled = true;
            }
            let ua = dims
                .get("userAgent")
                .filter(|v| pystr::truthy(Some(v)))
                .cloned()
                .unwrap_or_else(|| json!(""));
            let category = dims
                .get("verifiedBotCategory")
                .filter(|v| pystr::truthy(Some(v)))
                .cloned()
                .unwrap_or_else(|| json!(""));
            let (family, name) = classify(&ua, &category)?;
            let path = dims
                .get("clientRequestPath")
                .filter(|v| pystr::truthy(Some(v)))
                .cloned()
                .unwrap_or_else(|| json!("/"));
            let code = py_int_or_zero(dims.get("edgeResponseStatus"))?;
            let origin = py_int_or_zero(dims.get("originResponseStatus"))?;
            let day_key = dims
                .get("date")
                .filter(|v| pystr::truthy(Some(v)))
                .cloned()
                .unwrap_or_else(|| json!(stamp_day(day)));

            // Preserve the measured edge/origin/cache dimensions at URL grain. This is
            // still grouped analytics, not a request log: no IP or sub-day timestamp exists.
            let host = dims
                .get("clientRequestHTTPHost")
                .filter(|v| pystr::truthy(Some(v)))
                .cloned()
                .unwrap_or_else(|| json!("unknown"));
            let cache_status = dims
                .get("cacheStatus")
                .filter(|v| pystr::truthy(Some(v)))
                .cloned()
                .unwrap_or_else(|| json!("unknown"));
            let fact_key = vec![
                host.clone(),
                path.clone(),
                day_key.clone(),
                json!(family),
                json!(name),
                if pystr::truthy(Some(&category)) {
                    category.clone()
                } else {
                    Value::Null
                },
                json!(code),
                if origin == 0 {
                    Value::Null
                } else {
                    json!(origin)
                },
                cache_status.clone(),
            ];
            // Hashing the fact key fails for unhashable members before the bound check runs.
            let key_text = fact_key
                .iter()
                .map(|member| {
                    py_hash_key(member).map_err(|e| {
                        CfError::Python(
                            "TypeError",
                            format!("cannot use 'tuple' as a dict key ({e})"),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
                .join("\u{2}");
            if let Some(entry) = url_facts.get_mut(&key_text) {
                entry.1 += count;
            } else if url_facts.len() < max_url_facts {
                url_facts.insert(key_text, (fact_key, count));
            } else {
                url_facts_truncated = true;
                url_facts_omitted += 1;
            }

            families
                .entry(family.clone())
                .or_default()
                .add(json!(name), count)?;
            status
                .entry(family.clone())
                .or_default()
                .add(json!(code), count)?;
            sections
                .entry(family.clone())
                .or_default()
                .add(json!(section(&path)?), count)?;
            let tracked = paths.entry(family.clone()).or_default();
            if tracked.contains(&path)? || tracked.len() < max_tracked_paths {
                tracked.add(path.clone(), count)?;
            }
            cache
                .entry(family.clone())
                .or_default()
                .add(cache_status, count)?;
            edge_vs_origin.add(
                json!(format!(
                    "{}/{}",
                    code,
                    if origin == 0 {
                        "none".to_owned()
                    } else {
                        origin.to_string()
                    }
                )),
                count,
            )?;
            daily.add(day_key.clone(), count)?;
            hosts.add(host, count)?;
            if family != "human" {
                bot_url_status.entry(name.clone()).or_default().add(
                    json!(format!("{}\t{}", pystr::repr_value(&path), code)),
                    count,
                )?;
                bots_daily
                    .entry(name.clone())
                    .or_default()
                    .add(day_key, count)?;
                if pystr::truthy(Some(&category)) {
                    categories
                        .entry(name.clone())
                        .or_default()
                        .add(category, count)?;
                }
            }
        }
    }

    if let Some(e) = loop_err {
        return match e {
            CfError::Cloudflare(m) => {
                Ok(json!({"ok": false, "source": SOURCE, "zone": requested_zone, "error": m}))
            }
            other => Err(other),
        };
    }

    if let Some(sink) = env.spend {
        // The spend record sits outside the error handler, so its failure propagates.
        sink(&crate::spend::Record {
            source: "cloudflare".to_owned(),
            operation: "graphql-traffic".to_owned(),
            cost: Some(0.0),
            unit: "requests".to_owned(),
            task_id: None,
            items: Some(
                split.queries as i64
                    + 1
                    + if include_cfg {
                        (crate::cloudflare::AI_SETTINGS.len()
                            + crate::cloudflare::RULE_PHASES.len()
                            + 1) as i64
                    } else {
                        0
                    },
            ),
            extra: None,
            at: None,
        })
        .map_err(|e| CfError::Os(e.to_string()))?;
    }

    let requests: i64 = daily.total();
    let bots: Vec<Value> = {
        let mut items: Vec<(String, i64, usize)> = bots_daily
            .iter()
            .enumerate()
            .map(|(i, (n, c))| (n.clone(), c.total(), i))
            .collect();
        items.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
        items
            .into_iter()
            .take(40)
            .map(|(n, total, _)| {
                json!({
                    "name": n,
                    "hits": total,
                    "verified_bot_categories": categories
                        .get(&n)
                        .map(counter_dict)
                        .unwrap_or_else(|| json!({})),
                })
            })
            .collect()
    };
    let url_fact_rows: Vec<Value> = python_tuple_sort(url_facts.into_values().collect())?
        .into_iter()
        .map(|(raw, count)| {
            let edge = raw.get(6).and_then(Value::as_i64).unwrap_or(0);
            json!({
                "host": raw.first().cloned().unwrap_or(Value::Null),
                "path": raw.get(1).cloned().unwrap_or(Value::Null),
                "date": raw.get(2).cloned().unwrap_or(Value::Null),
                "bot_family": raw.get(3).cloned().unwrap_or(Value::Null),
                "bot_name": raw.get(4).cloned().unwrap_or(Value::Null),
                "verified_bot_category": raw.get(5).cloned().unwrap_or(Value::Null),
                "edge_status": raw.get(6).cloned().unwrap_or(Value::Null),
                "origin_status": raw.get(7).cloned().unwrap_or(Value::Null),
                "cache_status": raw.get(8).cloned().unwrap_or(Value::Null),
                "edge_redirect": (300..400).contains(&edge),
                "requests": count,
            })
        })
        .collect();

    // A non-string `dimensions.date` can make the sort fail, and that error propagates.
    let daily_json = counter_sorted(&daily)?;
    let status_json = family_sorted(&status)?;
    let bots_daily_json = family_sorted(&bots_daily)?;
    let bot_url_status_json = {
        let mut out = Map::new();
        for (n, c) in &bot_url_status {
            let mut rows = Vec::new();
            for (k, v) in c.most_common(Some(25)) {
                // A tab inside the path shifts the status field, so the parse can fail.
                let key = k.as_str().map(str::to_owned).unwrap_or_default();
                let mut parts = key.split('\t');
                let path_part = parts.next().unwrap_or_default();
                let status_text = parts.next().ok_or_else(|| {
                    CfError::Python("IndexError", "list index out of range".to_owned())
                })?;
                let status_v = pystr::to_python_int(&json!(status_text)).map_err(
                    |(class, msg)| match class {
                        "ValueError" => CfError::Value(msg),
                        other => CfError::Python(other, msg),
                    },
                )?;
                rows.push(json!({"path": path_part, "status": status_v, "hits": v}));
            }
            out.insert(n.clone(), Value::Array(rows));
        }
        Value::Object(out)
    };

    let mut result = json!({
        "ok": true,
        "source": SOURCE,
        "note": NOTE,
        "zone_id": info.get("id").cloned().unwrap_or(Value::Null),
        "zone": info.get("name").cloned().unwrap_or(Value::Null),
        "plan": info.get("plan").cloned().unwrap_or(Value::Null),
        "period": {"from": stamp_day(start), "to": stamp_day(end)},
        "queries": split.queries,
        "sampled": split.sampled,
        "truncated_days": split.truncated_days,
        "requests": requests,
        "by_family": family_map(&families, Some(20)),
        "bots": bots,
        "status_by_family": status_json,
        "sections_by_family": family_map(&sections, Some(20)),
        "top_paths_by_family": family_map(&paths, Some(15)),
        "cache_by_family": family_map(&cache, None),
        "edge_origin_status": counter_json(&edge_vs_origin, Some(20)),
        "hosts": counter_json(&hosts, Some(20)),
        "daily": daily_json,
        "url_facts": url_fact_rows,
        "url_facts_truncated": url_facts_truncated,
        "url_facts_omitted": url_facts_omitted,
        "bots_daily": bots_daily_json,
        "bot_url_status": bot_url_status_json,
        "verification": {
            "checked": false,
            "reason": "Aggregated edge data carries no client IPs; reverse-DNS verification is impossible. verified_bot_categories is Cloudflare's own verdict where present.",
        },
    });
    result["findings"] = findings(&result);
    result["state"] = json!(if split.sampled
        || !split.truncated_days.is_empty()
        || url_facts_truncated
    {
        "partial"
    } else {
        "complete"
    });
    if include_cfg {
        // The zone id is URL-quoted, so a non-text id is an error.
        let zone_id_str = match zone_id.as_str() {
            Some(s) => s.to_owned(),
            None => {
                return Err(CfError::Python(
                    "TypeError",
                    "quote_from_bytes() expected bytes".to_owned(),
                ))
            }
        };
        let cfg = crate::cloudflare::configuration(
            env.host,
            env.home,
            transport,
            sleeper,
            &zone_id_str,
            Some(&json!(secret)),
            send,
        )
        .await?;
        result["configuration"] = cfg;
        result["configuration_requests"] =
            json!(crate::cloudflare::AI_SETTINGS.len() + crate::cloudflare::RULE_PHASES.len() + 1);
    }
    if project_given {
        let unavailable = || {
            json!({
                "state": "unavailable",
                "reason": "the aggregate result could not be added to project history",
            })
        };
        // Only I/O and value errors are caught here; other errors propagate.
        let history = match env.history {
            Some(h) => match h(&result, &project_s) {
                Ok(v) => v,
                Err(CfError::Os(_)) | Err(CfError::Value(_)) => unavailable(),
                Err(e) => return Err(e),
            },
            None => {
                match crate::cloudflare_history::save_snapshot(&result, &json!(project_s), None) {
                    Ok(v) => v,
                    Err(e) => match e.python_class() {
                        "OSError" | "ValueError" => unavailable(),
                        other => return Err(CfError::Python(other, e.to_string())),
                    },
                }
            }
        };
        result["history"] = history;
    }
    Ok(result)
}
