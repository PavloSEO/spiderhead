//! Checkpoint indexing and correspondence binding: loading one source
//! collection into the SQLite work index, then mapping baseline URLs onto
//! their declared follow-up destinations.

use rusqlite::Connection;
use serde_json::{json, Value};
use spiderhead_core::job_control::JobProgressUpdate;
use spiderhead_parse::py_str::py_repr;

use crate::compare::base::{iter_rows, key_of, mapped_url, AuditSource, CompareError};
use crate::segments::Segment;

use super::checkpoint::{commit_txn, set_state, state_value_or};
use super::export::INDEX_BATCH;
use super::job::Recorder;
use super::shared::{check_cancel, py_json};
use super::source::{collection_count, pair, primary_segment as segment_of};

fn label_of(side: i64) -> &'static str {
    if side == 0 {
        "before"
    } else {
        "after"
    }
}

fn integrity_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(ffi_error, _)
            if ffi_error.extended_code & 0xFF == rusqlite::ffi::SQLITE_CONSTRAINT
    )
}

/// Forward one running-index progress event through the recorder: construction
/// failures are swallowed like `_notify`, write failures abort the job.
fn report_index_progress(
    recorder: &mut Recorder<'_>,
    job_id: &str,
    stage: &str,
    completed: i64,
    total: Option<i64>,
) -> Result<(), CompareError> {
    if let Ok(update) =
        JobProgressUpdate::new(job_id, "running", stage, Some(completed), total, None)
    {
        recorder.persist_progress(&update)?;
    }
    Ok(())
}

