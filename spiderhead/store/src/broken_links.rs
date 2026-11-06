//! The "broken links and resources" report of one retained scan: every link,
//! script, stylesheet, image, font, video or document whose recorded answer is
//! an HTTP error status or a failed fetch, with the page that references it.
//!
//! Read-only over the scan; the report is written to a new CSV or XLSX file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use spiderhead_parse::py_url::urlparse;

use crate::error::StoreError;
use crate::scan_artifact::open_scan;
use crate::scan_export::{publish_files, write_csv_bytes, write_xlsx, Record, Selection, Writer};

/// Wire format tag of the answer.
pub const FORMAT: &str = "spiderhead.scan-broken-links.v1";
/// Output file formats.
pub const FORMATS: &[&str] = &["csv", "xlsx"];
/// Report columns, in output order.
pub const COLUMNS: [&str; 5] = ["source_page", "target", "status", "type", "anchor_or_alt"];

/// One reported defect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenRow {
    /// Page that references the target.
    pub source_page: String,
    /// Referenced URL.
    pub target: String,
    /// HTTP status, or 0 when no response arrived (transport failure).
    pub status: i64,
    /// `page`, `image`, `css`, `js`, `font`, `video`, `pdf` or `external`.
    pub kind: String,
    /// Link text or the wrapped image's alt; empty for resources.
    pub anchor_or_alt: String,
}

impl BrokenRow {
    fn record(&self) -> Record {
        let mut out = Map::new();
        out.insert("source_page".into(), json!(self.source_page));
        out.insert("target".into(), json!(self.target));
        out.insert("status".into(), json!(self.status));
        out.insert("type".into(), json!(self.kind));
        out.insert("anchor_or_alt".into(), json!(self.anchor_or_alt));
        out
    }
}

fn failure(code: &str, message: String) -> StoreError {
    StoreError::Invalid(format!("{code}: {message}"))
}

fn table_exists(con: &Connection, name: &str) -> Result<bool, StoreError> {
    Ok(con
        .query_row(
            "SELECT 1 FROM main.sqlite_master WHERE type='table' AND name=?1",
            [name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// A recorded answer is a defect: an error status, or none at all for a
/// request that was made and failed.
fn is_broken(status: Option<i64>, failed: bool) -> Option<i64> {
    match status {
        Some(code) if code >= 400 => Some(code),
        Some(code) if code > 0 => None,
        _ if failed => Some(0),
        _ => None,
    }
}

fn host(url: &str) -> String {
    urlparse(url)
        .map(|p| p.netloc.to_lowercase())
        .unwrap_or_default()
}

/// Resource type from the declaring kind, falling back to the file extension.
fn classify(url: &str, declared: &str) -> &'static str {
    match declared {
        "script" => return "js",
        "stylesheet" | "css_url" | "css_import" => return "css",
        "image" => return "image",
        "font" => return "font",
        "video" | "audio" | "media" | "track" => return "video",
        _ => {}
    }
    let path = urlparse(url)
        .map(|p| p.path.to_lowercase())
        .unwrap_or_default();
    let extension = path.rsplit('.').next().unwrap_or("");
    match extension {
        "pdf" => "pdf",
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif" | "svg" | "ico" | "bmp" => "image",
        "css" => "css",
        "js" | "mjs" => "js",
        "woff" | "woff2" | "ttf" | "otf" | "eot" => "font",
        "mp4" | "webm" | "ogv" | "mov" | "mp3" | "ogg" | "wav" => "video",
        _ => "page",
    }
}

/// External link check answers by URL: `(status, failed)`.
fn external_answers(con: &Connection) -> Result<BTreeMap<String, (Option<i64>, bool)>, StoreError> {
    let mut out = BTreeMap::new();
    if !table_exists(con, "external_checks")? {
        return Ok(out);
    }
    let mut statement = con.prepare("SELECT outcome,payload_json FROM external_checks")?;
    let rows = statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (outcome, payload) = row?;
        let Ok(payload) = serde_json::from_str::<Value>(&payload) else {
            continue;
        };
        let Some(url) = payload["url"].as_str() else {
            continue;
        };
        out.insert(
            url.to_owned(),
            (payload["status_code"].as_i64(), outcome == "failed"),
        );
    }
    Ok(out)
}

/// Every broken reference in the scan, deduplicated by (source, target).
pub fn collect(con: &Connection) -> Result<Vec<BrokenRow>, StoreError> {
    let external = external_answers(con)?;
    let mut rows: Vec<BrokenRow> = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut push = |row: BrokenRow, rows: &mut Vec<BrokenRow>| {
        if seen.insert((row.source_page.clone(), row.target.clone())) {
            rows.push(row);
        }
    };

    // Hyperlinks: a crawled destination answers from `pages`, any other one
    // from the external link check.
    let mut statement = con.prepare(
        "SELECT su.url, du.url, l.anchor, p.status_code, p.url_id IS NOT NULL \
         FROM links l \
         JOIN urls su ON su.url_id=l.source_url_id \
         JOIN urls du ON du.url_id=l.destination_url_id \
         LEFT JOIN pages p ON p.url_id=l.destination_url_id \
         ORDER BY l.link_id",
    )?;
    let links = statement.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, bool>(4)?,
        ))
    })?;
    for link in links {
        let (source, target, anchor, status, crawled) = link?;
        let (status, failed) = if crawled {
            (status, false)
        } else if let Some(&(status, failed)) = external.get(&target) {
            (status, failed)
        } else {
            continue;
        };
        let Some(code) = is_broken(status, failed) else {
            continue;
        };
        let kind = if host(&source) != host(&target) {
            "external"
        } else {
            classify(&target, "")
        };
        push(
            BrokenRow {
                source_page: source,
                target,
                status: code,
                kind: kind.to_owned(),
                anchor_or_alt: anchor,
            },
            &mut rows,
        );
    }

    // Scripts and stylesheets recorded with the page.
    let mut statement = con.prepare(
        "SELECT pu.url, ru.url, r.kind, rs.status_code, r.capture_state='fetch_failed' \
         FROM resource_refs r \
         JOIN urls pu ON pu.url_id=r.page_url_id \
         JOIN urls ru ON ru.url_id=r.resource_url_id \
         LEFT JOIN responses rs ON rs.response_id=r.response_id \
         ORDER BY r.resource_ref_id",
    )?;
    let refs = statement.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, bool>(4)?,
        ))
    })?;
    for item in refs {
        let (source, target, kind, status, failed) = item?;
        if let Some(code) = is_broken(status, failed) {
            let row = BrokenRow {
                kind: classify(&target, &kind).to_owned(),
                source_page: source,
                target,
                status: code,
                anchor_or_alt: String::new(),
            };
            push(row, &mut rows);
        }
    }

    // Images, fonts, media and documents of the declared-resource graph.
    if table_exists(con, "resource_graph_occurrences")?
        && table_exists(con, "resource_graph_fetches")?
    {
        let mut statement = con.prepare(
            "SELECT pu.url, o.resolved_url, o.kind, f.status_code, f.state='failed' \
             FROM resource_graph_occurrences o \
             JOIN urls pu ON pu.url_id=o.page_url_id \
             JOIN resource_graph_fetches f ON f.resolved_url=o.resolved_url \
             ORDER BY o.occurrence_id",
        )?;
        let occurrences = statement.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, bool>(4)?,
            ))
        })?;
        for item in occurrences {
            let (source, target, kind, status, failed) = item?;
            if let Some(code) = is_broken(status, failed) {
                let row = BrokenRow {
                    kind: classify(&target, &kind).to_owned(),
                    source_page: source,
                    target,
                    status: code,
                    anchor_or_alt: String::new(),
                };
                push(row, &mut rows);
            }
        }
    }
    rows.sort_by(|a, b| (&a.source_page, &a.target).cmp(&(&b.source_page, &b.target)));
    Ok(rows)
}

