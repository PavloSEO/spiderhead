//! Link findings from retained occurrences and observed HTTP outcomes.
//!
//! A connection-local temp projection is indexed on disk; each finding keeps a bounded location
//! sample plus exact counts, while every contributing occurrence stays in the immutable scan.
//! Unfetched destinations never become broken links.

use crate::analysis_graph::selected_links_cte;
use crate::crawl_records::spider::Scope;
use crate::error::StoreError;
use crate::ledger::urlparts::urlsplit;
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use spiderhead_parse::py_url::{urljoin, urlparse, urlunparse};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

/// Check ids this projection can produce.
pub const CHECK_IDS: &[&str] = &[
    "BROKEN_INTERNAL_LINK",
    "LINK_TO_5XX",
    "INTERNAL_LINK_TO_REDIRECT",
    "REDIRECT_CHAIN",
    "REDIRECT_LOOP",
    "INTERNAL_LINK_NO_ANCHOR",
    "EXTERNAL_NO_RESPONSE",
];

/// Process-wide counter giving each projection its own temp table name.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Anchor text made only of these characters counts as empty.
const BLANK_ANCHOR: &str = "trim(anchor, char(9)||char(10)||char(13)||char(160)||' ')=''";

/// Error kinds that mean the check was refused or skipped, not that a server failed to answer.
const NOT_A_FAILURE: [&str; 3] = ["blocked_redirect", "private_network", "invalid_url"];

/// A page row as the redirect walk sees it.
struct Hop {
    url: String,
    status: Option<i64>,
    location: String,
}

/// Connection-local evidence projection with bounded per-finding memory.
///
/// The temp table lives until the value is dropped; `PRAGMA query_only` is restored then.
pub struct RetainedLinkFindings<'a> {
    con: &'a Connection,
    limit: i64,
    hop_cap: i64,
    table: String,
    before: i64,
    ready: bool,
}

/// Fragment-less form of `url`; text without a `#` is returned untouched.
fn defrag(url: &str) -> String {
    if !url.contains('#') {
        return url.to_owned();
    }
    match urlparse(url) {
        Ok(mut parts) => {
            parts.fragment.clear();
            urlunparse(&parts)
        }
        Err(_) => url.split('#').next().unwrap_or("").to_owned(),
    }
}

fn str_field<'v>(item: &'v Value, key: &str) -> Result<&'v str, StoreError> {
    item.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| StoreError::Invalid(format!("external check payload lacks `{key}`")))
}

fn int_field(item: &Value, key: &str) -> Result<i64, StoreError> {
    item.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| StoreError::Invalid(format!("external check payload lacks `{key}`")))
}

impl<'a> RetainedLinkFindings<'a> {
    /// Build the projection. `max_locations` is clamped to 1..=1000 (default 200) and
    /// `hop_cap` to 2..=100 (default 20).
    pub fn new(con: &'a Connection, max_locations: i64, hop_cap: i64) -> Result<Self, StoreError> {
        let before: i64 = con.query_row("PRAGMA query_only", [], |r| r.get(0))?;
        let mut this = RetainedLinkFindings {
            con,
            limit: max_locations.clamp(1, 1000),
            hop_cap: hop_cap.clamp(2, 100),
            table: format!(
                "retained_links_{:x}",
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ),
            before,
            ready: false,
        };
        this.build()?;
        Ok(this)
    }

