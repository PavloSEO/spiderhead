//! Plain data of an audit: pages, findings, groups, skipped checks.

use serde_json::{Map, Value};

/// One crawled URL with the metrics the rules read (reference `models.Page`).
#[derive(Debug, Clone, Default)]
pub struct Page {
    /// Address as stored in the scan.
    pub url: String,
    /// HTTP status, when a response arrived.
    pub status_code: Option<i64>,
    /// Response `Content-Type`.
    pub content_type: String,
    /// `Indexable` / `Non-Indexable`.
    pub indexability: String,
    /// Reason accompanying a non-indexable verdict.
    pub indexability_status: String,
    /// Metrics derived during the audit (`bytes_per_word`, `size_vs_median_ratio`, ...).
    pub metrics: Map<String, Value>,
    /// Check ids of the findings on this page (filled by aggregation).
    pub issues: Vec<String>,
    /// Ids of the findings on this page (filled by aggregation).
    pub issue_ids: Vec<String>,
    /// The page's own record: title, description, word count, ... (reference `metrics["_record"]`).
    pub rec: Map<String, Value>,
}

impl Page {
    /// Content-Type names HTML.
    pub fn is_html(&self) -> bool {
        self.content_type.to_lowercase().contains("html")
    }

    /// A 2xx answer.
    pub fn is_2xx(&self) -> bool {
        self.status_code.is_some_and(|c| (200..300).contains(&c))
    }

    /// Indexability reads `Indexable`.
    pub fn is_indexable(&self) -> bool {
        self.indexability.trim().to_lowercase() == "indexable"
    }

    /// A text field of the record ("" when absent or not a string).
    pub fn text(&self, key: &str) -> &str {
        self.rec.get(key).and_then(Value::as_str).unwrap_or("")
    }

    /// A text field as the analyzer sees it: blank strings read as `null`.
    pub fn opt(&self, key: &str) -> Value {
        match self.rec.get(key) {
            Some(Value::String(s)) if s.is_empty() => Value::Null,
            Some(v) => v.clone(),
            None => Value::Null,
        }
    }

    /// A numeric field of the record.
    pub fn num(&self, key: &str) -> Option<f64> {
        self.rec.get(key).and_then(Value::as_f64)
    }

    /// The row's HTML body was not parsed or retained (`body_unavailable` marker).
    pub fn body_unavailable(&self) -> bool {
        self.rec.get("body_unavailable").is_some_and(|v| match v {
            Value::Null => false,
            Value::String(s) => !s.is_empty(),
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_f64() != Some(0.0),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
        })
    }

    /// reference `Page.to_json`: url, status, verdict, metrics and issue backlinks.
    pub fn to_json(&self) -> Map<String, Value> {
        let mut out = Map::new();
        out.insert("url".into(), Value::from(self.url.clone()));
        out.insert(
            "status_code".into(),
            self.status_code.map_or(Value::Null, Value::from),
        );
        out.insert(
            "indexability".into(),
            Value::from(self.indexability.clone()),
        );
        out.insert(
            "indexability_status".into(),
            if self.indexability_status.is_empty() {
                Value::Null
            } else {
                Value::from(self.indexability_status.clone())
            },
        );
        out.insert(
            "content_type".into(),
            Value::from(self.content_type.clone()),
        );
        out.insert("metrics".into(), Value::Object(self.metrics.clone()));
        out.insert("issues".into(), Value::from(self.issues.clone()));
        out.insert("issue_ids".into(), Value::from(self.issue_ids.clone()));
        out
    }

