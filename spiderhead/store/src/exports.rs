//! Deterministically restore legacy files from a validated scan.
//!
//! `export_run` opens the scan read-only; the full `open_scan` validation belongs to
//! the scan artifact reader and is not repeated here.

use crate::discovery_ledger::{py_dumps, query_maps};
use crate::error::StoreError;
use rusqlite::Connection;
use serde_json::{Map, Value};
use spiderhead_core::filesystem::{file_identity, link_identity, PrivateMode};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const PAGE_SYSTEM_COLUMNS: &[&str] = &["document_id", "page_ordinal", "url", "url_id"];

/// Boolean page fields (copied; `scan_artifact` owns the canonical list).
const PAGE_BOOLS: &[&str] = &[
    "head_not_first",
    "transport_upgraded",
    "title_outside_head",
    "meta_description_outside_head",
    "canonical_outside_head",
    "directives_outside_head",
    "hreflang_outside_head",
];

/// Late-added page fields as (field name, column).
const LATE_PAGE_FIELDS: &[(&str, &str)] = &[
    ("content_frames", "content_frames"),
    ("content_frames_same_origin", "content_frames_same_origin"),
    ("hreflang", "hreflang_json"),
    ("heading_outline", "heading_outline_json"),
    ("link_placement", "link_placement_json"),
    ("canonical_chain", "canonical_chain_json"),
    ("final_canonical", "final_canonical"),
    ("body_unavailable", "body_unavailable"),
    ("meta_refresh", "meta_refresh"),
    ("http_refresh", "http_refresh"),
    ("meta_description_count", "meta_description_count"),
    ("h1_alt_text", "h1_alt_text"),
    ("lorem_ipsum_count", "lorem_ipsum_count"),
    ("images_total", "images_total"),
    ("images_missing_alt_attr", "images_missing_alt_attr"),
    ("images_max_alt_length", "images_max_alt_length"),
    ("plugin_elements", "plugin_elements"),
    ("meta_fragment", "meta_fragment"),
    ("ajax_scheme_outlinks", "ajax_scheme_outlinks"),
    ("og_url", "og_url"),
    ("amphtml", "amphtml"),
    ("mobile_alternate_broken", "mobile_alternate_broken"),
];

/// Owned files: path -> (device, inode) recorded when this run created them.
pub type Owned = BTreeMap<String, (i64, i64)>;

fn fail(msg: impl Into<String>) -> StoreError {
    StoreError::Invalid(msg.into())
}