    /// Create the helper functions, the temp table and its two indexes.
    fn build(&mut self) -> Result<(), StoreError> {
        let (start_url, config_json): (String, String) = self.con.query_row(
            "SELECT start_url,config_json FROM scan WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let config: Value = serde_json::from_str(&config_json)?;
        let scope_map: Option<BTreeMap<String, Value>> = config
            .get("scope")
            .and_then(Value::as_object)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        let scope = Scope::from_config(scope_map.as_ref());
        let host = urlsplit(&start_url).hostname().unwrap_or_default();
        let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
        self.con
            .create_scalar_function("retained_link_target", 1, flags, |ctx| {
                Ok(defrag(&ctx.get::<String>(0)?))
            })?;
        self.con
            .create_scalar_function("retained_link_internal", 1, flags, move |ctx| {
                Ok(i64::from(scope.is_internal(&ctx.get::<String>(0)?, &host)))
            })?;
        if self.before != 0 {
            self.con.execute_batch("PRAGMA query_only=OFF")?;
        }
        let table = &self.table;
        self.con.execute_batch(&format!(
            "CREATE TEMP TABLE {table} AS {cte}\
             SELECT l.link_id,l.source_url_id,l.destination_url_id,l.anchor,l.position,l.nofollow,\
             src.url source,dst.url destination,p.url_id target_id,p.status_code status_code,\
             retained_link_internal(dst.url) internal \
             FROM l JOIN urls src ON src.url_id=l.source_url_id \
             JOIN urls dst ON dst.url_id=l.destination_url_id \
             LEFT JOIN pages p ON p.url_id=(SELECT url_id FROM urls WHERE url=retained_link_target(dst.url))",
            cte = selected_links_cte("l"),
        ))?;
        self.ready = true;
        self.con.execute_batch(&format!(
            "CREATE INDEX temp.{table}_target ON {table}(target_id,internal,link_id);\
             CREATE INDEX temp.{table}_source ON {table}(source_url_id,internal,link_id);"
        ))?;
        Ok(())
    }

    /// Bounded location sample plus exact `(occurrences, distinct source pages)` for one page.
    ///
    /// `source` keys the lookup on the linking page instead of the target; `empty` keeps only
    /// links whose anchor text is blank.
    pub fn locations(
        &self,
        url_id: i64,
        source: bool,
        empty: bool,
    ) -> Result<(Vec<Value>, i64, i64), StoreError> {
        let column = if source { "source_url_id" } else { "target_id" };
        let blank = if empty {
            format!(" AND {BLANK_ANCHOR}")
        } else {
            String::new()
        };
        let filter = format!("{column}=?1 AND internal=1{blank}");
        let table = &self.table;
        let (total, sources): (i64, i64) = self.con.query_row(
            &format!("SELECT COUNT(*),COUNT(DISTINCT source_url_id) FROM {table} WHERE {filter}"),
            [url_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut stmt = self.con.prepare(&format!(
            "SELECT source,destination,anchor,position,nofollow FROM {table} WHERE {filter} \
             ORDER BY link_id LIMIT ?2"
        ))?;
        let shown = stmt
            .query_map((url_id, self.limit), |r| {
                let position: Option<String> = r.get(3)?;
                let nofollow: Option<i64> = r.get(4)?;
                Ok(json!({
                    "source_url": r.get::<_, String>(0)?,
                    "destination_url": r.get::<_, String>(1)?,
                    "anchor": r.get::<_, Option<String>>(2)?,
                    "link_position": position.filter(|p| !p.is_empty()),
                    "follow": nofollow.unwrap_or(0) == 0,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((shown, total, sources))
    }

    /// Internal links to targets that answered with a redirect, client error or server error.
    pub fn status_findings(&self) -> Result<Vec<Value>, StoreError> {
        let table = &self.table;
        let mut stmt = self.con.prepare(&format!(
            "SELECT l.target_id,u.url,l.status_code FROM {table} l JOIN urls u ON u.url_id=l.target_id \
             WHERE internal=1 AND status_code>=300 AND status_code<600 GROUP BY l.target_id ORDER BY l.target_id"
        ))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (target_id, url, status) in rows {
            let check = if (400..500).contains(&status) {
                "BROKEN_INTERNAL_LINK"
            } else if status >= 500 {
                "LINK_TO_5XX"
            } else {
                "INTERNAL_LINK_TO_REDIRECT"
            };
            let (shown, total, sources) = self.locations(target_id, false, false)?;
            out.push(json!({
                "check": check,
                "target_url": url,
                "status_code": status,
                "occurrences_count": sources,
                "locations": shown,
                "details": {
                    "destination_scope": "internal",
                    "link_occurrences": total,
                    "source_pages": sources,
                    "locations_omitted": total - shown.len() as i64,
                },
                "evidence": {
                    "source": "retained hyperlinks and fetched target",
                    "all_locations": "scan-url-links --direction in",
                },
            }));
        }
        Ok(out)
    }

    /// One finding per linking page that has internal links with blank anchor text.
    pub fn empty_anchor_findings(&self) -> Result<Vec<Value>, StoreError> {
        let table = &self.table;
        let mut stmt = self.con.prepare(&format!(
            "SELECT DISTINCT source_url_id,source FROM {table} WHERE internal=1 AND {BLANK_ANCHOR} \
             ORDER BY source_url_id"
        ))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (source_id, source) in rows {
            let (shown, total, _) = self.locations(source_id, true, true)?;
            out.push(json!({
                "check": "INTERNAL_LINK_NO_ANCHOR",
                "target_url": source,
                "occurrences_count": total,
                "locations": shown,
                "details": {
                    "reason": "internal hyperlink has empty retained anchor text",
                    "locations_omitted": total - shown.len() as i64,
                },
            }));
        }
        Ok(out)
    }

    /// The fetched page row for `url` (fragment ignored), if any.
    fn observed(&self, url: &str) -> Result<Option<Hop>, StoreError> {
        Ok(self
            .con
            .query_row(
                "SELECT u.url,p.status_code,p.redirect_url FROM pages p JOIN urls u USING(url_id) \
                 WHERE u.url=?",
                [defrag(url)],
                |r| {
                    Ok(Hop {
                        url: r.get(0)?,
                        status: r.get(1)?,
                        location: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    })
                },
            )
            .optional()?)
    }

    /// Redirect chains (two or more hops, or an unresolved one) and loops, in crawl order.
    pub fn redirect_findings(&self) -> Result<Vec<Value>, StoreError> {
        let mut stmt = self.con.prepare(
            "SELECT p.url_id,u.url,p.status_code,p.redirect_url,p.redirect_chain_json \
             FROM pages p JOIN urls u USING(url_id) \
             WHERE status_code>=300 AND status_code<400 AND redirect_url<>'' ORDER BY page_ordinal",
        )?;
        let firsts = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for (url_id, first_url, first_status, first_location, chain_json) in firsts {
            let mut current = Hop {
                url: first_url.clone(),
                status: Some(first_status),
                location: first_location,
            };
            let mut seen = BTreeSet::new();
            let mut path: Vec<Value> = Vec::new();
            let mut redirects = 0_i64;
            let (last_status, reason, looped): (Option<i64>, &str, bool) = loop {
                if seen.contains(&current.url) {
                    path.push(
                        json!({"url": current.url, "status_code": current.status, "location": current.location}),
                    );
                    break (
                        current.status,
                        "retained redirect path revisits a URL",
                        true,
                    );
                }
                seen.insert(current.url.clone());
                path.push(
                    json!({"url": current.url, "status_code": current.status, "location": current.location}),
                );
                let redirecting = matches!(current.status, Some(s) if (300..400).contains(&s))
                    && !current.location.is_empty();
                if !redirecting {
                    let reason = match current.status {
                        None => "terminal target was not fetched",
                        Some(s) if s >= 400 => "redirect target has an HTTP error",
                        _ => "",
                    };
                    break (current.status, reason, false);
                }
                if redirects >= self.hop_cap {
                    break (
                        current.status,
                        "retained redirect hop budget exhausted",
                        false,
                    );
                }
                redirects += 1;
                let target = urljoin(&current.url, &current.location).unwrap_or_default();
                if let Some(page) = self.observed(&target)? {
                    current = page;
                    continue;
                }
                // List-mode chains can retain a hop without a separate pages row.
                let captured: Value = if chain_json.is_empty() {
                    json!([])
                } else {
                    serde_json::from_str(&chain_json)?
                };
                let hop = captured.as_array().and_then(|hops| {
                    hops.iter()
                        .find(|h| h.get("url").and_then(Value::as_str) == Some(&target))
                });
                current = Hop {
                    status: hop
                        .and_then(|h| h.get("status_code"))
                        .and_then(Value::as_i64),
                    location: hop
                        .and_then(|h| h.get("location").or_else(|| h.get("redirect_url")))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    url: target,
                };
            };
            if !looped && redirects < 2 {
                continue;
            }
            let (shown, total, sources) = self.locations(url_id, false, false)?;
            let statuses: BTreeSet<i64> = path
                .iter()
                .filter_map(|h| h["status_code"].as_i64())
                .filter(|s| (300..400).contains(s))
                .collect();
            let budget = reason == "retained redirect hop budget exhausted";
            let unresolved_end = last_status.is_none() || budget;
            let hop_urls: Vec<&Value> = path.iter().map(|h| &h["url"]).collect();
            let final_url = if looped || unresolved_end {
                Value::Null
            } else {
                path[path.len() - 1]["url"].clone()
            };
            out.push(json!({
                "check": if looped { "REDIRECT_LOOP" } else { "REDIRECT_CHAIN" },
                "target_url": first_url,
                "status_code": first_status,
                "locations": shown,
                "details": {
                    "hops": redirects,
                    "path": hop_urls,
                    "hop_list": path,
                    "final_url": final_url,
                    "terminal_status": last_status,
                    "mixed_permanent_temporary":
                        (statuses.contains(&301) || statuses.contains(&308))
                        && (statuses.contains(&302) || statuses.contains(&303) || statuses.contains(&307)),
                    "unresolved": !reason.is_empty() && !looped && unresolved_end,
                    "reason": reason,
                    "inbound_links": shown,
                    "inbound_links_count": total,
                    "source_pages": sources,
                    "locations_omitted": total - shown.len() as i64,
                },
            }));
        }
        Ok(out)
    }

    /// External destinations whose request failed without a response.
    pub fn external_findings(&self) -> Result<Vec<Value>, StoreError> {
        let has_table: bool = self.con.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='external_checks')",
            [],
            |r| r.get(0),
        )?;
        if !has_table {
            return Ok(Vec::new());
        }
        let mut stmt = self.con.prepare(
            "SELECT payload_json FROM external_checks WHERE outcome='failed' ORDER BY ordinal",
        )?;
        let payloads = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for payload in payloads {
            let item: Value = serde_json::from_str(&payload)?;
            // A safety refusal is not a server failing to answer.
            if NOT_A_FAILURE.contains(&str_field(&item, "error_kind")?) {
                continue;
            }
            let url = str_field(&item, "url")?;
            let source_count = int_field(&item, "source_count")?;
            let sources = item["sources"].as_array().cloned().unwrap_or_default();
            let chain = item["redirect_chain"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let failure_url = chain.last().map_or(json!(url), |hop| hop["url"].clone());
            let locations: Vec<Value> = sources.iter().map(|s| json!({"source_url": s})).collect();
            out.push(json!({
                "check": "EXTERNAL_NO_RESPONSE",
                "target_url": url,
                "occurrences_count": source_count.max(1),
                "locations": locations,
                "details": {
                    "reason": item["reason"],
                    "error": item["error"],
                    "error_kind": item["error_kind"],
                    "failure_url": failure_url,
                    "hop_list": chain,
                    "sources_omitted": source_count - sources.len() as i64,
                },
            }));
        }
        Ok(out)
    }

    /// Every finding: status, empty-anchor, redirect, then external.
    pub fn findings(&self) -> Result<Vec<Value>, StoreError> {
        let mut all = self.status_findings()?;
        all.extend(self.empty_anchor_findings()?);
        all.extend(self.redirect_findings()?);
        all.extend(self.external_findings()?);
        Ok(all)
    }
}

impl Drop for RetainedLinkFindings<'_> {
    fn drop(&mut self) {
        if self.ready {
            let _ = self
                .con
                .execute_batch(&format!("DROP TABLE IF EXISTS temp.{}", self.table));
        }
        let _ = self
            .con
            .execute_batch(&format!("PRAGMA query_only={}", self.before));
        let _ = self.con.remove_function("retained_link_target", 1);
        let _ = self.con.remove_function("retained_link_internal", 1);
    }
}
