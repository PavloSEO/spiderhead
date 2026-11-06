//! `import-dataforseo`: ingest saved DataForSEO v3 responses into the Google
//! tables. Every task is recorded in `provider_runs` (cost included) keyed by
//! its provider task id, so importing the same folder twice adds nothing.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};

use super::{country, gnorm};
use crate::store::{zulu_seconds, Store};
use crate::Error;

/// What a task is, decided from `data` and `path`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Ranked,
    Suggestions,
    Ideas,
    Related,
    Competitors,
    AdsVolume,
    AdsIdeas,
    Serp,
    Other,
}

impl Kind {
    fn of(task: &Value) -> Kind {
        let data = &task["data"];
        let mut hay = task["path"]
            .as_array()
            .map(|p| {
                p.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .unwrap_or_default();
        for k in ["api", "function"] {
            if let Some(s) = data[k].as_str() {
                hay.push('/');
                hay.push_str(s);
            }
        }
        let has = |s: &str| hay.contains(s);
        if has("locations") || has("languages") {
            Kind::Other
        } else if has("ranked_keywords") {
            Kind::Ranked
        } else if has("keyword_suggestions") {
            Kind::Suggestions
        } else if has("keyword_ideas") {
            Kind::Ideas
        } else if has("related_keywords") {
            Kind::Related
        } else if has("competitors_domain") {
            Kind::Competitors
        } else if has("keywords_for_keywords") {
            Kind::AdsIdeas
        } else if has("search_volume") {
            Kind::AdsVolume
        } else if has("serp") {
            Kind::Serp
        } else {
            Kind::Other
        }
    }

    fn label(self) -> &'static str {
        match self {
            Kind::Ranked => "labs_ranked",
            Kind::Suggestions => "labs_suggestions",
            Kind::Ideas => "labs_ideas",
            Kind::Related => "labs_related",
            Kind::Competitors => "labs_competitors",
            Kind::AdsVolume => "ads_volume",
            Kind::AdsIdeas => "ads_ideas",
            Kind::Serp => "serp_organic",
            Kind::Other => "other",
        }
    }
}

/// Request context shared by all rows of one task.
struct Ctx<'a> {
    conn: &'a Connection,
    lang: String,
    loc: i64,
    device: String,
    source: &'static str,
    detail: String,
    now: String,
}

fn s(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(str::to_owned)
}

fn num_i(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64))
}

/// Keep only the provider fields that have no column of their own.
fn extra(kd: &Value) -> Option<String> {
    let mut m = Map::new();
    let mut put = |k: &str, v: &Value| {
        if !v.is_null() {
            m.insert(k.into(), v.clone());
        }
    };
    let info = &kd["keyword_info"];
    put("categories", &info["categories"]);
    put("trend", &info["search_volume_trend"]);
    put("info_updated", &info["last_updated_time"]);
    let props = &kd["keyword_properties"];
    put("words_count", &props["words_count"]);
    put("detected_language", &props["detected_language"]);
    put("core_keyword", &props["core_keyword"]);
    put("serp_item_types", &kd["serp_info"]["serp_item_types"]);
    put("se_results_count", &kd["serp_info"]["se_results_count"]);
    put(
        "foreign_intent",
        &kd["search_intent_info"]["foreign_intent"],
    );
    let bl = &kd["avg_backlinks_info"];
    put("backlinks", &bl["backlinks"]);
    put("referring_domains", &bl["referring_domains"]);
    put("main_domain_rank", &bl["main_domain_rank"]);
    (!m.is_empty()).then(|| Value::Object(m).to_string())
}

