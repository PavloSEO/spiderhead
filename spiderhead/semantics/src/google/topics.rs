//! `import-topics`: load the cross-language topic map (one row per topic and
//! page type, one landing column per locale) that links Google and Yandex
//! phrases of the same topic.

use std::path::Path;

use rusqlite::params;
use serde_json::{json, Value};

use super::gnorm;
use crate::store::Store;
use crate::Error;

/// Import `topic_id,en_topic,page_type,<loc>_landing,...[,demand_<loc>,main_kw_<loc>]`.
/// Empty landing cells are kept as gaps (`url=''`); `main_kw_<loc>` becomes a
/// Google topic member. Re-importing replaces the topic's rows.
pub fn import_topics(store: &Store, file: &Path) -> Result<Value, Error> {
    super::ensure_schema(&store.conn)?;
    let text =
        std::fs::read_to_string(file).map_err(|e| Error::Io(format!("{e}: {}", file.display())))?;
    let text = text.trim_start_matches('\u{feff}');
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(text.as_bytes());
    let headers: Vec<String> = rdr
        .headers()
        .map_err(|e| Error::Config(format!("topic map: {e}")))?
        .iter()
        .map(|h| h.trim().to_owned())
        .collect();
    let col = |name: &str| headers.iter().position(|h| h == name);
    let (Some(id_c), Some(name_c)) = (col("topic_id"), col("en_topic")) else {
        return Err(Error::Config(
            "topic map needs topic_id and en_topic columns".into(),
        ));
    };
    let type_c = col("page_type");
    let landings: Vec<(String, usize)> = headers
        .iter()
        .enumerate()
        .filter_map(|(i, h)| h.strip_suffix("_landing").map(|l| (l.to_owned(), i)))
        .collect();
    let tx = store.conn.unchecked_transaction().map_err(Error::Db)?;
    let (mut topics, mut members) = (0, 0);
    for rec in rdr.records() {
        let rec = rec.map_err(|e| Error::Config(format!("topic map: {e}")))?;
        let get = |i: usize| rec.get(i).unwrap_or("").trim().to_owned();
        let id = get(id_c);
        if id.is_empty() {
            continue;
        }
        tx.execute(
            "INSERT INTO topics(id,name,page_type) VALUES(?,?,?) \
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, page_type=excluded.page_type",
            params![id, get(name_c), type_c.map(get)],
        )
        .map_err(Error::Db)?;
        for (locale, i) in &landings {
            tx.execute(
                "INSERT OR REPLACE INTO topic_landings(topic_id,locale,url) VALUES(?,?,?)",
                params![id, locale, get(*i)],
            )
            .map_err(Error::Db)?;
        }
        for (locale, _) in &landings {
            let kw = col(&format!("main_kw_{locale}"))
                .map(get)
                .unwrap_or_default();
            if kw.is_empty() {
                continue;
            }
            let demand = col(&format!("demand_{locale}")).and_then(|i| get(i).parse::<i64>().ok());
            tx.execute(
                "INSERT OR REPLACE INTO topic_members(topic_id,engine,norm,language,location_code,demand) \
                 VALUES(?,?,?,?,0,?)",
                params![id, "google", gnorm(&kw), locale, demand],
            )
            .map_err(Error::Db)?;
            members += 1;
        }
        topics += 1;
    }
    tx.commit().map_err(Error::Db)?;
    let gaps: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM v_topic_gaps", [], |r| r.get(0))
        .map_err(Error::Db)?;
    Ok(json!({
        "contract": "spiderhead.semantics.operation.v1",
        "stage": "import-topics",
        "topics": topics,
        "locales": landings.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>(),
        "members": members,
        "missing_landings": gaps,
    }))
}
