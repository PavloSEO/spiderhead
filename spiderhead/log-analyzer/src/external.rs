//! Matching the URLs seen in logs against an external URL list (CSV, XLSX, sitemap or a
//! plain list), with the three comparison views: matched, not in logs, not in list.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use regex::Regex;
use serde_json::{json, Value};

use crate::error::{LogsError, Result};
use crate::join::{origin_of, url_key};
use crate::store::{import_metadata, open_log_source, paging, query_rows};
use crate::summary::{where_clause, Filter};

/// Header names that mark the URL column of a table, lowercase.
const URL_HEADERS: [&str; 6] = ["url", "address", "loc", "page", "link", "href"];

fn looks_like_url(cell: &str) -> bool {
    let t = cell.trim();
    t.starts_with('/') || t.starts_with("http://") || t.starts_with("https://")
}

/// URLs of a table: the column titled like a URL, else the first column; the first row is
/// a header unless it already holds a URL.
fn column_urls(rows: Vec<Vec<String>>) -> Vec<String> {
    let Some(first) = rows.first() else {
        return Vec::new();
    };
    let named = first
        .iter()
        .position(|h| URL_HEADERS.contains(&h.trim().to_lowercase().as_str()));
    let has_header = named.is_some() || !first.first().is_some_and(|c| looks_like_url(c));
    let column = named.unwrap_or(0);
    rows.into_iter()
        .skip(usize::from(has_header))
        .filter_map(|r| r.get(column).map(|c| c.trim().to_string()))
        .filter(|c| !c.is_empty())
        .collect()
}

/// Parse the `<loc>` entries of a sitemap or sitemap index.
pub fn sitemap_urls(xml: &str) -> Vec<String> {
    let Ok(loc) = Regex::new(r"(?s)<loc>\s*(?:<!\[CDATA\[(.*?)\]\]>|(.*?))\s*</loc>") else {
        return Vec::new();
    };
    loc.captures_iter(xml)
        .filter_map(|c| c.get(1).or_else(|| c.get(2)))
        .map(|m| m.as_str().trim().replace("&amp;", "&"))
        .filter(|u| !u.is_empty())
        .collect()
}

/// Read an external URL list. The format follows the extension: `.csv`, `.xlsx`/`.xls`,
/// `.xml` (sitemap) or anything else as one URL per line.
pub fn load_url_list(path: &Path) -> Result<Vec<String>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "xlsx" | "xls" => {
            use calamine::Reader;
            let mut book = calamine::open_workbook_auto(path)
                .map_err(|e| LogsError::value(format!("cannot read workbook: {e}")))?;
            let name = book
                .sheet_names()
                .first()
                .cloned()
                .ok_or_else(|| LogsError::value("workbook has no sheets"))?;
            let range = book
                .worksheet_range(&name)
                .map_err(|e| LogsError::value(format!("cannot read sheet: {e}")))?;
            Ok(column_urls(
                range
                    .rows()
                    .map(|r| r.iter().map(ToString::to_string).collect())
                    .collect(),
            ))
        }
        "csv" => {
            let mut reader = csv::ReaderBuilder::new()
                .has_headers(false)
                .flexible(true)
                .from_path(path)
                .map_err(|e| LogsError::value(format!("cannot read CSV: {e}")))?;
            let mut rows = Vec::new();
            for record in reader.records() {
                let record = record.map_err(|e| LogsError::value(format!("bad CSV row: {e}")))?;
                rows.push(record.iter().map(str::to_string).collect());
            }
            Ok(column_urls(rows))
        }
        "xml" => Ok(sitemap_urls(&std::fs::read_to_string(path)?)),
        _ => Ok(std::fs::read_to_string(path)?
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()),
    }
}

/// Which side of the comparison to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListView {
    /// In both the logs and the list.
    Matched,
    /// In the list but never requested in the logs.
    NotInLogs,
    /// Requested in the logs but absent from the list.
    NotInList,
}