/// Store one keyword record. `kd` is a Labs keyword_data object or an Ads
/// result item; both are probed for the same fields.
fn put_phrase(c: &Ctx, kd: &Value) -> Result<Option<String>, Error> {
    let Some(keyword) = s(&kd["keyword"]) else {
        return Ok(None);
    };
    let norm = gnorm(&keyword);
    let lang = s(&kd["language_code"]).unwrap_or_else(|| c.lang.clone());
    let loc = kd["location_code"].as_i64().unwrap_or(c.loc);
    let info = if kd["keyword_info"].is_object() {
        &kd["keyword_info"]
    } else {
        kd
    };
    let volume = num_i(&info["search_volume"]);
    let monthly =
        (!info["monthly_searches"].is_null()).then(|| info["monthly_searches"].to_string());
    let kdiff = num_i(&kd["keyword_properties"]["keyword_difficulty"]);
    // Ads reports competition as LOW/MEDIUM/HIGH plus a 0..100 index.
    let index = info["competition_index"].as_f64().map(|i| i / 100.0);
    let (competition, level) = match &info["competition"] {
        Value::Number(n) => (n.as_f64(), s(&info["competition_level"])),
        Value::String(t) => (index, Some(t.clone())),
        _ => (index, None),
    };
    c.conn
        .execute(
            "INSERT INTO g_phrases(norm,keyword,language,location_code,country,device,volume,\
             volume_monthly_json,kd,cpc,low_bid,high_bid,competition,competition_level,intent,\
             source,source_detail,extra_json,first_seen,last_update) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?19) \
             ON CONFLICT(norm,language,location_code,device) DO UPDATE SET \
              volume=COALESCE(excluded.volume,volume), \
              volume_monthly_json=COALESCE(excluded.volume_monthly_json,volume_monthly_json), \
              kd=COALESCE(excluded.kd,kd), cpc=COALESCE(excluded.cpc,cpc), \
              low_bid=COALESCE(excluded.low_bid,low_bid), high_bid=COALESCE(excluded.high_bid,high_bid), \
              competition=COALESCE(excluded.competition,competition), \
              competition_level=COALESCE(excluded.competition_level,competition_level), \
              intent=COALESCE(excluded.intent,intent), \
              extra_json=COALESCE(excluded.extra_json,extra_json), \
              keyword=COALESCE(keyword,excluded.keyword), last_update=excluded.last_update",
            params![
                norm,
                keyword,
                lang,
                loc,
                country(loc),
                c.device,
                volume,
                monthly,
                kdiff,
                info["cpc"].as_f64(),
                info["low_top_of_page_bid"].as_f64(),
                info["high_top_of_page_bid"].as_f64(),
                competition,
                level,
                s(&kd["search_intent_info"]["main_intent"]),
                c.source,
                c.detail,
                extra(kd),
                c.now,
            ],
        )
        .map_err(Error::Db)?;
    c.conn
        .execute(
            "INSERT OR IGNORE INTO g_sources(norm,language,location_code,source,source_detail) VALUES(?,?,?,?,?)",
            params![norm, lang, loc, c.source, c.detail],
        )
        .map_err(Error::Db)?;
    Ok(Some(norm))
}

fn ranked(c: &Ctx, res: &Value) -> Result<i64, Error> {
    let target = s(&res["target"]).unwrap_or_default();
    let mut n = 0;
    for item in res["items"].as_array().into_iter().flatten() {
        let kd = &item["keyword_data"];
        let Some(norm) = put_phrase(c, kd)? else {
            continue;
        };
        let el = &item["ranked_serp_element"]["serp_item"];
        let lang = s(&kd["language_code"]).unwrap_or_else(|| c.lang.clone());
        let loc = kd["location_code"].as_i64().unwrap_or(c.loc);
        c.conn
            .execute(
                "INSERT INTO g_rankings(norm,language,location_code,domain,url,target,position,rank_group,\
                 serp_type,etv,paid_cost,fetched_at,source) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?) \
                 ON CONFLICT(norm,language,location_code,domain,url) DO UPDATE SET \
                 position=excluded.position, rank_group=excluded.rank_group, serp_type=excluded.serp_type, \
                 etv=excluded.etv, paid_cost=excluded.paid_cost, fetched_at=excluded.fetched_at, target=excluded.target",
                params![
                    norm,
                    lang,
                    loc,
                    s(&el["domain"]).unwrap_or_default(),
                    s(&el["url"]).unwrap_or_default(),
                    target,
                    num_i(&el["rank_absolute"]),
                    num_i(&el["rank_group"]),
                    s(&el["type"]),
                    el["etv"].as_f64(),
                    el["estimated_paid_traffic_cost"].as_f64(),
                    s(&kd["serp_info"]["last_updated_time"]),
                    c.source,
                ],
            )
            .map_err(Error::Db)?;
        n += 1;
    }
    Ok(n)
}

/// Suggestions, ideas and related keywords: a seed record plus items that are
/// either keyword data themselves or wrap it in `keyword_data`.
fn keyword_list(c: &Ctx, res: &Value) -> Result<i64, Error> {
    let mut n = 0;
    if res["seed_keyword_data"].is_object() && put_phrase(c, &res["seed_keyword_data"])?.is_some() {
        n += 1;
    }
    for item in res["items"].as_array().into_iter().flatten() {
        let kd = if item["keyword_data"].is_object() {
            &item["keyword_data"]
        } else {
            item
        };
        if put_phrase(c, kd)?.is_some() {
            n += 1;
        }
    }
    Ok(n)
}

