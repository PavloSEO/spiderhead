//! Self-consistency scan of one finished crawl's own artifacts.
//!
//! Every defect found on live sites was an impossible number in a report: a size that
//! disagrees with the file on disk, more findings than pages, a canonical reported as a
//! redirect while it answers 200. Each is arithmetic over facts the run already wrote
//! down. This module reports only pairs of facts that cannot both be true, each naming
//! both values and where each came from. It is pure and never touches the network.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde_json::{json, Map, Value};
use spiderhead_core::finding_policy::IMPLAUSIBLE_SHARE;

use crate::error::{LogsError, Result};
use crate::urlsplit;

/// One pair of facts that cannot both be true. There is no severity: an anomaly is
/// proven by the artifacts or it is not reported.
#[derive(Debug, Clone, PartialEq)]
pub struct Anomaly {
    pub rule: &'static str,
    pub message: String,
    pub observed: Value,
    pub expected: Value,
    pub target: String,
    /// Where `observed` and `expected` were read from.
    pub sources: Vec<(&'static str, String)>,
}

impl Anomaly {
    fn as_json(&self, rename: &dyn Fn(&str) -> String) -> Value {
        let sources: Map<String, Value> = self
            .sources
            .iter()
            .map(|(k, v)| ((*k).to_string(), json!(rename(v))))
            .collect();
        json!({
            "rule": self.rule, "message": self.message, "observed": self.observed,
            "expected": self.expected, "target": self.target, "sources": sources,
        })
    }
}

/// Everything a scan reads. Any of it may be absent; a rule needing a missing artifact
/// does not run rather than guessing.
#[derive(Debug, Clone, Default)]
pub struct RunArtifacts {
    /// The audit document (`summary`, `issues`, `run`).
    pub audit: Option<Value>,
    /// One record per crawled page.
    pub pages: Vec<Value>,
    /// URL to bytes on disk of downloaded files.
    pub files: BTreeMap<String, i64>,
    /// Crawl decision trace, when one was written.
    pub decisions: Vec<Value>,
    pub audit_path: String,
    pub pages_path: String,
    pub decisions_path: String,
    /// True when the artifacts came from a stored scan rather than loose files.
    pub native: bool,
}

fn read_jsonl(path: &Path) -> Result<Vec<Value>> {
    let text = std::fs::read(path)?;
    let text = String::from_utf8(text)
        .map_err(|e| LogsError::value(format!("{}: {e}", path.display())))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        // A truncated final line from an interrupted run is not a finding.
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(Value::is_object)
        .collect())
}