fn key_of(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Remove `path` only if it is still the regular file this run created.
pub fn unlink_owned(path: &Path, owned: &Owned) -> Result<(), StoreError> {
    let Some(&(dev, ino)) = owned.get(&key_of(path)) else {
        return Ok(());
    };
    let state = match fs::symlink_metadata(path) {
        Ok(state) => state,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if link_identity(path)
        .map(|i| (i.0 as i64, i.1 as i64))
        .unwrap_or((0, 0))
        == (dev, ino)
        && !state.file_type().is_symlink()
    {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Create `path` exclusively (mode 0600), record its identity, write every chunk and fsync.
pub fn write_file(
    path: &Path,
    chunks: impl IntoIterator<Item = Vec<u8>>,
    owned: &mut Owned,
) -> Result<(), StoreError> {
    let mut file: File = OpenOptions::new()
        .write(true)
        .create_new(true)
        .private_mode(0o600)
        .open(path)?;
    let state = file_identity(&file)?;
    owned.insert(key_of(path), (state.0 as i64, state.1 as i64));
    for chunk in chunks {
        file.write_all(&chunk)?;
    }
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

/// One JSON Lines record: compact, sorted keys, non-ASCII kept, trailing newline.
pub fn json_line(row: &Map<String, Value>) -> Vec<u8> {
    let mut line = py_dumps(&Value::Object(row.clone()), true, false);
    line.push('\n');
    line.into_bytes()
}

pub fn write_jsonl(
    path: &Path,
    rows: &[Map<String, Value>],
    owned: &mut Owned,
) -> Result<(), StoreError> {
    write_file(path, rows.iter().map(json_line), owned)
}

/// Write `name` in `directory` through a hidden temporary file, then hard-link it into place.
pub fn publish(
    directory: &Path,
    name: &str,
    rows: &[Map<String, Value>],
    owned: &mut Owned,
) -> Result<(), StoreError> {
    let temporary = directory.join(format!(".{name}.tmp"));
    let final_path = directory.join(name);
    write_jsonl(&temporary, rows, owned)?;
    fs::hard_link(&temporary, &final_path)?;
    let identity = owned[&key_of(&temporary)];
    owned.insert(key_of(&final_path), identity);
    unlink_owned(&temporary, owned)?;
    Ok(())
}

fn parse_json_text(value: &Value) -> Result<Value, StoreError> {
    let text = value
        .as_str()
        .ok_or_else(|| fail("cannot export scan: JSON column is not text"))?;
    serde_json::from_str(text).map_err(|e| fail(format!("cannot export scan: {e}")))
}

/// One page record per `pages` row in crawl order.
pub fn page_rows(
    con: &Connection,
    url: Option<&str>,
) -> Result<Vec<Map<String, Value>>, StoreError> {
    page_rows_limited(con, url, None, 0)
}

/// [`page_rows`] bounded to at most `limit` rows after `offset` (`None` = all).
pub fn page_rows_limited(
    con: &Connection,
    url: Option<&str>,
    limit: Option<i64>,
    offset: i64,
) -> Result<Vec<Map<String, Value>>, StoreError> {
    let late: BTreeMap<&str, &str> = LATE_PAGE_FIELDS
        .iter()
        .map(|(name, col)| (*col, *name))
        .collect();
    let mut sql = String::from(
        "SELECT p.*, u.url, c.payload_json AS render_blocking_payload \
         FROM pages AS p JOIN urls AS u USING(url_id) LEFT JOIN context_items c \
         ON c.kind='legacy_render_blocking' AND c.item_key='page:'||p.url_id",
    );
    let mut params: Vec<rusqlite::types::Value> = match url {
        Some(u) => {
            sql.push_str(" WHERE u.url=?1");
            vec![rusqlite::types::Value::Text(u.to_string())]
        }
        None => Vec::new(),
    };
    sql.push_str(" ORDER BY p.page_ordinal");
    if let Some(limit) = limit {
        sql.push_str(" LIMIT ? OFFSET ?");
        params.push(rusqlite::types::Value::Integer(limit));
        params.push(rusqlite::types::Value::Integer(offset));
    }
    let records = query_maps(con, &sql, rusqlite::params_from_iter(params))?;
    let mut out = Vec::with_capacity(records.len());
    for record in records {
        let mut page = Map::new();
        page.insert(
            "url".into(),
            record.get("url").cloned().unwrap_or(Value::Null),
        );
        for (name, value) in &record {
            if PAGE_SYSTEM_COLUMNS.contains(&name.as_str()) {
                continue;
            }
            let is_null = value.is_null();
            match name.as_str() {
                "render_blocking_payload" => {
                    if !is_null {
                        let resources = parse_json_text(value)?
                            .get("resources")
                            .cloned()
                            .unwrap_or(Value::Null);
                        page.insert(
                            "render_blocking".into(),
                            if resources.is_null() {
                                Value::Null
                            } else {
                                Value::String(py_dumps(&resources, false, true))
                            },
                        );
                    }
                }
                "redirect_chain_json" => {
                    page.insert("redirect_chain".into(), parse_json_text(value)?);
                }
                "hreflang_json" => {
                    if !is_null {
                        page.insert("hreflang".into(), parse_json_text(value)?);
                    }
                }
                "heading_outline_json" => {
                    if !is_null {
                        page.insert("heading_outline".into(), parse_json_text(value)?);
                    }
                }
                "link_placement_json" => {
                    if !is_null {
                        page.insert("link_placement".into(), parse_json_text(value)?);
                    }
                }
                "canonical_chain_json" => {
                    if !is_null {
                        page.insert("canonical_chain".into(), parse_json_text(value)?);
                    }
                }
                "invalid_link_targets_json" => {
                    page.insert(
                        "invalid_link_targets".into(),
                        if is_null {
                            Value::Null
                        } else {
                            parse_json_text(value)?
                        },
                    );
                }
                "trust_signals_json" => {
                    page.insert(
                        "trust_signals".into(),
                        if is_null {
                            Value::Null
                        } else {
                            parse_json_text(value)?
                        },
                    );
                }
                "duplicate_ids_json" => {
                    page.insert(
                        "duplicate_ids".into(),
                        if is_null {
                            Value::Null
                        } else {
                            parse_json_text(value)?
                        },
                    );
                }
                other if late.contains_key(other) => {
                    if !is_null {
                        page.insert(late[other].into(), value.clone());
                    }
                }
                other if PAGE_BOOLS.contains(&other) => {
                    let flag = if is_null {
                        Value::Null
                    } else {
                        Value::Bool(value.as_i64().unwrap_or(0) != 0)
                    };
                    page.insert(other.into(), flag);
                }
                other => {
                    page.insert(other.into(), value.clone());
                }
            }
        }
        out.push(page);
    }
    Ok(out)
}

/// One link record per selected link.
pub fn link_rows(con: &Connection) -> Result<Vec<Map<String, Value>>, StoreError> {
    link_rows_limited(con, None, 0)
}

/// [`link_rows`] bounded to at most `limit` rows after `offset` (`None` = all).
pub fn link_rows_limited(
    con: &Connection,
    limit: Option<i64>,
    offset: i64,
) -> Result<Vec<Map<String, Value>>, StoreError> {
    let mut sql = format!(
        "{}SELECT l.*, s.url AS source, d.url AS destination FROM l \
         JOIN urls AS s ON s.url_id = l.source_url_id \
         JOIN urls AS d ON d.url_id = l.destination_url_id ORDER BY l.link_id",
        crate::analysis_graph::selected_links_cte("l")
    );
    let params: Vec<rusqlite::types::Value> = match limit {
        Some(limit) => {
            sql.push_str(" LIMIT ? OFFSET ?");
            vec![
                rusqlite::types::Value::Integer(limit),
                rusqlite::types::Value::Integer(offset),
            ]
        }
        None => Vec::new(),
    };
    let records = query_maps(con, &sql, rusqlite::params_from_iter(params))?;
    let mut out = Vec::with_capacity(records.len());
    for record in records {
        let get = |k: &str| record.get(k).cloned().unwrap_or(Value::Null);
        let mut row = Map::new();
        row.insert("source".into(), get("source"));
        row.insert("destination".into(), get("destination"));
        row.insert("anchor".into(), get("anchor"));
        if record.contains_key("anchor_source") {
            row.insert("anchor_source".into(), get("anchor_source"));
        }
        row.insert(
            "nofollow".into(),
            Value::Bool(get("nofollow").as_i64().unwrap_or(0) != 0),
        );
        row.insert("position".into(), get("position"));
        row.insert("rel".into(), parse_json_text(&get("rel_json"))?);
        row.insert("target".into(), get("target"));
        row.insert("raw_href".into(), get("raw_href"));
        out.push(row);
    }
    Ok(out)
}

/// Write the stored audit document verbatim.
pub fn write_audit(path: &Path, con: &Connection, owned: &mut Owned) -> Result<(), StoreError> {
    let document: Option<String> = con
        .query_row(
            "SELECT document_json FROM audit WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some(document) = document else {
        return Err(fail("scan has no saved audit"));
    };
    write_file(path, [document.into_bytes()], owned)
}

/// Fsync a directory's entries so new names survive a crash.
fn fsync_directory(directory: &Path) -> Result<(), StoreError> {
    File::open(directory)?.sync_all()?;
    Ok(())
}

/// Export one scan to a new legacy-format directory without modifying its source.
pub fn export_run(scan: &Path, out_dir: &Path) -> Result<Map<String, Value>, StoreError> {
    let destination: PathBuf = std::path::absolute(out_dir)?;
    if destination.symlink_metadata().is_ok() {
        return Err(fail(format!(
            "export destination already exists: {}",
            destination.display()
        )));
    }
    let con = spiderhead_core::sqlite::open_readonly(scan, Duration::from_secs(5), &[])?;
    let partial: i64 = con.query_row("SELECT crawl_partial FROM scan", [], |r| r.get(0))?;
    let audit_text: String = con.query_row("SELECT document_json FROM audit", [], |r| r.get(0))?;
    let audit = parse_json_text(&Value::String(audit_text))?;
    let run_partial = audit
        .get("run")
        .and_then(|r| r.get("crawl_partial"))
        .is_some_and(|v| v.as_bool() == Some(true));
    if partial != 0 && !run_partial {
        return Err(fail(
            "three-file export would hide recovered crawl partialness; keep and use the original SQLite scan",
        ));
    }
    let pages_count: i64 = con.query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))?;
    let links_count: i64 = con.query_row("SELECT COUNT(*) FROM links", [], |r| r.get(0))?;
    let mut owned = Owned::new();
    fs::create_dir(&destination)?;
    let result = (|| -> Result<(), StoreError> {
        publish(
            &destination,
            "pages.jsonl",
            &page_rows(&con, None)?,
            &mut owned,
        )?;
        publish(&destination, "links.jsonl", &link_rows(&con)?, &mut owned)?;
        let tmp = destination.join(".audit.json.tmp");
        write_audit(&tmp, &con, &mut owned)?;
        let audit_path = destination.join("audit.json");
        fs::hard_link(&tmp, &audit_path)?;
        let identity = owned[&key_of(&tmp)];
        owned.insert(key_of(&audit_path), identity);
        unlink_owned(&tmp, &owned)?;
        fsync_directory(&destination)?;
        Ok(())
    })();
    if let Err(err) = result {
        for key in owned.keys().rev().cloned().collect::<Vec<_>>() {
            let _ = unlink_owned(Path::new(&key), &owned);
        }
        let _ = fs::remove_dir(&destination);
        return Err(match err {
            StoreError::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists => fail(format!(
                "export destination already exists: {}",
                destination.display()
            )),
            other => fail(format!("cannot export scan: {other}")),
        });
    }
    let mut counts = Map::new();
    counts.insert("pages".into(), Value::from(pages_count));
    counts.insert("links".into(), Value::from(links_count));
    let mut out = Map::new();
    out.insert("ok".into(), Value::Bool(true));
    out.insert("path".into(), Value::from(key_of(&destination)));
    out.insert("counts".into(), Value::Object(counts));
    Ok(out)
}
