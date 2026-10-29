//! `AuditContext`: thresholds, findings, groups and skips (reference the reference suite).

use std::collections::BTreeSet;

use serde_json::{Map, Value};
use spiderhead_formats::check_registry;

use super::model::{Group, Issue, Page, SkippedCheck};

/// Analyzer fields a native stored scan always measures: the
/// `INTERNAL_FIELD_MAP` keys whose canonical column the reference suite
/// emits, plus the graph-derived inlink counts. `has_column` answers true for
/// exactly this set on every retained native audit.
pub const NATIVE_FIELDS: &[&str] = &[
    "url",
    "content_type",
    "status_code",
    "status",
    "indexability",
    "indexability_status",
    "body_unavailable",
    "title",
    "title_length",
    "meta_description",
    "desc_length",
    "meta_description_count",
    "h1",
    "h1_length",
    "h1_2",
    "h1_alt_text",
    "h2",
    "heading_outline",
    "invalid_link_targets",
    "sentence_count",
    "avg_words_per_sentence",
    "flesch",
    "readability",
    "link_placement",
    "trust_signals",
    "duplicate_ids",
    "canonical",
    "amphtml",
    "meta_robots",
    "x_robots",
    "content_encoding",
    "render_blocking",
    "http_refresh",
    "doctype",
    "viewport",
    "meta_refresh",
    "meta_charset",
    "title_outside_head",
    "meta_description_outside_head",
    "canonical_outside_head",
    "directives_outside_head",
    "hreflang_outside_head",
    "head_count",
    "body_count",
    "head_not_first",
    "invalid_head_elements",
    "og_title",
    "og_description",
    "og_image",
    "og_url",
    "size_bytes",
    "word_count",
    "content_frames",
    "content_frames_same_origin",
    "text_ratio",
    "outlinks",
    "external_outlinks",
    "response_time",
    "redirect_url",
    "crawl_depth",
    "structured_data",
    "structured_data_parsed",
    "images_total",
    "images_missing_alt_attr",
    "images_max_alt_length",
    "lorem_ipsum_count",
    "plugin_elements",
    "mobile_alternate_broken",
    "meta_fragment",
    "ajax_scheme_outlinks",
    "representation",
    "inlinks",
    "unique_inlinks",
];

/// Default thresholds of `sf.config.load_config` that the ported rules read.
#[derive(Debug, Clone)]
pub struct Thresholds {
    /// `title_max_chars`.
    pub title_max_chars: f64,
    /// `title_min_chars`.
    pub title_min_chars: f64,
    /// `desc_max_chars`.
    pub desc_max_chars: f64,
    /// `desc_min_chars`.
    pub desc_min_chars: f64,
    /// `thin_content_words`.
    pub thin_content_words: f64,
    /// `low_text_ratio_pct`.
    pub low_text_ratio_pct: f64,
    /// `readability_flesch_min`.
    pub readability_flesch_min: f64,
    /// `large_html_abs_kb`.
    pub large_html_abs_kb: f64,
    /// `large_html_x_median`.
    pub large_html_x_median: f64,
    /// `bytes_per_word_x_median`.
    pub bytes_per_word_x_median: f64,
    /// `templated_title_share`.
    pub templated_title_share: f64,
    /// `dom_depth_max`.
    pub dom_depth_max: f64,
    /// `dom_nodes_max`.
    pub dom_nodes_max: f64,
    /// `alt_max_chars`.
    pub alt_max_chars: f64,
    /// `img_max_kb`.
    pub img_max_kb: f64,
    /// `near_duplicate_similarity`.
    pub near_duplicate_similarity: f64,
    /// `semantic_similarity.threshold`.
    pub semantic_threshold: f64,
    /// `semantic_similarity.relevance_threshold`.
    pub semantic_relevance_threshold: f64,
    /// `semantic_similarity.max_documents`.
    pub semantic_max_documents: i64,
    /// `semantic_similarity.max_candidate_comparisons`.
    pub semantic_max_candidate_comparisons: i64,
    /// `thresholds.link_score_low_ratio` (default 0.25).
    pub link_score_low_ratio: f64,
    /// `thresholds.crawl_depth_max` (default 4): DEEP_CLICK_DEPTH budget.
    pub crawl_depth_max: i64,
    /// Threshold keys without a declared field (`h2_max_chars`, `url_max_chars`,
    /// ...): kept verbatim so `get` mirrors the reference `thresholds.get(key, default)`.
    pub extra: Map<String, Value>,
}