/// Read one source collection into the checkpoint index, batch-committing a
/// resumable cursor and progress with every [`INDEX_BATCH`] rows.
#[allow(clippy::too_many_arguments)]
pub(crate) fn index_collection(
    con: &Connection,
    source: &AuditSource<'_>,
    side: i64,
    name: &str,
    ordered_segments: &[Segment],
    checkpoint_key: &str,
    recorder: &mut Recorder<'_>,
    job_id: &str,
    progress_base: i64,
    progress_total: Option<i64>,
    cancel_event: Option<&dyn Fn() -> bool>,
) -> Result<i64, CompareError> {
    let source_backed = matches!(source, AuditSource::Reader(_));
    let offset = state_value_or(con, checkpoint_key, json!(0))?
        .as_i64()
        .unwrap_or(0);
    let total = collection_count(source, name);
    if total.is_some_and(|bound| offset > bound) {
        return Err(CompareError(
            "comparison checkpoint exceeds its source collection".into(),
        ));
    }
    let stage = format!("index_{}_{}", label_of(side), name);
    // Reader-backed sources stream one item per ordinal; document sources were
    // already materialized whole by iter_rows.
    let rows: Box<dyn Iterator<Item = (i64, Result<Value, CompareError>)> + '_> = if source_backed {
        let AuditSource::Reader(reader) = source else {
            return Err(CompareError(
                "validated audit.v2 source lacks a collection count".into(),
            ));
        };
        let Some(bound) = total else {
            return Err(CompareError(
                "validated audit.v2 source lacks a collection count".into(),
            ));
        };
        let pointer = format!("/{name}");
        Box::new((offset..bound).map(move |ordinal| {
            (
                ordinal,
                reader
                    .get_item(&pointer, ordinal)
                    .map_err(|e| CompareError(e.to_string())),
            )
        }))
    } else {
        let materialized = iter_rows(source, name)?;
        Box::new(
            materialized
                .into_iter()
                .enumerate()
                .filter(|(ordinal, _)| *ordinal as i64 >= offset)
                .map(|(ordinal, row_value)| (ordinal as i64, Ok(row_value))),
        )
    };
    let mut last_offset = offset;
    for (ordinal, row) in rows {
        check_cancel(cancel_event)?;
        last_offset = ordinal + 1;
        let row = row?;
        let Some(record) = row.as_object() else {
            return Err(CompareError(if name == "pages" {
                "comparison page must contain a nonempty URL".into()
            } else {
                "comparison finding must contain a nonempty check".into()
            }));
        };
        if name == "pages" {
            let Some(url) = record
                .get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.is_empty())
            else {
                return Err(CompareError(
                    "comparison page must contain a nonempty URL".into(),
                ));
            };
            let document = py_json(&row);
            let segment = segment_of(record, ordered_segments);
            let insert = con.execute(
                "INSERT INTO pages VALUES (?,?,?,?,?,?)",
                rusqlite::params![
                    side,
                    url,
                    url,
                    ordinal,
                    if source_backed { None } else { Some(document) },
                    segment,
                ],
            );
            if let Err(error) = insert {
                if integrity_error(&error) {
                    return Err(CompareError(format!(
                        "comparison has duplicate page URL {}",
                        py_repr(url)
                    )));
                }
                return Err(CompareError(error.to_string()));
            }
        } else {
            // key_of re-derives the check below; this binding only validates it.
            let Some(_) = record
                .get("check")
                .and_then(Value::as_str)
                .filter(|check| !check.is_empty())
            else {
                return Err(CompareError(
                    "comparison finding must contain a nonempty check".into(),
                ));
            };
            let target = record.get("target_url");
            if !matches!(target, None | Some(Value::Null))
                && !matches!(target, Some(Value::String(_)))
            {
                return Err(CompareError(
                    "comparison finding target_url must be a string or null".into(),
                ));
            }
            let (check, url) = key_of(record);
            let document = py_json(&row);
            let insert = con.execute(
                "INSERT INTO issues VALUES (?,?,?,?,?,?)",
                rusqlite::params![
                    side,
                    check,
                    url,
                    url,
                    ordinal,
                    if source_backed { None } else { Some(document) },
                ],
            );
            if let Err(error) = insert {
                if integrity_error(&error) {
                    return Err(CompareError(format!(
                        "comparison has duplicate finding key ({}, {})",
                        py_repr(&check),
                        py_repr(&url)
                    )));
                }
                return Err(CompareError(error.to_string()));
            }
        }
        let next_offset = ordinal + 1;
        if next_offset % INDEX_BATCH == 0 {
            set_state(con, checkpoint_key, &json!(next_offset))?;
            commit_txn(con)?;
            report_index_progress(
                recorder,
                job_id,
                &stage,
                progress_base + next_offset,
                progress_total,
            )?;
        }
    }
    if total.is_some_and(|bound| offset != bound) {
        let bound = total.unwrap_or_default();
        set_state(con, checkpoint_key, &json!(bound))?;
        commit_txn(con)?;
        report_index_progress(
            recorder,
            job_id,
            &stage,
            progress_base + bound,
            progress_total,
        )?;
    } else if total.is_none() {
        set_state(con, checkpoint_key, &json!(last_offset))?;
        commit_txn(con)?;
    }
    match total {
        Some(bound) => Ok(bound),
        None => Ok(state_value_or(con, checkpoint_key, json!(0))?
            .as_i64()
            .unwrap_or(0)),
    }
}

