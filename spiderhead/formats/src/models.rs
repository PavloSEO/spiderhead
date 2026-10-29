//! Normalized data model shared by every stage of the pipeline.
//!
//! The model is deliberately source-agnostic: the rule engine never knows whether the data
//! came from a live crawl or from exports parsed off disk. It only sees [`Page`], [`Link`]
//! and the issues and groups derived from them.
//!
//! JSON objects are kept as insertion-ordered [`serde_json`] maps.

use serde_json::{Map, Value};
use std::collections::HashSet;

mod audit_v2;

#[cfg(test)]
mod tests;

pub use audit_v2::{escape_pointer_token, AuditV2Parts, RowIter, Rows};

/// A JSON object: the shape every `to_json` in this module produces.
pub type Object = Map<String, Value>;

/// The severities an issue may carry, lowest to highest.
pub const SEVERITIES: [&str; 4] = ["critical", "warning", "notice", "opportunity"];

/// Most locations a merged image issue keeps inline before counting the rest as omitted.
const IMAGE_LOCATION_CAP: usize = 200;

/// Write an optional scalar as a JSON value, `null` when absent.
fn json_opt<T: Clone + Into<Value>>(value: &Option<T>) -> Value {
    value.clone().map_or(Value::Null, Into::into)
}

/// Truthiness of a JSON value: `None`, `false`, zero, empty string, array and object
/// are false; everything else is true.
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_none_or(|float| float != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// A single link instance, as found in a bulk inlinks export.
///
/// Carries the localization that answers "where is this link, where does it go, where in the
/// DOM": source page, anchor, semantic position and XPath path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Link {
    /// The page the link appears on.
    pub source_url: Option<String>,
    /// The URL the link points to.
    pub destination_url: Option<String>,
    /// Visible anchor text.
    pub anchor: Option<String>,
    /// Alternative text of an image link.
    pub alt_text: Option<String>,
    /// HTTP status of the destination, when known.
    pub status_code: Option<i64>,
    /// Semantic position: Navigation, Content, Sidebar or Footer.
    pub link_position: Option<String>,
    /// XPath of the link in the DOM.
    pub link_path: Option<String>,
    /// Whether the link is followed (no `nofollow`).
    pub follow: Option<bool>,
    /// The `rel` attribute.
    pub rel: Option<String>,
    /// The `target` attribute.
    pub target: Option<String>,
}

impl Link {
    /// The location fields of the link, without the destination.
    pub fn as_location(&self) -> Object {
        let mut out = Object::new();
        out.insert("source_url".into(), json_opt(&self.source_url));
        out.insert("anchor".into(), json_opt(&self.anchor));
        out.insert("alt_text".into(), json_opt(&self.alt_text));
        out.insert("link_position".into(), json_opt(&self.link_position));
        out.insert("link_path".into(), json_opt(&self.link_path));
        out.insert("follow".into(), json_opt(&self.follow));
        out.insert("rel".into(), json_opt(&self.rel));
        out.insert("target".into(), json_opt(&self.target));
        out
    }
}

/// One crawled URL with its normalized metrics.
///
/// `metrics` holds typed, JSON-ready values (numbers as numbers, `None` for blanks).
/// `issues` and `issue_ids` are filled in by the aggregator so each page links back to the
/// issues that reference it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    /// The crawled URL.
    pub url: String,
    /// HTTP status code, when a response was received.
    pub status_code: Option<i64>,
    /// Free-text status, when the source carries one.
    pub status: Option<String>,
    /// The `Content-Type` header as received.
    pub content_type: Option<String>,
    /// Indexability verdict, such as `Indexable`.
    pub indexability: Option<String>,
    /// Reason behind the indexability verdict.
    pub indexability_status: Option<String>,
    /// Typed metrics of the page.
    pub metrics: Object,
    /// Human-readable issue references.
    pub issues: Vec<String>,
    /// Ids of the issues that reference this page.
    pub issue_ids: Vec<String>,
    /// Ids of issues suppressed for this page.
    pub suppressed_issue_ids: Vec<String>,
}