impl Thresholds {
    /// Overlay a `thresholds` config map: declared fields take typed values,
    /// every other key lands in [`Thresholds::extra`].
    pub fn apply(&mut self, map: &Map<String, Value>) {
        for (key, value) in map {
            let Some(n) = value.as_f64() else { continue };
            match key.as_str() {
                "title_max_chars" => self.title_max_chars = n,
                "title_min_chars" => self.title_min_chars = n,
                "desc_max_chars" => self.desc_max_chars = n,
                "desc_min_chars" => self.desc_min_chars = n,
                "thin_content_words" => self.thin_content_words = n,
                "low_text_ratio_pct" => self.low_text_ratio_pct = n,
                "readability_flesch_min" => self.readability_flesch_min = n,
                "large_html_abs_kb" => self.large_html_abs_kb = n,
                "large_html_x_median" => self.large_html_x_median = n,
                "bytes_per_word_x_median" => self.bytes_per_word_x_median = n,
                "templated_title_share" => self.templated_title_share = n,
                "dom_depth_max" => self.dom_depth_max = n,
                "dom_nodes_max" => self.dom_nodes_max = n,
                "alt_max_chars" => self.alt_max_chars = n,
                "img_max_kb" => self.img_max_kb = n,
                "near_duplicate_similarity" => self.near_duplicate_similarity = n,
                "semantic_threshold" => self.semantic_threshold = n,
                "semantic_relevance_threshold" => self.semantic_relevance_threshold = n,
                "semantic_max_documents" => self.semantic_max_documents = n as i64,
                "semantic_max_candidate_comparisons" => {
                    self.semantic_max_candidate_comparisons = n as i64
                }
                "link_score_low_ratio" => self.link_score_low_ratio = n,
                "crawl_depth_max" => self.crawl_depth_max = n as i64,
                _ => {
                    self.extra.insert(key.clone(), value.clone());
                }
            }
        }
    }

    /// reference `thresholds.get(key)`: extra thresholds by name.
    pub fn get(&self, key: &str) -> Option<f64> {
        self.extra.get(key).and_then(Value::as_f64)
    }
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            title_max_chars: 60.0,
            title_min_chars: 30.0,
            desc_max_chars: 160.0,
            desc_min_chars: 70.0,
            thin_content_words: 200.0,
            low_text_ratio_pct: 10.0,
            readability_flesch_min: 30.0,
            large_html_abs_kb: 200.0,
            large_html_x_median: 3.0,
            bytes_per_word_x_median: 4.0,
            templated_title_share: 0.6,
            dom_depth_max: 32.0,
            dom_nodes_max: 1500.0,
            alt_max_chars: 100.0,
            img_max_kb: 150.0,
            near_duplicate_similarity: 0.92,
            semantic_threshold: 0.82,
            semantic_relevance_threshold: 0.35,
            semantic_max_documents: 1000,
            semantic_max_candidate_comparisons: 250_000,
            link_score_low_ratio: 0.25,
            crawl_depth_max: 4,
            extra: Map::from_iter([
                ("title_max_px".to_owned(), Value::from(561)),
                ("title_min_px".to_owned(), Value::from(200)),
                ("desc_max_px".to_owned(), Value::from(985)),
                ("desc_min_px".to_owned(), Value::from(400)),
                ("h1_max_chars".to_owned(), Value::from(70)),
                ("url_max_chars".to_owned(), Value::from(115)),
                ("click_depth_max".to_owned(), Value::from(10)),
                ("orphan_inlinks_min".to_owned(), Value::from(1)),
                ("response_time_max_s".to_owned(), Value::from(1.5)),
                (
                    "page_weight_max_bytes".to_owned(),
                    Value::from(2 * 1024 * 1024),
                ),
                (
                    "resource_max_bytes".to_owned(),
                    Value::from(2 * 1024 * 1024),
                ),
                (
                    "network_payload_max_bytes".to_owned(),
                    Value::from(4 * 1024 * 1024),
                ),
                ("sitemap_lastmod_stale_days".to_owned(), Value::from(365)),
                ("sitemap_desync_pct_warn".to_owned(), Value::from(20)),
                ("high_external_outlinks".to_owned(), Value::from(100)),
                ("high_outlinks".to_owned(), Value::from(300)),
                ("redirect_hop_cap".to_owned(), Value::from(20)),
                ("avg_words_per_sentence_max".to_owned(), Value::from(25)),
            ]),
        }
    }
}