/// Sizes of downloaded files keyed by the URL their manifest names; no manifest, no claim.
fn downloaded_sizes(dir: &Path) -> BTreeMap<String, i64> {
    let mut sizes = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(dir.join("manifest.json")) else {
        return sizes;
    };
    let Ok(raw) = serde_json::from_str::<Value>(&text) else {
        return sizes;
    };
    let entries = match &raw {
        Value::Object(o) => o.get("images").cloned().unwrap_or(Value::Null),
        other => other.clone(),
    };
    for entry in entries.as_array().into_iter().flatten() {
        let (Some(url), Some(path)) = (
            entry
                .get("url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
            entry
                .get("path")
                .or_else(|| entry.get("file"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
        ) else {
            continue;
        };
        let candidate = if Path::new(path).is_absolute() {
            Path::new(path).to_path_buf()
        } else {
            dir.join(path)
        };
        if let Ok(meta) = std::fs::metadata(&candidate) {
            if meta.is_file() {
                sizes.insert(url.to_string(), meta.len() as i64);
            }
        }
    }
    sizes
}

fn expand_home(path: &str) -> std::path::PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => Path::new(path).to_path_buf(),
    }
}

/// Read the artifacts of one finished run from a directory of loose files.
pub fn load_run(run_dir: &str, images_dir: Option<&str>) -> Result<RunArtifacts> {
    let base = expand_home(run_dir);
    let mut run = RunArtifacts::default();
    let audit = base.join("audit.json");
    if audit.is_file() {
        run.audit = serde_json::from_str(&std::fs::read_to_string(&audit)?).ok();
        if run.audit.is_some() {
            run.audit_path = audit.to_string_lossy().into_owned();
        }
    }
    let pages = base.join("pages.jsonl");
    if pages.is_file() {
        run.pages = read_jsonl(&pages)?;
        run.pages_path = pages.to_string_lossy().into_owned();
    }
    let decisions = base.join("decisions.jsonl");
    if decisions.is_file() {
        run.decisions = read_jsonl(&decisions)?;
        run.decisions_path = decisions.to_string_lossy().into_owned();
    }
    if let Some(images) = images_dir.filter(|s| !s.is_empty()) {
        run.files = downloaded_sizes(&expand_home(images));
    }
    Ok(run)
}

fn is_int(v: Option<&Value>) -> bool {
    v.is_some_and(|v| v.is_i64() || v.is_u64())
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        None | Some(Value::Null) => "None".into(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => other.to_string(),
    }
}

fn issues(run: &RunArtifacts) -> &[Value] {
    run.audit
        .as_ref()
        .and_then(|a| a.get("issues"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn by_check(run: &RunArtifacts) -> Option<&Map<String, Value>> {
    run.audit
        .as_ref()?
        .pointer("/summary/by_check")
        .and_then(Value::as_object)
}

fn rule_recorded_size(run: &RunArtifacts) -> Vec<Anomaly> {
    let mut out = Vec::new();
    for page in &run.pages {
        let url = text(page.get("url"));
        let recorded = page.get("size_bytes");
        let Some(actual) = run.files.get(&url) else {
            continue;
        };
        let Some(recorded_int) = recorded
            .filter(|_| is_int(recorded))
            .and_then(Value::as_i64)
        else {
            continue;
        };
        if recorded_int == *actual {
            continue;
        }
        out.push(Anomaly {
            rule: "size_matches_file",
            message: "recorded size differs from the downloaded file".into(),
            observed: json!(recorded_int),
            expected: json!(actual),
            target: url,
            sources: vec![
                ("observed", "pages.jsonl:size_bytes".into()),
                ("expected", "downloaded file".into()),
            ],
        });
    }
    out
}

fn rule_text_ratio(run: &RunArtifacts) -> Vec<Anomaly> {
    run.pages
        .iter()
        .filter_map(|page| {
            let ratio = page.get("text_ratio").filter(|v| v.is_number())?;
            (ratio.as_f64()? > 100.0).then(|| Anomaly {
                rule: "text_ratio_is_a_percentage",
                message: "text ratio exceeds 100%: the text and the size were not measured alike"
                    .into(),
                observed: ratio.clone(),
                expected: json!("<= 100"),
                target: text(page.get("url")),
                sources: vec![("observed", "pages.jsonl:text_ratio".into())],
            })
        })
        .collect()
}

fn rule_a_200_has_bytes(run: &RunArtifacts) -> Vec<Anomaly> {
    run.pages
        .iter()
        .filter_map(|page| {
            let status = page
                .get("status_code")
                .filter(|_| is_int(page.get("status_code")))?
                .as_i64()?;
            let size_zero = page.get("size_bytes").and_then(Value::as_f64) == Some(0.0);
            let words = page.get("word_count");
            ((200..300).contains(&status) && size_zero && truthy(words)).then(|| Anomaly {
                rule: "words_without_bytes",
                message: "words were counted on a page recorded as zero bytes".into(),
                observed: words.cloned().unwrap_or(Value::Null),
                expected: json!(0),
                target: text(page.get("url")),
                sources: vec![
                    ("observed", "pages.jsonl:word_count".into()),
                    ("expected", "pages.jsonl:size_bytes".into()),
                ],
            })
        })
        .collect()
}

/// The producer's site-wide lastmod finding may name an origin the crawl never fetched.
fn site_lastmod_target(run: &RunArtifacts, issue: &Value, target: &str) -> bool {
    if issue.get("check").and_then(Value::as_str) != Some("SITEMAP_STALE_LASTMOD")
        || issue.get("source").and_then(Value::as_str) != Some("sitemap")
    {
        return false;
    }
    let audit = run.audit.as_ref();
    let measured = audit.and_then(|a| a.pointer("/summary/sitemap/lastmod"));
    let Some(Value::Object(measured_map)) = measured else {
        return false;
    };
    if measured_map.is_empty() || issue.get("details") != measured {
        return false;
    }
    let source = audit
        .and_then(|a| a.pointer("/run/source"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let (Ok(parts), Ok(source)) = (urlsplit::split(target), urlsplit::split(source)) else {
        return false;
    };
    let web = |s: &str| s == "http" || s == "https";
    if !web(&parts.scheme)
        || parts.netloc.is_empty()
        || !(parts.path.is_empty() || parts.path == "/")
        || !parts.query.is_empty()
        || !parts.fragment.is_empty()
    {
        return false;
    }
    let origin = (parts.scheme.to_lowercase(), parts.netloc.to_lowercase());
    if web(&source.scheme) && !source.netloc.is_empty() {
        return (source.scheme.to_lowercase(), source.netloc.to_lowercase()) == origin;
    }
    // An export audit may name a directory as its source; a retained page must then
    // establish that this origin belongs to the audited site.
    run.pages.iter().any(|page| {
        urlsplit::split(&text(page.get("url")))
            .is_ok_and(|v| (v.scheme.to_lowercase(), v.netloc.to_lowercase()) == origin)
    })
}

/// Checks whose target is deliberately a URL the crawl did not fetch.
const EXEMPT_FROM_CRAWLED: [&str; 5] = [
    "SITEMAP_ORPHAN",
    "BROKEN_LINK",
    "HREFLANG_BROKEN_TARGET",
    "UNLINKED_CANONICAL",
    // Names an in-host edge destination, not a fetched page.
    "FOLLOW_AND_NOFOLLOW_INLINKS",
];

fn rule_findings_about_crawled(run: &RunArtifacts) -> Vec<Anomaly> {
    if run.pages.is_empty() || run.audit.is_none() {
        return Vec::new();
    }
    let crawled: HashSet<String> = run
        .pages
        .iter()
        .filter(|p| truthy(p.get("url")))
        .map(|p| text(p.get("url")))
        .collect();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut out = Vec::new();
    for issue in issues(run) {
        let check = issue
            .get("check")
            .filter(|v| truthy(Some(v)))
            .map_or(String::new(), |v| text(Some(v)));
        let target = issue
            .get("target_url")
            .filter(|v| truthy(Some(v)))
            .map_or(String::new(), |v| text(Some(v)));
        if target.is_empty()
            || EXEMPT_FROM_CRAWLED.contains(&check.as_str())
            || crawled.contains(&target)
            || site_lastmod_target(run, issue, &target)
        {
            continue;
        }
        if !seen.insert((check.clone(), target.clone())) {
            continue;
        }
        out.push(Anomaly {
            rule: "findings_are_about_crawled_urls",
            message: format!("{check} reports a URL absent from this run's own page list"),
            observed: json!(target),
            expected: json!("a URL in pages.jsonl"),
            target,
            sources: vec![
                ("observed", "audit.json:issues".into()),
                ("expected", "pages.jsonl:url".into()),
            ],
        });
    }
    out
}

fn rule_check_within_population(run: &RunArtifacts) -> Vec<Anomaly> {
    if run.pages.is_empty() || run.audit.is_none() {
        return Vec::new();
    }
    let html = run
        .pages
        .iter()
        .filter(|p| text_lower(p.get("content_type")).contains("html"))
        .count() as i64;
    if html == 0 {
        return Vec::new();
    }
    by_check(run)
        .into_iter()
        .flatten()
        .filter(|(_, count)| is_int(Some(count)) && count.as_i64().is_some_and(|c| c > html))
        .map(|(check, count)| Anomaly {
            rule: "check_within_its_population",
            message: format!("{check} fired more often than there are HTML pages to fire on"),
            observed: count.clone(),
            expected: json!(html),
            target: check.clone(),
            sources: vec![
                ("observed", "audit.json:summary.by_check".into()),
                (
                    "expected",
                    "pages.jsonl: pages with an HTML content type".into(),
                ),
            ],
        })
        .collect()
}

fn text_lower(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.to_lowercase(),
        other => text(other).to_lowercase(),
    }
}

fn rule_summary_matches_rows(run: &RunArtifacts) -> Vec<Anomaly> {
    let Some(claims) = by_check(run).filter(|m| !m.is_empty()) else {
        return Vec::new();
    };
    let mut counted: BTreeMap<String, i64> = BTreeMap::new();
    for issue in issues(run) {
        let check = issue
            .get("check")
            .filter(|v| truthy(Some(v)))
            .map_or(String::new(), |v| text(Some(v)));
        *counted.entry(check).or_insert(0) += 1;
    }
    let mut names: Vec<&String> = claims.keys().collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|check| {
            let claimed = &claims[check];
            let actual = counted.get(check).copied().unwrap_or(0);
            if !is_int(Some(claimed)) || claimed.as_i64() == Some(actual) {
                return None;
            }
            Some(Anomaly {
                rule: "summary_matches_detail",
                message: format!(
                    "summary claims {} {check} findings, the issue list holds {actual}",
                    text(Some(claimed))
                ),
                observed: claimed.clone(),
                expected: json!(actual),
                target: check.clone(),
                sources: vec![
                    ("observed", "audit.json:summary.by_check".into()),
                    ("expected", "audit.json:issues".into()),
                ],
            })
        })
        .collect()
}

fn rule_canonical_twin(run: &RunArtifacts) -> Vec<Anomaly> {
    if run.pages.is_empty() || run.audit.is_none() {
        return Vec::new();
    }
    let answering: HashSet<String> = run
        .pages
        .iter()
        .filter(|p| {
            is_int(p.get("status_code"))
                && p.get("status_code")
                    .and_then(Value::as_i64)
                    .is_some_and(|s| (200..300).contains(&s))
        })
        .map(|p| text(p.get("url")).trim_end_matches('/').to_string())
        .collect();
    issues(run)
        .iter()
        .filter(|i| text(i.get("check")) == "CANONICAL_TO_REDIRECT")
        .filter_map(|issue| {
            let canonical = issue
                .get("details")
                .and_then(|d| d.get("canonical"))
                .filter(|v| truthy(Some(v)))
                .map_or(String::new(), |v| text(Some(v)));
            (!canonical.is_empty() && answering.contains(canonical.trim_end_matches('/'))).then(
                || Anomaly {
                    rule: "canonical_to_redirect_has_no_answering_twin",
                    message:
                        "canonical reported as a redirect, but that URL answered 2xx in this run"
                            .into(),
                    observed: json!("3xx"),
                    expected: json!("2xx"),
                    target: canonical,
                    sources: vec![
                        (
                            "observed",
                            "audit.json:issues[CANONICAL_TO_REDIRECT]".into(),
                        ),
                        ("expected", "pages.jsonl:status_code".into()),
                    ],
                },
            )
        })
        .collect()
}

fn rule_representation(run: &RunArtifacts) -> Vec<Anomaly> {
    let labelled = run
        .pages
        .iter()
        .filter(|p| truthy(p.get("representation")))
        .count();
    if run.pages.is_empty() || labelled == 0 || labelled == run.pages.len() {
        return Vec::new();
    }
    vec![Anomaly {
        rule: "representation_is_recorded",
        message: "some pages record how they were measured and some do not".into(),
        observed: json!(run.pages.len() - labelled),
        expected: json!(0),
        target: String::new(),
        sources: vec![("observed", "pages.jsonl:representation".into())],
    }]
}

/// A URL rejected as off-host whose hostname is the crawl's own host: a scope bug, never
/// a judgement call. Visible only in the decision trace.
fn rule_outside_host(run: &RunArtifacts) -> Vec<Anomaly> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for entry in &run.decisions {
        if entry.get("type").and_then(Value::as_str) != Some("exclude") {
            continue;
        }
        let reason = entry.get("reason").and_then(Value::as_str).unwrap_or("");
        if reason != "outside_host" && reason != "redirect_off_host" {
            continue;
        }
        let url = entry
            .get("url")
            .filter(|v| truthy(Some(v)))
            .map_or(String::new(), |v| text(Some(v)));
        let crawl_host = entry
            .get("host")
            .filter(|v| truthy(Some(v)))
            .map_or(String::new(), |v| text(Some(v)))
            .to_lowercase();
        if url.is_empty() || crawl_host.is_empty() || seen.contains(&url) {
            continue;
        }
        let host = hostname(&url);
        if host.as_deref() != Some(crawl_host.as_str()) {
            continue;
        }
        seen.insert(url.clone());
        out.push(Anomaly {
            rule: "outside_host_exclusion_matches_its_own_host",
            message: format!("excluded as {reason}, but its host is the crawl's own host"),
            observed: json!(host),
            expected: json!(format!("a host other than '{crawl_host}'")),
            target: url,
            sources: vec![
                ("observed", "decisions.jsonl:url".into()),
                ("expected", "decisions.jsonl:host".into()),
            ],
        });
    }
    out
}

/// Lowercased hostname of a URL: userinfo and port removed, brackets stripped.
fn hostname(url: &str) -> Option<String> {
    let netloc = urlsplit::split(url).ok()?.netloc;
    let host_port = netloc
        .rsplit_once('@')
        .map_or(netloc.as_str(), |(_, tail)| tail);
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// A check covering most of the crawl, named for a reviewer. Read from the audit's own
/// `implausible_checks` so the scan never disagrees with the report it reads.
fn review_describes_most_of_site(run: &RunArtifacts) -> Vec<Anomaly> {
    let Some(audit) = &run.audit else {
        return Vec::new();
    };
    audit
        .pointer("/summary/implausible_checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|row| row.is_object())
        .map(|row| {
            let share = row.get("share").filter(|v| truthy(Some(v))).and_then(Value::as_f64).unwrap_or(0.0);
            let check = text(row.get("check"));
            Anomaly {
                rule: "check_describes_most_of_the_site",
                message: format!(
                    "{check} describes {:.0}% of the crawled pages -- true of some sites, and what a broken check looks like on the rest",
                    share * 100.0
                ),
                observed: row.get("pages").cloned().unwrap_or(Value::Null),
                expected: json!(format!(
                    "under {:.0}% of pages, for a check about the unusual",
                    IMPLAUSIBLE_SHARE * 100.0
                )),
                target: check,
                sources: vec![("observed", "audit.json:summary.implausible_checks".into())],
            }
        })
        .collect()
}

type Rule = fn(&RunArtifacts) -> Vec<Anomaly>;

/// Contradiction rules, in report order, with the names `rules_run` lists.
const RULES: [(&str, Rule); 9] = [
    ("recorded_size_matches_the_file", rule_recorded_size),
    ("text_ratio_is_a_percentage", rule_text_ratio),
    ("a_200_has_bytes", rule_a_200_has_bytes),
    (
        "findings_are_about_crawled_urls",
        rule_findings_about_crawled,
    ),
    (
        "a_check_cannot_exceed_its_population",
        rule_check_within_population,
    ),
    ("summary_matches_the_issue_rows", rule_summary_matches_rows),
    (
        "canonical_to_redirect_has_no_answering_twin",
        rule_canonical_twin,
    ),
    (
        "outside_host_exclusion_matches_its_own_host",
        rule_outside_host,
    ),
    ("representation_is_recorded", rule_representation),
];

/// Apply every rule. `max_per_rule` caps listed examples of one contradiction while the
/// counts stay complete.
pub fn scan(run: &RunArtifacts, max_per_rule: usize) -> Value {
    let rename = |source: &str| -> String {
        if !run.native {
            return source.to_string();
        }
        source
            .replace("audit.json", &run.audit_path)
            .replace("pages.jsonl", &run.pages_path)
    };
    let mut anomalies: Vec<Value> = Vec::new();
    let mut per_rule: Vec<(&'static str, i64)> = Vec::new();
    for (_, rule) in RULES {
        for (index, found) in rule(run).iter().enumerate() {
            match per_rule.iter_mut().find(|(name, _)| *name == found.rule) {
                Some((_, n)) => *n += 1,
                None => per_rule.push((found.rule, 1)),
            }
            if index < max_per_rule {
                anomalies.push(found.as_json(&rename));
            }
        }
    }
    let review: Vec<Value> = review_describes_most_of_site(run)
        .iter()
        .take(max_per_rule)
        .map(|a| a.as_json(&rename))
        .collect();
    let by_rule: Map<String, Value> = per_rule
        .iter()
        .map(|(k, v)| ((*k).to_string(), json!(v)))
        .collect();
    let limitations: Vec<&str> = if run.native {
        vec!["Native scan has no decisions.jsonl exclusion trace; the outside-host decision rule was not measured."]
    } else {
        Vec::new()
    };
    json!({
        "ok": true,
        "anomalies": anomalies,
        "anomaly_count": per_rule.iter().map(|(_, n)| n).sum::<i64>(),
        "review": review,
        "by_rule": by_rule,
        "read": {
            "audit": run.audit.as_ref().is_some_and(|a| truthy(Some(a))),
            "pages": run.pages.len(),
            "downloaded_files": run.files.len(),
            "decisions": run.decisions.len(),
        },
        // A clean result must not be mistakable for a complete one: a rule that had no
        // artifact to read is silent for the same reason a rule that found nothing is.
        "rules_run": RULES.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        "limitations": limitations,
    })
}
