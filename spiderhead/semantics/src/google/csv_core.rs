//! `import-google-csv`: a per-language keyword table (`keyword,sv,location,kd,
//! intent,en_topic,page_type,landing_url,...[,sv_<CC>]`) as Google phrases.

use std::path::Path;

use rusqlite::params;
use serde_json::{json, Map, Value};

use super::{country, gnorm};
use crate::store::{zulu_seconds, Store};
use crate::Error;

/// DataForSEO location code for a country code in the `location` column.
fn location_of(cc: &str) -> Option<i64> {
    (2000..3000).find(|c| country(*c).is_some_and(|x| x.eq_ignore_ascii_case(cc)))
}

/// Language from `…-<lang>.csv` (e.g. `ml-core-ja.csv`), or `language` column.
fn language_of(file: &Path) -> Option<String> {
    let stem = file.file_stem()?.to_str()?;
    let tail = stem.rsplit('-').next()?;
    (stem.contains('-') && (2..=3).contains(&tail.len())).then(|| tail.to_lowercase())
}

/// Import the table. Provider volumes already stored are kept; the row's
/// topic/landing columns and any `laya_*` flags go to `extra_json`. Rows whose
/// `en_topic`/`page_type` match a topic also become its Google members.
pub fn import_google_csv(store: &Store, file: &Path) -> Result<Value, Error> {
    super::ensure_schema(&store.conn)?;
    let text =
        std::fs::read_to_string(file).map_err(|e| Error::Io(format!("{e}: {}", file.display())))?;
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(text.trim_start_matches('\u{feff}').as_bytes());
    let headers: Vec<String> = rdr
        .headers()
        .map_err(|e| Error::Config(format!("csv: {e}")))?
        .iter()
        .map(|h| h.trim().to_owned())
        .collect();
    let col = |n: &str| headers.iter().position(|h| h == n);
    let kw_c =
        col("keyword").ok_or_else(|| Error::Config("google csv needs a keyword column".into()))?;
    let file_lang = language_of(file);
    let (lang_c, loc_c) = (col("language"), col("location"));
    let now = zulu_seconds();
    let tx = store.conn.unchecked_transaction().map_err(Error::Db)?;
    let (mut rows, mut members) = (0, 0);
    for rec in rdr.records() {
        let rec = rec.map_err(|e| Error::Config(format!("csv: {e}")))?;
        let get = |i: usize| rec.get(i).unwrap_or("").trim().to_owned();
        let keyword = get(kw_c);
        let lang = lang_c
            .map(get)
            .filter(|l| !l.is_empty())
            .or_else(|| file_lang.clone());
        let Some(lang) = lang.filter(|_| !keyword.is_empty()) else {
            continue;
        };
        let norm = gnorm(&keyword);
        let mut meta = Map::new();
        for (i, h) in headers.iter().enumerate() {
            let v = get(i);
            let keep = matches!(
                h.as_str(),
                "en_topic" | "page_type" | "landing_url" | "service_url" | "qlang" | "money"
            ) || h.starts_with("laya_");
            if keep && !v.is_empty() {
                meta.insert(h.clone(), json!(v));
            }
        }
        let extra = Value::Object(meta).to_string();
        let cc = loc_c.map(get).unwrap_or_default();
        // The main location plus one extra location per `sv_<CC>` column.
        let mut places: Vec<(i64, String, String)> = vec![(
            location_of(&cc).unwrap_or(0),
            col("sv").map(get).unwrap_or_default(),
            col("kd").map(get).unwrap_or_default(),
        )];
        for (i, h) in headers.iter().enumerate() {
            if let Some(code) = h.strip_prefix("sv_").and_then(location_of) {
                places.push((code, get(i), String::new()));
            }
        }
        for (loc, sv, kd) in places {
            if loc == 0 && !cc.is_empty() {
                continue;
            }
            let primary = places_is_primary(loc, &cc);
            if !primary && sv.is_empty() {
                continue;
            }
            tx.execute(
                "INSERT INTO g_phrases(norm,keyword,language,location_code,country,device,volume,kd,cpc,intent,\
                 source,source_detail,extra_json,first_seen,last_update) \
                 VALUES(?1,?2,?3,?4,?5,'desktop',?6,?7,?8,?9,'csv',?10,?11,?12,?12) \
                 ON CONFLICT(norm,language,location_code,device) DO UPDATE SET \
                  volume=COALESCE(volume,excluded.volume), kd=COALESCE(kd,excluded.kd), \
                  cpc=COALESCE(cpc,excluded.cpc), intent=COALESCE(intent,excluded.intent), \
                  extra_json=json_patch(COALESCE(extra_json,'{}'),excluded.extra_json), \
                  last_update=excluded.last_update",
                params![
                    norm,
                    keyword,
                    lang,
                    loc,
                    country(loc),
                    sv.parse::<f64>().ok().map(|v| v as i64),
                    kd.parse::<f64>().ok().map(|v| v as i64),
                    col("cpc").and_then(|i| get(i).parse::<f64>().ok()),
                    col("intent").map(get).filter(|v| !v.is_empty()),
                    file.file_name().map(|f| f.to_string_lossy().into_owned()),
                    extra,
                    now,
                ],
            )
            .map_err(Error::Db)?;
            tx.execute(
                "INSERT OR IGNORE INTO g_sources(norm,language,location_code,source,source_detail) VALUES(?,?,?,'csv',?)",
                params![norm, lang, loc, file.file_name().map(|f| f.to_string_lossy().into_owned())],
            )
            .map_err(Error::Db)?;
            members += tx
                .execute(
                    "INSERT OR IGNORE INTO topic_members(topic_id,engine,norm,language,location_code) \
                     SELECT id,'google',?1,?2,?3 FROM topics WHERE name=?4 AND page_type=?5",
                    params![
                        norm,
                        lang,
                        loc,
                        col("en_topic").map(get).unwrap_or_default(),
                        col("page_type").map(get).unwrap_or_default()
                    ],
                )
                .map_err(Error::Db)?;
            rows += 1;
        }
    }
    tx.commit().map_err(Error::Db)?;
    Ok(json!({
        "contract": "spiderhead.semantics.operation.v1",
        "stage": "import-google-csv",
        "file": file.to_string_lossy(),
        "rows": rows,
        "topic_members_added": members,
    }))
}

fn places_is_primary(loc: i64, cc: &str) -> bool {
    location_of(cc) == Some(loc)
}