/// Findings and skips of one audit run.
#[derive(Default)]
pub struct AuditContext {
    /// Read-only connection to the scan the pages came from, when there is one.
    pub scan: Option<rusqlite::Connection>,
    /// Saved corpus derivations (duplicates, structured data, internationalization) of the scan.
    pub saved_corpus: Option<Value>,
    /// Every crawled page, in scan order.
    pub pages: Vec<Page>,
    /// Thresholds in force.
    pub thresholds: Thresholds,
    /// Findings in the order rules added them.
    pub issues: Vec<Issue>,
    /// Groups in creation order.
    pub groups: Vec<Group>,
    /// Checks that could not be answered.
    pub skipped: Vec<SkippedCheck>,
    /// Measurement scopes `_measurement` records per check (`ctx.asset_measurements`);
    /// insertion-ordered — the reference dict keeps the checks' declaration order.
    pub asset_measurements: Map<String, Value>,
    /// The embedding-adapter semantic pass's evidence (`ctx.semantic_evidence`).
    pub semantic_evidence: Option<Value>,
    /// The offline lexical pass's evidence (`ctx.semantic_lexical_evidence`).
    pub semantic_lexical_evidence: Option<Value>,
    /// The link-graph shape block `check_internal_link_graph` writes (`ctx.internal_linking`).
    pub internal_linking: Map<String, Value>,
    /// The issue-#823 scope/trust-page/citation evidence block (`ctx.trust_evidence`).
    pub trust_evidence: Map<String, Value>,
    /// Analyzer config (`sf.config.load_config(None)` + crawl `analysis` overlays).
    pub config: Map<String, Value>,
    /// The crawl's start URL when one exists (`ctx.start_url`).
    pub start_url: Option<String>,
    /// `saved_corpus["internationalization"]` — retained hreflang declarations.
    pub native_hreflang: Option<Value>,
    /// `ctx._saved_corpus.owner.self_codes`: each page's self-declared hreflang
    /// codes keyed by normalized URL, filled only when the corpus derivation
    /// ran (the declared-rows index semantics, not the fallback fold).
    pub corpus_self_codes:
        Option<std::collections::BTreeMap<String, std::collections::BTreeSet<String>>>,
    /// The scan's `finish_reason` (`result.finish_reason` -> `run.crawl_finish_reason`).
    pub finish_reason: String,
    /// The scan's stopped reason (`result.stopped_reason` -> `run.crawl_stopped_reason`).
    pub stopped_reason: String,
    /// Whether the scan's crawl ended before the frontier was exhausted (`result.partial`).
    pub crawl_partial: bool,
    /// Whether the scan resumed an earlier crawl (`result.resumed`).
    pub crawl_resumed: bool,
    /// `result.cache_replay`.
    pub cache_replay: bool,
    /// `result.cache_stats`.
    pub cache_stats: Map<String, Value>,
    /// `summary.sitemap` block returned by `run_retained_sitemap`.
    pub sitemap_summary: Map<String, Value>,
    /// `summary.link_rel` block (`measured`/`reason`).
    pub link_rel: Map<String, Value>,
    /// The `link_position` response block `_audit_crawl_result` builds from the
    /// stored-graph composition (empty when `link_position.classify` is off).
    pub link_position: Map<String, Value>,
    /// `summary.fragment_links` evaluation block.
    pub fragment_links: Map<String, Value>,
    /// `config["requirements"]` — which optional checks the run requires.
    pub requirements: Map<String, Value>,
    /// `ctx.target_probes`: canonical-walk probe results keyed by normalized URL.
    pub target_probes: Map<String, Value>,
    /// Whether a native graph backend is available to the link checks
    /// (`ctx.graph_access` in reference — set only when `stored_graph_available`:
    /// a stored scan with a non-empty `links` table that is not a list scan).
    pub graph_access: bool,
    /// Project-bound provider reads resolved for the parity seam
    /// (`ctx.provider_fixture` — bindings, retained history, inspection rows).
    pub provider_fixture: Option<Value>,
    /// The `provider_findings.v1` evidence document the provider seam produced
    /// (`ctx.provider_evidence`, mirroring `summary["provider_findings"]`).
    pub provider_evidence: Option<Value>,
    /// Supplied Lighthouse input for the parity seam (`ctx.lighthouse_fixture`):
    /// `{"files": {name: content}, "reports": {url: name-or-raw}}`.
    pub lighthouse_fixture: Option<Value>,
    /// Analyzer field names whose source column the run's Internal:All frame
    /// carries (`find_column(ctx.internal_df, INTERNAL_FIELD_MAP[field])`).
    /// A native scan projects the full [`NATIVE_FIELDS`] contract; narrower
    /// sets model SF exports that lack the column.
    pub fields: std::collections::BTreeSet<String>,
    /// Typed export records `ctx.exports` carries (`export name ->` the
    /// `records_from_df` projection of that frame). `internal_all` itself is
    /// read through `pages`; this holds the bulk reports a run may add
    /// (`all_inlinks`, `security_mixed`, …). A present-but-empty list is a
    /// frame that exists yet has no rows.
    pub exports: std::collections::BTreeMap<String, Vec<Map<String, Value>>>,
    fired: BTreeSet<String>,
    skipped_ids: BTreeSet<String>,
    group_seq: usize,
}