    /// `_metrics_from_record`: the public per-page metrics block derived from the record.
    /// Passes fill `size_vs_median_ratio`, `bytes_per_word`, `dom_depth`, `dom_nodes`,
    /// `is_in_sitemap`, `sitemap_lastmod` and `link_score_computed` later.
    pub fn fill_metrics(&mut self) {
        let rec = &self.rec;
        // `records_from_df` maps blank cells to None.
        let get = |k: &str| match rec.get(k) {
            Some(Value::String(s)) if s.trim().is_empty() => Value::Null,
            other => other.cloned().unwrap_or(Value::Null),
        };
        let text = |k: &str| rec.get(k).and_then(Value::as_str).unwrap_or("");
        let h1_count = i64::from(!text("h1").is_empty()) + i64::from(!text("h1_2").is_empty());
        let h2_count = i64::from(!text("h2").is_empty()) + i64::from(!text("h2_2").is_empty());
        let h1: Vec<Value> = ["h1", "h1_2"]
            .iter()
            .filter_map(|k| match rec.get(*k) {
                Some(Value::String(s)) if !s.is_empty() => Some(Value::from(s.clone())),
                _ => None,
            })
            .collect();
        let mut m = Map::new();
        for (k, v) in [
            ("title", get("title")),
            ("title_length", get("title_length")),
            ("title_px", get("title_px")),
            ("meta_description", get("meta_description")),
            ("desc_length", get("desc_length")),
            ("h1_count", Value::from(h1_count)),
            ("h1", Value::Array(h1)),
            ("h2_count", Value::from(h2_count)),
            ("word_count", get("word_count")),
            ("text_ratio", get("text_ratio")),
            ("size_bytes", get("size_bytes")),
            ("size_vs_median_ratio", Value::Null),
            ("bytes_per_word", Value::Null),
            ("dom_depth", Value::Null),
            ("dom_nodes", Value::Null),
            ("inlinks", get("inlinks")),
            ("unique_inlinks", get("unique_inlinks")),
            ("outlinks", get("outlinks")),
            ("external_outlinks", get("external_outlinks")),
            ("crawl_depth", get("crawl_depth")),
            ("response_time", get("response_time")),
            ("canonical", get("canonical")),
            ("meta_robots", get("meta_robots")),
            ("link_score", get("link_score")),
            ("is_in_sitemap", Value::Null),
            ("sitemap_lastmod", Value::Null),
            (
                "representation",
                match rec.get("representation") {
                    Some(Value::String(s)) if !s.is_empty() => Value::from(s.clone()),
                    _ => Value::from("static"),
                },
            ),
        ] {
            m.insert(k.into(), v);
        }
        self.metrics = m;
    }
}

/// One finding (reference `models.Issue`).
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    /// Check identifier.
    pub check: String,
    /// Severity name.
    pub severity: String,
    /// Evidence source tag.
    pub source: String,
    /// Message shown with the finding.
    pub message: String,
    /// Fix hint from the registry.
    pub fix_hint: Option<String>,
    /// Page the finding is about.
    pub target_url: Option<String>,
    /// Status code attached to the finding.
    pub status_code: Option<i64>,
    /// How many occurrences the finding stands for.
    pub occurrences_count: i64,
    /// Check-specific details, key order preserved.
    pub details: Map<String, Value>,
    /// Bounded sample of where the problem sits (links, sources).
    pub locations: Vec<Value>,
    /// Group this finding belongs to.
    pub group_id: Option<String>,
    /// `ISSUE-NNNNNN`, assigned by aggregation.
    pub id: Option<String>,
    /// 12-hex fingerprint of (check, target, status), assigned by aggregation.
    pub fingerprint: Option<String>,
    /// Check-declared evidence hints (`evidence=` kwarg); the contract block is added later.
    pub evidence: Map<String, Value>,
}