impl Page {
    /// A page with only its URL set.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Self::default()
        }
    }

    /// Whether the `Content-Type` mentions HTML. A pure Content-Type read: a 301 or 404 can
    /// answer with an HTML type too, so use [`Page::is_2xx`] alongside it where that matters.
    pub fn is_html(&self) -> bool {
        self.content_type
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            .contains("html")
    }

    /// Whether the status code is in the 2xx range.
    pub fn is_2xx(&self) -> bool {
        self.status_code
            .is_some_and(|code| (200..300).contains(&code))
    }

    /// Whether the indexability verdict is `indexable`, ignoring case and surrounding space.
    pub fn is_indexable(&self) -> bool {
        self.indexability
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_lowercase()
            == "indexable"
    }

    /// The audit.v2 page row. `suppressed_issue_ids` appears only when non-empty.
    pub fn to_json(&self) -> Object {
        let mut out = Object::new();
        out.insert("url".into(), Value::String(self.url.clone()));
        out.insert("status_code".into(), json_opt(&self.status_code));
        out.insert("indexability".into(), json_opt(&self.indexability));
        out.insert(
            "indexability_status".into(),
            json_opt(&self.indexability_status),
        );
        out.insert("content_type".into(), json_opt(&self.content_type));
        out.insert("metrics".into(), Value::Object(self.metrics.clone()));
        out.insert("issues".into(), string_array(&self.issues));
        out.insert("issue_ids".into(), string_array(&self.issue_ids));
        if !self.suppressed_issue_ids.is_empty() {
            out.insert(
                "suppressed_issue_ids".into(),
                string_array(&self.suppressed_issue_ids),
            );
        }
        out
    }
}

fn string_array(items: &[String]) -> Value {
    Value::Array(items.iter().cloned().map(Value::String).collect())
}

/// Identity of an issue for de-duplication. Two issues with the same key merge.
///
/// Document ids differ between runs; source URLs and image scope do not.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DedupeKey {
    /// The check id.
    pub check: String,
    /// The URL the issue is about.
    pub target_url: Option<String>,
    /// The page an image issue was found on.
    pub image_source: Option<String>,
    /// The image scope, only meaningful for image issues.
    pub scope: Option<String>,
}

/// A single detected problem, localized and traceable to its source export.
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    /// The check id, such as `TITLE_MISSING`.
    pub check: String,
    /// One of [`SEVERITIES`].
    pub severity: String,
    /// Where the issue was read from.
    pub source: String,
    /// Human-readable message.
    pub message: String,
    /// Stable document id.
    pub id: Option<String>,
    /// Stable hash for diffing runs.
    pub fingerprint: Option<String>,
    /// The URL the issue is about.
    pub target_url: Option<String>,
    /// HTTP status of the target, when known.
    pub status_code: Option<i64>,
    /// How many occurrences the issue stands for.
    pub occurrences_count: i64,
    /// Where the issue occurs, one object per location.
    pub locations: Vec<Object>,
    /// Check-specific details.
    pub details: Object,
    /// Group the issue belongs to, if any.
    pub group_id: Option<String>,
    /// A short hint on how to fix the issue.
    pub fix_hint: Option<String>,
    /// Evidence the verdict rests on.
    pub evidence: Object,
}

impl Issue {
    /// An issue with the four required fields and one occurrence.
    pub fn new(
        check: impl Into<String>,
        severity: impl Into<String>,
        source: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            check: check.into(),
            severity: severity.into(),
            source: source.into(),
            message: message.into(),
            id: None,
            fingerprint: None,
            target_url: None,
            status_code: None,
            occurrences_count: 1,
            locations: Vec::new(),
            details: Object::new(),
            group_id: None,
            fix_hint: None,
            evidence: Object::new(),
        }
    }

    /// The page an image issue was found on: only for `IMG_` checks with a non-empty
    /// `details.source_page` string.
    pub fn image_source(&self) -> Option<&str> {
        if !self.check.starts_with("IMG_") {
            return None;
        }
        self.details
            .get("source_page")
            .and_then(Value::as_str)
            .filter(|page| !page.is_empty())
    }

    /// The de-duplication identity of the issue.
    pub fn dedupe_key(&self) -> DedupeKey {
        let image_source = self.image_source().map(str::to_owned);
        let scope = if image_source.is_some() {
            self.details
                .get("target_scope")
                .and_then(Value::as_str)
                .map(str::to_owned)
        } else {
            None
        };
        DedupeKey {
            check: self.check.clone(),
            target_url: self.target_url.clone(),
            image_source,
            scope,
        }
    }

    /// Fold one duplicate into this issue.
    ///
    /// Native image issues stay bound to their source page: their locations are kept up to
    /// [`IMAGE_LOCATION_CAP`] and the rest counted in `details.locations_omitted`. Other
    /// issues append their locations and take the larger of the distinct source count and
    /// the two occurrence counts.
    pub fn merge(&mut self, other: &Issue) {
        if self.image_source().is_some() {
            let left = omitted_count(&self.details);
            let right = omitted_count(&other.details);
            let total = self.locations.len() + left + other.locations.len() + right;
            self.locations.truncate(IMAGE_LOCATION_CAP);
            let room = IMAGE_LOCATION_CAP.saturating_sub(self.locations.len());
            self.locations
                .extend(other.locations.iter().take(room).cloned());
            let omitted = total - self.locations.len();
            self.details
                .insert("locations_omitted".into(), Value::from(omitted as i64));
            self.occurrences_count += other.occurrences_count;
            return;
        }
        self.locations.extend(other.locations.iter().cloned());
        let unique_sources: HashSet<String> = self
            .locations
            .iter()
            .filter_map(|location| location.get("source_url"))
            .filter(|value| is_truthy(value))
            .map(Value::to_string)
            .collect();
        let distinct = unique_sources.len() as i64;
        self.occurrences_count = distinct
            .max(self.occurrences_count)
            .max(other.occurrences_count);
    }

    /// The audit.v2 issue row. Empty optional collections are left out.
    pub fn to_json(&self) -> Object {
        let mut out = Object::new();
        out.insert("id".into(), json_opt(&self.id));
        out.insert("fingerprint".into(), json_opt(&self.fingerprint));
        out.insert("check".into(), Value::String(self.check.clone()));
        out.insert("severity".into(), Value::String(self.severity.clone()));
        out.insert("source".into(), Value::String(self.source.clone()));
        out.insert("message".into(), Value::String(self.message.clone()));
        out.insert("target_url".into(), json_opt(&self.target_url));
        out.insert("status_code".into(), json_opt(&self.status_code));
        out.insert(
            "occurrences_count".into(),
            Value::from(self.occurrences_count),
        );
        if !self.locations.is_empty() {
            let locations = self.locations.iter().cloned().map(Value::Object).collect();
            out.insert("locations".into(), Value::Array(locations));
        }
        if !self.details.is_empty() {
            out.insert("details".into(), Value::Object(self.details.clone()));
        }
        if let Some(group_id) = self.group_id.as_deref().filter(|id| !id.is_empty()) {
            out.insert("group_id".into(), Value::String(group_id.to_owned()));
        }
        if let Some(hint) = self.fix_hint.as_deref().filter(|hint| !hint.is_empty()) {
            out.insert("fix_hint".into(), Value::String(hint.to_owned()));
        }
        if !self.evidence.is_empty() {
            out.insert("evidence".into(), Value::Object(self.evidence.clone()));
        }
        out
    }
}