impl AuditContext {
    /// A context over `pages` with default thresholds.
    pub fn new(pages: Vec<Page>) -> Self {
        Self {
            pages,
            ..Self::default()
        }
    }

    /// Fetched, 2xx HTML pages (`html_pages`).
    pub fn html_pages(&self) -> Vec<usize> {
        (0..self.pages.len())
            .filter(|&i| self.pages[i].is_html() && self.pages[i].is_2xx())
            .collect()
    }

    /// HTML pages that are indexable (`indexable_html_pages`).
    pub fn indexable_html_pages(&self) -> Vec<usize> {
        self.html_pages()
            .into_iter()
            .filter(|&i| self.pages[i].is_indexable())
            .collect()
    }

    /// Record a finding, filling message, source, severity and fix hint from the registry.
    pub fn add(&mut self, check: &str, target_url: Option<&str>, details: Map<String, Value>) {
        self.add_with(check, target_url, details, None, 1, None);
    }

    /// [`AuditContext::add`] with status code, occurrence count and group.
    /// Returns `false` when the check is disabled (`ctx.add` -> `None`).
    pub fn add_with(
        &mut self,
        check: &str,
        target_url: Option<&str>,
        details: Map<String, Value>,
        status_code: Option<i64>,
        occurrences_count: i64,
        group_id: Option<String>,
    ) -> bool {
        if !self.enabled(check) {
            return false;
        }
        let meta = check_registry::builtin()
            .ok()
            .map(|r| r.meta_or_fallback(check));
        let (severity, source, message, fix_hint) = match meta {
            Some(m) => (m.severity.as_str().to_owned(), m.source, m.message, m.fix),
            None => ("notice".to_owned(), String::new(), String::new(), None),
        };
        let severity = self.severity(check).unwrap_or(severity);
        self.issues.push(Issue {
            check: check.to_owned(),
            severity,
            source,
            message,
            fix_hint,
            target_url: target_url.map(str::to_owned),
            status_code,
            occurrences_count,
            details,
            locations: Vec::new(),
            evidence: Map::new(),
            group_id,
            id: None,
            fingerprint: None,
        });
        self.fired.insert(check.to_owned());
        if self.skipped_ids.remove(check) {
            self.skipped.retain(|s| s.id != check);
        }
        true
    }

