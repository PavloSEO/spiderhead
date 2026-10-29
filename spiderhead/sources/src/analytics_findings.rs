//! Web-analytics findings read from an offline crawl/CSV join.
//!
//! The join says which crawled pages have an analytics row and which rows name no
//! crawled page; this module reads that result as four crawl findings. It is pure:
//! it never calls a provider. A value that cannot be read is counted as
//! `unreadable`, never turned into a zero, and a finding that asserts a negative
//! over the whole site is withheld on a partial crawl.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use crate::cloudflare::CfError;
use crate::external_join::orphan_urls;

/// Schema version of the findings document.
pub const SCHEMA_VERSION: &str = "analytics_findings.v1";
/// A bounce rate above this percentage is reported.
pub const BOUNCE_THRESHOLD: f64 = 70.0;

/// Finding id: external-only same-origin URLs.
pub const ORPHAN_URL: &str = "GA_ORPHAN_URL";
/// Finding id: indexable crawled pages with no analytics row.
pub const NO_DATA: &str = "NO_GA_DATA";
/// Finding id: non-indexable crawled pages that still have visits.
pub const NON_INDEXABLE_WITH_DATA: &str = "NON_INDEXABLE_WITH_GA_DATA";
/// Finding id: matched pages with a high bounce rate.
pub const BOUNCE_ABOVE: &str = "BOUNCE_RATE_ABOVE_70";

/// A cell's text the way a spreadsheet export prints it.
fn cell_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        other => other.to_string(),
    }
}