fn competitors(c: &Ctx, res: &Value) -> Result<i64, Error> {
    let target = s(&res["target"]).unwrap_or_default();
    let mut n = 0;
    for it in res["items"].as_array().into_iter().flatten() {
        let Some(domain) = s(&it["domain"]) else {
            continue;
        };
        let m = &it["metrics"]["organic"];
        let full = &it["full_domain_metrics"]["organic"];
        c.conn
            .execute(
                "INSERT OR REPLACE INTO g_competitors(target,language,location_code,domain,avg_position,\
                 intersections,etv,kw_count,domain_etv,domain_kw_count) VALUES(?,?,?,?,?,?,?,?,?,?)",
                params![
                    target,
                    c.lang,
                    c.loc,
                    domain,
                    it["avg_position"].as_f64(),
                    num_i(&it["intersections"]),
                    m["etv"].as_f64(),
                    num_i(&m["count"]),
                    full["etv"].as_f64(),
                    num_i(&full["count"]),
                ],
            )
            .map_err(Error::Db)?;
        n += 1;
    }
    Ok(n)
}

/// Every `references` array reachable under a SERP item (AI overview cites).
fn references(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == "references" {
                    for r in x.as_array().into_iter().flatten() {
                        out.push(
                            json!({"domain": r["domain"], "url": r["url"], "title": r["title"]}),
                        );
                    }
                } else {
                    references(x, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| references(x, out)),
        _ => {}
    }
}

fn serp(c: &Ctx, res: &Value) -> Result<i64, Error> {
    let Some(keyword) = s(&res["keyword"]) else {
        return Ok(0);
    };
    let norm = gnorm(&keyword);
    let lang = s(&res["language_code"]).unwrap_or_else(|| c.lang.clone());
    let loc = res["location_code"].as_i64().unwrap_or(c.loc);
    let at = s(&res["datetime"]).unwrap_or_else(|| c.now.clone());
    c.conn
        .execute(
            "DELETE FROM g_serp WHERE norm=?1 AND language=?2 AND location_code=?3 AND device=?4 AND fetched_at=?5",
            params![norm, lang, loc, c.device, at],
        )
        .map_err(Error::Db)?;
    let mut ai = 0;
    let mut md = None;
    let mut cited = Vec::new();
    let mut paa = Vec::new();
    let mut related = Value::Null;
    let mut n = 0;
    for it in res["items"].as_array().into_iter().flatten() {
        let ty = s(&it["type"]).unwrap_or_default();
        match ty.as_str() {
            "ai_overview" => {
                ai = 1;
                md = md.or_else(|| s(&it["markdown"]));
                references(it, &mut cited);
            }
            "people_also_ask" => {
                for q in it["items"].as_array().into_iter().flatten() {
                    paa.push(json!({"title": q["title"]}));
                    references(q, &mut cited);
                }
            }
            "related_searches" => related = it["items"].clone(),
            _ => {}
        }
        if s(&it["url"]).is_some() || s(&it["domain"]).is_some() {
            c.conn
                .execute(
                    "INSERT INTO g_serp(norm,language,location_code,device,fetched_at,pos,rank_group,type,domain,url,title) \
                     VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                    params![
                        norm,
                        lang,
                        loc,
                        c.device,
                        at,
                        num_i(&it["rank_absolute"]),
                        num_i(&it["rank_group"]),
                        ty,
                        s(&it["domain"]),
                        s(&it["url"]),
                        s(&it["title"]),
                    ],
                )
                .map_err(Error::Db)?;
            n += 1;
        }
    }
    let js = |v: &Value| (!v.is_null()).then(|| v.to_string());
    c.conn
        .execute(
            "INSERT OR REPLACE INTO g_serp_features(norm,language,location_code,device,fetched_at,ai_overview,\
             ai_overview_markdown,cited_json,paa_json,related_searches_json,item_types_json,se_results_count,check_url) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                norm,
                lang,
                loc,
                c.device,
                at,
                ai,
                md,
                (!cited.is_empty()).then(|| Value::Array(cited).to_string()),
                (!paa.is_empty()).then(|| Value::Array(paa).to_string()),
                js(&related),
                js(&res["item_types"]),
                num_i(&res["se_results_count"]),
                s(&res["check_url"]),
            ],
        )
        .map_err(Error::Db)?;
    Ok(n)
}

/// Tasks of one saved file: a response (`tasks`), a collector wrapper
/// (`{key, payload, resp}`), a bare list of tasks, or one bare task as a
/// task-queue `task_get` download saves it (`{id, data, result}`). Anything
/// else is not a response and yields `None`.
fn tasks_of(doc: &Value) -> Option<Vec<Value>> {
    let doc = if doc["resp"].is_object() {
        &doc["resp"]
    } else {
        doc
    };
    if let Some(t) = doc["tasks"].as_array() {
        return Some(t.clone());
    }
    if doc["id"].is_string() && doc["data"].is_object() && doc["result"].is_array() {
        return Some(vec![doc.clone()]);
    }
    let list = doc.as_array()?;
    (!list.is_empty()
        && list
            .iter()
            .all(|t| t["result"].is_array() || t["data"].is_object()))
    .then(|| list.clone())
}