    /// Record a finding that carries a location sample.
    pub fn add_located(
        &mut self,
        check: &str,
        target_url: &str,
        status_code: Option<i64>,
        occurrences: i64,
        locations: Vec<Value>,
        details: Map<String, Value>,
    ) {
        if self.add_with(
            check,
            Some(target_url),
            details,
            status_code,
            occurrences,
            None,
        ) {
            if let Some(issue) = self.issues.last_mut() {
                issue.locations = locations;
            }
        }
    }

    /// `ctx.add(check_id, source=..., locations=...)`: an explicit evidence source and location
    /// sample instead of the registry's defaults (retained native markup checks).
    pub fn add_source(
        &mut self,
        check: &str,
        target_url: Option<&str>,
        status_code: Option<i64>,
        occurrences_count: i64,
        evidence: (&str, Vec<Value>),
        details: Map<String, Value>,
    ) {
        if self.add_with(
            check,
            target_url,
            details,
            status_code,
            occurrences_count,
            None,
        ) {
            if let Some(issue) = self.issues.last_mut() {
                issue.source = evidence.0.to_owned();
                issue.locations = evidence.1;
            }
        }
    }

    /// `ctx.add(..., evidence={...})`: a finding carrying check-declared evidence hints.
    pub fn add_evidence(
        &mut self,
        check: &str,
        target_url: Option<&str>,
        status_code: Option<i64>,
        occurrences_count: i64,
        located_evidence: (Vec<Value>, Map<String, Value>),
        details: Map<String, Value>,
    ) {
        if self.add_with(
            check,
            target_url,
            details,
            status_code,
            occurrences_count,
            None,
        ) {
            if let Some(issue) = self.issues.last_mut() {
                issue.locations = located_evidence.0;
                issue.evidence = located_evidence.1;
            }
        }
    }

    /// `_measurement` (the reference suite): record a measurement scope and skip the check when
    /// the scope is not complete. `eligible` is `None` when the evidence frame is absent.
    pub fn measure(
        &mut self,
        code: &str,
        unit: &str,
        eligible: Option<i64>,
        measured: i64,
        partial: i64,
        reason: &str,
    ) {
        let unavailable = eligible.map(|e| e - measured);
        let state = if measured == 0 && eligible.is_none_or(|e| e > 0) {
            "unavailable"
        } else if partial > 0 || unavailable.is_none_or(|u| u > 0) {
            "partial"
        } else {
            "complete"
        };
        let mut record = Map::new();
        record.insert("unit".into(), Value::from(unit));
        record.insert("eligible".into(), eligible.map_or(Value::Null, Value::from));
        record.insert("measured".into(), Value::from(measured));
        record.insert("partial".into(), Value::from(partial));
        record.insert(
            "unavailable".into(),
            unavailable.map_or(Value::Null, Value::from),
        );
        record.insert("state".into(), Value::from(state));
        record.insert(
            "reason".into(),
            Value::from(if state == "complete" { "" } else { reason }),
        );
        self.asset_measurements
            .insert(code.to_owned(), Value::Object(record));
        if state != "complete" {
            self.skip(code, reason);
        }
    }