/// `details.locations_omitted` when it is a non-negative integer, otherwise zero.
fn omitted_count(details: &Object) -> usize {
    details
        .get("locations_omitted")
        .and_then(Value::as_i64)
        .filter(|count| *count >= 0)
        .map_or(0, |count| count as usize)
}

/// A cluster of URLs sharing a value (duplicate title, description or hash).
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// Stable id of the group.
    pub group_id: String,
    /// The check the group belongs to.
    pub check: String,
    /// The shared value, when there is one.
    pub value: Option<String>,
    /// The URLs in the group.
    pub urls: Vec<String>,
    /// How many URLs the group counts.
    pub count: i64,
}

impl Group {
    /// The audit.v2 group row, with `urls` as a JSON array.
    pub fn to_json(&self) -> Object {
        let mut out = Object::new();
        out.insert("group_id".into(), Value::String(self.group_id.clone()));
        out.insert("check".into(), Value::String(self.check.clone()));
        out.insert("value".into(), json_opt(&self.value));
        out.insert("urls".into(), string_array(&self.urls));
        out.insert("count".into(), Value::from(self.count));
        out
    }
}

/// A check that did not run, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedCheck {
    /// The check id.
    pub id: String,
    /// Why it did not run.
    pub reason: String,
}

impl SkippedCheck {
    /// The `{id, reason}` row written into `run.checks_skipped` and `run.checks_disabled`.
    pub fn to_json(&self) -> Object {
        let mut out = Object::new();
        out.insert("id".into(), Value::String(self.id.clone()));
        out.insert("reason".into(), Value::String(self.reason.clone()));
        out
    }
}

/// The full audit, the single contract every reporter consumes.
#[derive(Debug, Clone, Default)]
pub struct AuditResult {
    /// Run metadata: mode, timestamps, inputs.
    pub run: Object,
    /// Totals and roll-ups.
    pub summary: Object,
    /// Every detected issue.
    pub issues: Vec<Issue>,
    /// Every crawled page.
    pub pages: Vec<Page>,
    /// Every group of URLs.
    pub groups: Vec<Group>,
    /// Checks that were skipped.
    pub skipped: Vec<SkippedCheck>,
    /// Checks that were disabled by configuration.
    pub disabled: Vec<SkippedCheck>,
    /// Issues suppressed by configuration, kept as written.
    pub suppressed_issues: Vec<Value>,
}

impl AuditResult {
    /// The compatibility JSON document: header plus every collection restored in place.
    pub fn to_json(&self) -> Object {
        let parts = self.audit_v2_parts();
        let mut document = parts.header;
        for (pointer, rows) in &parts.collections {
            let items: Vec<Value> = rows.iter().collect();
            audit_v2::set_collection(&mut document, pointer, items);
        }
        document
    }
}