fn json_files(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .map_err(|e| Error::Io(format!("{e}: {}", dir.display())))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json") && p.is_file())
        .map(|p| {
            (
                p.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH),
                p,
            )
        })
        .collect();
    // Oldest first: where files overlap, the newest data is written last.
    files.sort();
    Ok(files.into_iter().map(|(_, p)| p).collect())
}

/// Import every response in `dir`; returns the summary document.
pub fn import_dataforseo(store: &Store, dir: &Path) -> Result<Value, Error> {
    super::ensure_schema(&store.conn)?;
    let now = zulu_seconds();
    let (mut files, mut tasks, mut known, mut skipped, mut pending) = (0, 0, 0, 0, 0);
    let mut by_endpoint: Map<String, Value> = Map::new();
    let mut cost = 0.0;
    for path in json_files(dir)? {
        let name = path
            .file_stem()
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|d| tasks_of(&d));
        let Some(list) = parsed else {
            skipped += 1;
            continue;
        };
        files += 1;
        let tx = store.conn.unchecked_transaction().map_err(Error::Db)?;
        for (i, task) in list.iter().enumerate() {
            // A queued or failed task has no result yet; recording it would
            // make the later complete download look already imported.
            if task["status_code"].as_i64().is_some_and(|c| c != 20000) {
                pending += 1;
                continue;
            }
            let task_id = s(&task["id"]).unwrap_or_else(|| format!("{name}#{i}"));
            let seen: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM provider_runs WHERE task_id=?",
                    [&task_id],
                    |r| r.get(0),
                )
                .map_err(Error::Db)?;
            if seen > 0 {
                known += 1;
                continue;
            }
            tasks += 1;
            let kind = Kind::of(task);
            let data = &task["data"];
            let target = s(&data["target"]).or_else(|| s(&data["keyword"]));
            let c = Ctx {
                conn: &tx,
                lang: s(&data["language_code"]).unwrap_or_default(),
                loc: data["location_code"].as_i64().unwrap_or(0),
                device: s(&data["device"]).unwrap_or_else(|| "desktop".into()),
                source: kind.label(),
                detail: target.clone().unwrap_or_default(),
                now: now.clone(),
            };
            let mut rows = 0;
            // Ads results are a flat list; Labs and SERP results are one object with items.
            for res in task["result"].as_array().into_iter().flatten() {
                rows += match kind {
                    Kind::Ranked => ranked(&c, res)?,
                    Kind::Suggestions | Kind::Ideas | Kind::Related => keyword_list(&c, res)?,
                    Kind::Competitors => competitors(&c, res)?,
                    Kind::AdsVolume | Kind::AdsIdeas => i64::from(put_phrase(&c, res)?.is_some()),
                    Kind::Serp => serp(&c, res)?,
                    Kind::Other => 0,
                };
            }
            let task_cost = task["cost"].as_f64().unwrap_or(0.0);
            cost += task_cost;
            let endpoint = task["path"]
                .as_array()
                .map(|p| {
                    p.iter()
                        .filter_map(Value::as_str)
                        .filter(|x| *x != "v3")
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| kind.label().to_owned());
            tx.execute(
                "INSERT INTO provider_runs(provider,endpoint,name,task_id,cost_usd,rows,ts,raw_path,language,location_code,target) \
                 VALUES('dataforseo',?,?,?,?,?,?,?,?,?,?)",
                params![
                    endpoint,
                    name,
                    task_id,
                    task_cost,
                    rows,
                    now,
                    path.to_string_lossy(),
                    c.lang,
                    c.loc,
                    target
                ],
            )
            .map_err(Error::Db)?;
            let e = by_endpoint
                .entry(kind.label().to_owned())
                .or_insert(json!({"tasks": 0, "rows": 0}));
            e["tasks"] = json!(e["tasks"].as_i64().unwrap_or(0) + 1);
            e["rows"] = json!(e["rows"].as_i64().unwrap_or(0) + rows);
        }
        tx.commit().map_err(Error::Db)?;
    }
    Ok(json!({
        "contract": "spiderhead.semantics.operation.v1",
        "stage": "import-dataforseo",
        "files": files,
        "tasks_imported": tasks,
        "tasks_already_imported": known,
        "files_not_responses": skipped,
        "tasks_not_ready": pending,
        "by_endpoint": by_endpoint,
        "cost_usd": (cost * 1e5).round() / 1e5,
        "google": super::status(store),
    }))
}