impl ListView {
    /// Parse `matched`, `not_in_logs` or `not_in_list`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "matched" => Some(Self::Matched),
            "not_in_logs" => Some(Self::NotInLogs),
            "not_in_list" => Some(Self::NotInList),
            _ => None,
        }
    }
}

/// Compare the log URLs of `source` (under `filter`) with `list`.
///
/// URLs are compared as `path?query` on the project's origin; list entries of another
/// origin are counted in `foreign` and ignored. Case, query strings and trailing slashes
/// are significant. Rows are `{url, visits}` ordered by visits (list-only rows have 0).
pub fn compare_with_list(
    source: &str,
    list: &[String],
    filter: &Filter,
    view: ListView,
    limit: i64,
    offset: i64,
) -> Result<Value> {
    paging(limit, offset)?;
    let con = open_log_source(source)?;
    let target = import_metadata(&con)?
        .get("project_target")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let origin = origin_of(&target)?
        .ok_or_else(|| LogsError::value("log artifact has no project origin to compare against"))?;
    let mut keys: BTreeSet<String> = BTreeSet::new();
    let mut foreign = 0usize;
    for entry in list {
        match url_key(entry, &origin)? {
            Some(k) => {
                keys.insert(k);
            }
            None => foreign += 1,
        }
    }
    let (clause, params) = where_clause(&con, filter)?;
    let logged: BTreeMap<String, i64> = query_rows(
        &con,
        &format!("SELECT url,SUM(visits) visits FROM visits WHERE {clause} GROUP BY url"),
        &params,
    )?
    .into_iter()
    .filter_map(|r| {
        Some((
            r.get("url")?.as_str()?.to_string(),
            r.get("visits")?.as_i64()?,
        ))
    })
    .collect();
    let mut rows: Vec<(String, i64)> = match view {
        ListView::Matched => logged
            .iter()
            .filter(|(u, _)| keys.contains(*u))
            .map(|(u, v)| (u.clone(), *v))
            .collect(),
        ListView::NotInList => logged
            .iter()
            .filter(|(u, _)| !keys.contains(*u))
            .map(|(u, v)| (u.clone(), *v))
            .collect(),
        ListView::NotInLogs => keys
            .iter()
            .filter(|u| !logged.contains_key(*u))
            .map(|u| (u.clone(), 0))
            .collect(),
    };
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let total = rows.len() as i64;
    let page: Vec<Value> = rows
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|(url, visits)| json!({"url": url, "visits": visits}))
        .collect();
    let more = offset + (page.len() as i64) < total;
    let matched = logged.keys().filter(|u| keys.contains(*u)).count();
    Ok(json!({
        "ok": true, "rows": page, "total": total, "limit": limit, "offset": offset,
        "has_more": more, "next_offset": if more { json!(offset + limit) } else { Value::Null },
        "summary": {
            "list_urls": keys.len(), "foreign": foreign, "log_urls": logged.len(),
            "matched": matched,
            "not_in_logs": keys.len() - matched,
            "not_in_list": logged.len() - matched,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sitemap_locs_are_unescaped() {
        let xml = "<urlset><url><loc>https://e.test/a?x=1&amp;y=2</loc></url><url><loc> <![CDATA[https://e.test/b]]> </loc></url></urlset>";
        assert_eq!(
            sitemap_urls(xml),
            ["https://e.test/a?x=1&y=2", "https://e.test/b"]
        );
    }

    #[test]
    fn table_column_is_found_by_header_or_position() {
        let t = |rows: &[&[&str]]| {
            column_urls(
                rows.iter()
                    .map(|r| r.iter().map(|c| c.to_string()).collect())
                    .collect(),
            )
        };
        assert_eq!(
            t(&[&["id", "Address"], &["1", "/a"], &["2", "/b"]]),
            ["/a", "/b"]
        );
        assert_eq!(t(&[&["/a", "x"], &["/b", "y"]]), ["/a", "/b"]);
        assert_eq!(t(&[&["URL"], &["/a"], &[""]]), ["/a"]);
        assert!(t(&[]).is_empty());
    }
}