/// Write the report of `input_path` to the new file `out`.
pub fn write_report(
    input_path: &Path,
    out: &Path,
    fmt: &str,
) -> Result<Map<String, Value>, StoreError> {
    if !FORMATS.contains(&fmt) {
        return Err(failure("invalid_format", "fmt must be csv or xlsx".into()));
    }
    if out.as_os_str().is_empty() {
        return Err(failure(
            "invalid_output",
            "out must name a file path".into(),
        ));
    }
    let parent = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    if !parent.is_dir() {
        return Err(failure(
            "invalid_output",
            format!("output directory does not exist: {}", parent.display()),
        ));
    }
    if std::fs::symlink_metadata(out).is_ok() {
        return Err(failure(
            "output_exists",
            format!("output already exists: {}", out.display()),
        ));
    }
    let con = open_scan(input_path, false, None)?;
    let rows = collect(&con)?;
    drop(con);
    let fields: Vec<String> = COLUMNS.iter().map(|c| (*c).to_owned()).collect();
    let stream = || -> crate::scan_export::RecordStream<'_> {
        Box::new(rows.iter().map(|row| Ok(row.record())))
    };
    let mut files: indexmap::IndexMap<String, PathBuf> = indexmap::IndexMap::new();
    files.insert(fmt.to_owned(), out.to_path_buf());
    let writer: Writer<'_> = if fmt == "csv" {
        Box::new(|target, owned| write_csv_bytes(target, &fields, stream(), owned))
    } else {
        Box::new(|target, owned| {
            write_xlsx(
                target,
                &json!({"format": FORMAT, "rows": rows.len()}),
                vec![Selection {
                    name: "links".into(),
                    fields: fields.clone(),
                    stream: stream(),
                }],
                owned,
            )
        })
    };
    publish_files(&files, vec![(fmt.to_owned(), writer)])?;
    let mut by_type: BTreeMap<&str, i64> = BTreeMap::new();
    for row in &rows {
        *by_type.entry(row.kind.as_str()).or_default() += 1;
    }
    let mut result = Map::new();
    result.insert("ok".into(), json!(true));
    result.insert("format".into(), json!(FORMAT));
    result.insert("fmt".into(), json!(fmt));
    result.insert("file".into(), json!(out.to_string_lossy()));
    result.insert("rows".into(), json!(rows.len()));
    result.insert("by_type".into(), json!(by_type));
    result.insert("columns".into(), json!(COLUMNS));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_prefers_the_declared_kind_then_the_extension() {
        assert_eq!(classify("https://e.test/a.jpg", "image"), "image");
        assert_eq!(classify("https://e.test/a.php?x=1", "script"), "js");
        assert_eq!(classify("https://e.test/doc.PDF", ""), "pdf");
        assert_eq!(classify("https://e.test/f.woff2", ""), "font");
        assert_eq!(classify("https://e.test/x", ""), "page");
    }

    #[test]
    fn only_error_statuses_and_failed_requests_are_defects() {
        assert_eq!(is_broken(Some(404), false), Some(404));
        assert_eq!(is_broken(Some(200), false), None);
        assert_eq!(is_broken(Some(301), false), None);
        assert_eq!(is_broken(None, true), Some(0));
        assert_eq!(is_broken(None, false), None);
    }
}