/// Declare baseline destinations, detect collisions and push both URL tables
/// onto their comparison identities.
pub(crate) fn bind(
    con: &Connection,
    declaration: &crate::compare::base::Declaration,
    recorder: &mut Recorder<'_>,
    job_id: &str,
    total: Option<i64>,
    cancel_event: Option<&dyn Fn() -> bool>,
) -> Result<(), CompareError> {
    let mut completed: i64 = 0;
    let mut statement = con
        .prepare("SELECT url FROM pages WHERE side=0 ORDER BY url")
        .map_err(|e| CompareError(e.to_string()))?;
    let mut found = statement
        .query([])
        .map_err(|e| CompareError(e.to_string()))?;
    while let Some(row) = found.next().map_err(|e| CompareError(e.to_string()))? {
        check_cancel(cancel_event)?;
        completed += 1;
        let before_url: String = row.get(0).map_err(|e| CompareError(e.to_string()))?;
        let (after_url, kind) = mapped_url(&before_url, declaration);
        let Some(after_url) = after_url else {
            if completed % INDEX_BATCH == 0 {
                report_index_progress(recorder, job_id, "url_correspondence", completed, total)?;
            }
            continue;
        };
        let matched = con
            .query_row(
                "SELECT 1 FROM pages WHERE side=1 AND url=?1",
                rusqlite::params![after_url],
                |row| row.get::<_, i64>(0),
            )
            .is_ok();
        let state = if matched {
            "matched"
        } else {
            "after_not_crawled"
        };
        let document = py_json(&Value::Object(pair(
            &before_url,
            &after_url,
            kind.unwrap_or("explicit_pair"),
            state,
        )));
        con.execute(
            "INSERT INTO pairs VALUES (?,?,?,?)",
            rusqlite::params![before_url, after_url, state, document],
        )
        .map_err(|e| CompareError(e.to_string()))?;
        if matched {
            con.execute(
                "UPDATE pages SET comparison_url=?1 WHERE side=0 AND url=?2",
                rusqlite::params![after_url, before_url],
            )
            .map_err(|e| CompareError(e.to_string()))?;
        }
        if completed % INDEX_BATCH == 0 {
            report_index_progress(recorder, job_id, "url_correspondence", completed, total)?;
        }
    }
    drop(found);
    drop(statement);
    for (before_url, after_url) in &declaration.pairs {
        let present = con
            .query_row(
                "SELECT 1 FROM pages WHERE side=0 AND url=?1",
                rusqlite::params![before_url],
                |row| row.get::<_, i64>(0),
            )
            .is_ok();
        if !present {
            let document = py_json(&Value::Object(super::source::pair(
                before_url,
                after_url,
                "explicit_pair",
                "before_not_crawled",
            )));
            con.execute(
                "INSERT INTO pairs VALUES (?,?,?,?)",
                rusqlite::params![before_url, after_url, "before_not_crawled", document],
            )
            .map_err(|e| CompareError(e.to_string()))?;
        }
    }
    // Include identity-mapped pages in collision detection: A -> B must not
    // silently merge with an existing, otherwise unmapped baseline B.
    let collision: Option<String> = con
        .query_row(
            "SELECT comparison_url FROM pages WHERE side=0 GROUP BY comparison_url HAVING count(*)>1 LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();
    if let Some(target) = collision {
        return Err(CompareError(format!(
            "url correspondence has a page collision at {}",
            py_repr(&target)
        )));
    }
    con.execute(
        "UPDATE issues SET comparison_url=COALESCE(
            (SELECT after_url FROM pairs WHERE before_url=issues.url AND state='matched'), url)
            WHERE side=0",
        [],
    )
    .map_err(|e| CompareError(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use spiderhead_store::evidence_contract::Obj;

    #[test]
    fn rejects_pages_without_url() {
        let con = Connection::open_in_memory().expect("db");
        con.execute_batch(
            "CREATE TABLE pages(side INTEGER,url TEXT,comparison_url TEXT,ordinal INTEGER,document TEXT,segment TEXT NOT NULL,PRIMARY KEY(side,url));
             CREATE TABLE job_state(key TEXT PRIMARY KEY,value TEXT NOT NULL);",
        )
        .expect("schema");
        let document: Obj = serde_json::from_str(r#"{"pages": [{"url": ""}]}"#).expect("doc");
        let source = AuditSource::Document(&document);
        let job: Obj = serde_json::from_str(r#"{"stage": "indexing"}"#).expect("job");
        let mut recorder = Recorder::new(
            job,
            std::path::Path::new("/nonexistent/job.json"),
            "job",
            None,
            None,
        );
        let error = index_collection(
            &con,
            &source,
            0,
            "pages",
            &[],
            "indexed:0:pages",
            &mut recorder,
            "job",
            0,
            None,
            None,
        )
        .err()
        .map(|e| e.0);
        assert_eq!(
            error,
            Some("comparison page must contain a nonempty URL".into())
        );
    }
}