impl Issue {
    /// reference `Issue.to_json`: optional keys appear only when they carry a value.
    pub fn to_json(&self) -> Map<String, Value> {
        let mut out = Map::new();
        out.insert(
            "id".into(),
            self.id.clone().map_or(Value::Null, Value::from),
        );
        out.insert(
            "fingerprint".into(),
            self.fingerprint.clone().map_or(Value::Null, Value::from),
        );
        out.insert("check".into(), Value::from(self.check.clone()));
        out.insert("severity".into(), Value::from(self.severity.clone()));
        out.insert("source".into(), Value::from(self.source.clone()));
        out.insert("message".into(), Value::from(self.message.clone()));
        out.insert(
            "target_url".into(),
            self.target_url.clone().map_or(Value::Null, Value::from),
        );
        out.insert(
            "status_code".into(),
            self.status_code.map_or(Value::Null, Value::from),
        );
        out.insert(
            "occurrences_count".into(),
            Value::from(self.occurrences_count),
        );
        if !self.locations.is_empty() {
            out.insert("locations".into(), Value::Array(self.locations.clone()));
        }
        if !self.details.is_empty() {
            out.insert("details".into(), Value::Object(self.details.clone()));
        }
        if let Some(group_id) = &self.group_id {
            out.insert("group_id".into(), Value::from(group_id.clone()));
        }
        if let Some(fix_hint) = &self.fix_hint {
            out.insert("fix_hint".into(), Value::from(fix_hint.clone()));
        }
        if !self.evidence.is_empty() {
            out.insert("evidence".into(), Value::Object(self.evidence.clone()));
        }
        out
    }
    /// `image_source`: the page an `IMG_*` finding's markup evidence came from.
    pub fn image_source(&self) -> Option<&str> {
        if !self.check.starts_with("IMG_") {
            return None;
        }
        match self.details.get("source_page") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
            _ => None,
        }
    }

    /// `dedupe_key`: `(check, target, image_source, target_scope)` — document ids deliberately
    /// do not participate, source pages and image scopes do.
    pub fn dedupe_key(&self) -> (String, String, String, String) {
        let source = self.image_source().unwrap_or("");
        let scope = if source.is_empty() {
            ""
        } else {
            self.details
                .get("target_scope")
                .and_then(Value::as_str)
                .unwrap_or("")
        };
        (
            self.check.clone(),
            self.target_url.clone().unwrap_or_default(),
            source.to_owned(),
            scope.to_owned(),
        )
    }

    /// `merge`: fold a repeated dedupe identity into this finding.
    pub fn merge(&mut self, other: &Issue) {
        if self.image_source().is_some() {
            // Native image groups stay source-bound: locations cap at 200 and
            // `locations_omitted` keeps the honest total; occurrences add up.
            let omitted = |i: &Issue| -> i64 {
                i.details
                    .get("locations_omitted")
                    .and_then(Value::as_i64)
                    .filter(|v| *v >= 0)
                    .unwrap_or(0)
            };
            let total = self.locations.len() as i64
                + omitted(self)
                + other.locations.len() as i64
                + omitted(other);
            self.locations.truncate(200);
            let room = 200usize.saturating_sub(self.locations.len());
            self.locations
                .extend(other.locations.iter().take(room).cloned());
            self.details.insert(
                "locations_omitted".into(),
                Value::from(total - self.locations.len() as i64),
            );
            self.occurrences_count += other.occurrences_count;
            return;
        }
        self.locations.extend(other.locations.iter().cloned());
        let unique: std::collections::HashSet<&str> = self
            .locations
            .iter()
            .filter_map(|loc| {
                loc.get("source_url")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            })
            .collect();
        self.occurrences_count = self
            .occurrences_count
            .max(other.occurrences_count)
            .max(unique.len() as i64);
    }
}

/// Pages sharing one defect value (reference `models.Group`).
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// `GRP-<PREFIX>-<seq>`.
    pub group_id: String,
    /// Check identifier.
    pub check: String,
    /// Shared value, when the group has one.
    pub value: Option<String>,
    /// Member URLs.
    pub urls: Vec<String>,
}

impl Group {
    /// `Group.to_json`: `urls` is the member array the audit.v2 writer lifts into
    /// `group_members` and replaces with the group marker.
    pub fn to_json(&self) -> Map<String, Value> {
        let mut out = Map::new();
        out.insert("group_id".into(), Value::from(self.group_id.clone()));
        out.insert("check".into(), Value::from(self.check.clone()));
        out.insert(
            "value".into(),
            self.value.clone().map_or(Value::Null, Value::from),
        );
        out.insert("urls".into(), Value::from(self.urls.clone()));
        out.insert("count".into(), Value::from(self.urls.len() as i64));
        out
    }
}

/// A check that could not be answered, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedCheck {
    /// Check identifier.
    pub id: String,
    /// Why it was not evaluated.
    pub reason: String,
}

/// `norm_url`: strip, drop the trailing slash of the path, fold scheme and host case.
pub fn norm_url(url: &str) -> String {
    let url = url.trim();
    if url.is_empty() {
        return String::new();
    }
    match spiderhead_parse::py_url::urlsplit(url) {
        Ok(s) => spiderhead_parse::py_url::urlunsplit(
            &s.scheme.to_lowercase(),
            &s.netloc.to_lowercase(),
            s.path.trim_end_matches('/'),
            &s.query,
            &s.fragment,
        ),
        Err(_) => url.to_owned(),
    }
}