    /// `ctx.enabled(check_id)`: `config.checks.<id>.enabled` may turn a check off.
    pub fn enabled(&self, check: &str) -> bool {
        self.config
            .get("checks")
            .and_then(Value::as_object)
            .and_then(|checks| checks.get(check))
            .and_then(Value::as_object)
            .and_then(|cfg| cfg.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// `ctx.severity(check_id)`: `severity_overrides` then per-check `severity`.
    pub fn severity(&self, check: &str) -> Option<String> {
        if let Some(s) = self
            .config
            .get("severity_overrides")
            .and_then(Value::as_object)
            .and_then(|m| m.get(check))
            .and_then(Value::as_str)
        {
            return Some(s.to_owned());
        }
        self.config
            .get("checks")
            .and_then(Value::as_object)
            .and_then(|checks| checks.get(check))
            .and_then(Value::as_object)
            .and_then(|cfg| cfg.get("severity"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// `_has_column(ctx, field)`: the run measured `field` at all — an absent
    /// column and a column of blanks are different facts (issue #205).
    pub fn has_column(&self, field: &str) -> bool {
        self.fields.contains(field)
    }

    /// `ctx.exports.has(name)`: the frame exists at all — a present-but-empty
    /// report is still present.
    pub fn has_export(&self, name: &str) -> bool {
        self.exports.contains_key(name)
    }

    /// `ctx.exports.get(name)` -> typed records, or `None` when the frame was
    /// never exported; an empty frame is `Some(&[])` (`has` true, `get` empty).
    pub fn export_records(&self, name: &str) -> Option<&Vec<Map<String, Value>>> {
        self.exports.get(name)
    }

    /// The index of `url` in `pages` (`ctx.page_by_url[url]`), when the page exists.
    pub fn page_index(&self, url: &str) -> Option<usize> {
        self.pages.iter().position(|p| p.url == url)
    }

    /// Whether the check already produced findings (`check_id in _fired_ids`).
    pub fn fired(&self, check: &str) -> bool {
        self.fired.contains(check)
    }

    /// `ctx.settled`: the check either fired or was already skipped —
    /// the provider-emission guard against double-settling.
    pub fn settled(&self, check: &str) -> bool {
        self.fired.contains(check) || self.skipped_ids.contains(check)
    }

    /// One group of pages sharing a defect value. An empty id means the check
    /// is disabled (reference `add_group` -> `None`).
    pub fn add_group(&mut self, check: &str, value: Option<&str>, urls: Vec<String>) -> String {
        if !self.enabled(check) {
            return String::new();
        }
        let prefix: String = check
            .split('_')
            .next()
            .unwrap_or("")
            .chars()
            .take(6)
            .collect::<String>()
            .to_uppercase();
        self.group_seq += 1;
        let group_id = format!("GRP-{prefix}-{:04}", self.group_seq);
        self.groups.push(Group {
            group_id: group_id.clone(),
            check: check.to_owned(),
            value: value.map(str::to_owned),
            urls,
        });
        group_id
    }

    /// One finding for a defect shared by `urls` (`add_site_wide`).
    pub fn add_site_wide(&mut self, check: &str, urls: &[String], details: Map<String, Value>) {
        self.add_site_wide_occurrences(check, urls, details, None);
    }

    /// `ctx.add_site_wide(..., occurrences=n)`: `occurrences` is per page; the group
    /// finding keeps `occurrences * page_count` (or the page count when absent).
    pub fn add_site_wide_occurrences(
        &mut self,
        check: &str,
        urls: &[String],
        details: Map<String, Value>,
        occurrences: Option<i64>,
    ) {
        if let [only] = urls {
            let occurrences = occurrences.unwrap_or(1);
            self.add_with(check, Some(only), details, None, occurrences, None);
            return;
        }
        let mut ordered = urls.to_vec();
        ordered.sort();
        let group = self.add_group(check, None, ordered.clone());
        let group_id = (!group.is_empty()).then_some(group);
        let total = occurrences.map_or(ordered.len() as i64, |n| n * ordered.len() as i64);
        let mut details = details;
        details.insert("page_count".into(), Value::from(ordered.len()));
        details.insert(
            "sample_urls".into(),
            Value::from(ordered.iter().take(5).cloned().collect::<Vec<_>>()),
        );
        let first = ordered[0].clone();
        self.add_with(check, Some(&first), details, None, total, group_id);
    }

    /// Declare a check unanswerable, unless it already fired or was skipped.
    pub fn skip(&mut self, check: &str, reason: &str) {
        if self.fired.contains(check) || self.skipped_ids.contains(check) {
            return;
        }
        self.skipped_ids.insert(check.to_owned());
        self.skipped.push(SkippedCheck {
            id: check.to_owned(),
            reason: reason.to_owned(),
        });
    }

    /// `ctx.retract`: withdraw a check's findings and record why it cannot be
    /// answered (the partial-crawl withholding path in `aggregate`).
    pub fn retract(&mut self, check: &str, reason: &str) {
        self.issues.retain(|i| i.check != check);
        self.fired.remove(check);
        self.skip(check, reason);
    }
}