/// reference `float()` on already whitespace-free text: underscores are allowed only
/// between digits.
fn parse_float(text: &str) -> Option<f64> {
    let bytes: Vec<char> = text.chars().collect();
    let mut cleaned = String::with_capacity(text.len());
    for (index, ch) in bytes.iter().enumerate() {
        if *ch == '_' {
            let between = index > 0
                && index + 1 < bytes.len()
                && bytes[index - 1].is_ascii_digit()
                && bytes[index + 1].is_ascii_digit();
            if !between {
                return None;
            }
            continue;
        }
        cleaned.push(*ch);
    }
    if cleaned.is_empty() {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

fn compact(value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    Some(cell_text(value).split_whitespace().collect())
}

/// A bounce rate as a percentage in 0..=100, or `None` when unreadable. `"65"`, `"65%"`
/// and `"65,5 %"` are percentages; a decimal not above 1 (`"0.65"`) is a fraction; a bare
/// integer is always a percentage.
pub fn parse_percent(value: &Value) -> Option<f64> {
    let text = compact(value)?.replace(',', ".");
    let is_percent = text.ends_with('%');
    let text = if is_percent {
        text[..text.len() - 1].to_owned()
    } else {
        text
    };
    let number = parse_float(&text)?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    if is_percent || !text.contains('.') || number > 1.0 {
        return (number <= 100.0).then_some(number);
    }
    Some(number * 100.0)
}

/// A non-negative visit count, or `None` when unreadable.
pub fn parse_count(value: &Value) -> Option<f64> {
    let text = compact(value)?;
    let number = parse_float(&text)?;
    (number.is_finite() && number >= 0.0).then_some(number)
}

/// `Some(true)` / `Some(false)` from the crawl's Indexability column, `None` if absent.
fn is_indexable(page: &Value) -> Option<bool> {
    let value = page
        .get("Indexability")
        .or_else(|| page.get("indexability"))?;
    if value.is_null() {
        return None;
    }
    match cell_text(value).trim().to_lowercase().as_str() {
        "indexable" => Some(true),
        "non-indexable" => Some(false),
        _ => None,
    }
}

fn finding(
    state: &str,
    reason: &str,
    urls: impl IntoIterator<Item = String>,
    unreadable: usize,
) -> Value {
    let sorted: BTreeSet<String> = urls.into_iter().collect();
    json!({
        "state": state,
        "reason": reason,
        "urls": sorted.into_iter().collect::<Vec<_>>(),
        "unreadable": unreadable,
    })
}

/// Inputs of [`analytics_findings`] beyond the pages and the join result.
#[derive(Debug, Clone, Default)]
pub struct FindingOptions {
    /// The crawl stopped early.
    pub partial: bool,
    /// `None` means complete; `Some(false)` withholds whole-site negative claims.
    pub evidence_complete: Option<bool>,
    /// Why the evidence is incomplete; overrides the default withheld reason.
    pub evidence_reason: Option<String>,
    /// A provider history explicitly declaring complete empty days may report zero hits.
    pub empty_is_complete: bool,
    /// Optional column naming per-URL visits.
    pub visits_column: Option<String>,
    /// Optional column naming per-URL bounce rate.
    pub bounce_column: Option<String>,
    /// Why the bounce column is absent; overrides the default skipped reason.
    pub bounce_reason: Option<String>,
}

/// The four analytics findings, each with a `state` (`reported`, `withheld`, `skipped`)
/// and a `reason`.
pub fn analytics_findings(
    pages: &[Value],
    join_result: &Value,
    options: &FindingOptions,
) -> Result<Value, CfError> {
    // Last page wins per URL, like a dict comprehension keyed by `page.get("url")`.
    let mut by_url: Vec<(Value, &Value)> = Vec::new();
    for page in pages {
        let url = page.get("url").cloned().unwrap_or(Value::Null);
        match by_url.iter_mut().find(|(key, _)| *key == url) {
            Some(slot) => slot.1 = page,
            None => by_url.push((url, page)),
        }
    }
    let joined: Vec<Value> = join_result
        .get("joined")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let rows_total = join_result["summary"]
        .get("rows")
        .and_then(Value::as_i64)
        .unwrap_or(0);

    let evidence_complete = options.evidence_complete.unwrap_or(true);
    let evidence_reason = || {
        options
            .evidence_reason
            .clone()
            .unwrap_or_else(|| "analytics history coverage is incomplete".to_owned())
    };
    let orphans = if options.partial {
        finding(
            "withheld",
            "crawl is partial; an external-only URL cannot be proven an orphan",
            [],
            0,
        )
    } else if !evidence_complete {
        finding("withheld", &evidence_reason(), [], 0)
    } else {
        finding(
            "reported",
            "crawl completed; external-only same-origin URLs",
            orphan_urls(join_result, "url", None)?,
            0,
        )
    };

    let no_data = if options.partial {
        finding(
            "withheld",
            "crawl is partial; absence of analytics data cannot be established",
            [],
            0,
        )
    } else if !evidence_complete {
        finding("withheld", &evidence_reason(), [], 0)
    } else if rows_total == 0 && !options.empty_is_complete {
        finding(
            "skipped",
            "analytics input has no rows; a no-data claim would not be meaningful",
            [],
            0,
        )
    } else {
        let verdicts: Vec<(String, Option<bool>)> = join_result
            .get("crawl_only")
            .and_then(Value::as_array)
            .map(|urls| {
                urls.iter()
                    .map(|url| {
                        let page = by_url
                            .iter()
                            .find(|(key, _)| key == url)
                            .map(|(_, page)| is_indexable(page))
                            .unwrap_or(None);
                        (url.as_str().unwrap_or_default().to_owned(), page)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let unreadable = verdicts.iter().filter(|(_, v)| v.is_none()).count();
        finding(
            "reported",
            "indexable crawled pages with no analytics row",
            verdicts
                .into_iter()
                .filter(|(_, v)| *v == Some(true))
                .map(|(url, _)| url),
            unreadable,
        )
    };

    let non_indexable = match &options.visits_column {
        None => finding("skipped", "visits_column not given", [], 0),
        Some(column) => {
            let mut hits = Vec::new();
            let mut unreadable = 0;
            for entry in &joined {
                let indexable = is_indexable(&entry["page"]);
                let visits = entry["external"].get(column).and_then(parse_count);
                match (visits, indexable) {
                    (Some(visits), Some(false)) if visits > 0.0 => {
                        hits.push(entry["url"].as_str().unwrap_or_default().to_owned());
                    }
                    (None, _) | (_, None) => unreadable += 1,
                    _ => {}
                }
            }
            finding(
                "reported",
                "non-indexable crawled pages with visits above zero",
                hits,
                unreadable,
            )
        }
    };

    let bounce = match &options.bounce_column {
        None => finding(
            "skipped",
            &options.bounce_reason.clone().unwrap_or_else(|| {
                "bounce_column not given; per-URL bounce needs a per-URL export (#990, #984)"
                    .to_owned()
            }),
            [],
            0,
        ),
        Some(column) => {
            let mut hits = Vec::new();
            let mut unreadable = 0;
            for entry in &joined {
                match entry["external"].get(column).and_then(parse_percent) {
                    None => unreadable += 1,
                    Some(rate) if rate > BOUNCE_THRESHOLD => {
                        hits.push(entry["url"].as_str().unwrap_or_default().to_owned());
                    }
                    Some(_) => {}
                }
            }
            finding(
                "reported",
                "matched pages with bounce rate above 70%",
                hits,
                unreadable,
            )
        }
    };

    let mut findings = Map::new();
    findings.insert(ORPHAN_URL.into(), orphans);
    findings.insert(NO_DATA.into(), no_data);
    findings.insert(NON_INDEXABLE_WITH_DATA.into(), non_indexable);
    findings.insert(BOUNCE_ABOVE.into(), bounce);
    Ok(json!({"schema_version": SCHEMA_VERSION, "findings": Value::Object(findings)}))
}
