//! Canonical and directive checks (reference the reference suite).

use std::collections::HashMap;

use regex::Regex;
use serde_json::{json, Map, Value};
use spiderhead_parse::parser::meta::robots_directives;

use super::content_meta::skip_for_body_unavailable;
use super::details;
use crate::audit::{norm_url, AuditContext};

/// `check_response_codes`: 4xx and 5xx answers (a status of 0 means no response).
pub fn check_response_codes(ctx: &mut AuditContext) {
    for page in ctx.pages.clone() {
        let Some(code) = page.status_code else {
            continue;
        };
        match code {
            0 => {
                ctx.add_with(
                    "NO_RESPONSE",
                    Some(&page.url),
                    details([("status", page.opt("status"))]),
                    Some(0),
                    1,
                    None,
                );
            }
            400..=499 => {
                ctx.add_with(
                    "BROKEN_PAGE_4XX",
                    Some(&page.url),
                    details([
                        ("status", page.opt("status")),
                        ("inlinks", page.opt("inlinks")),
                    ]),
                    Some(code),
                    1,
                    None,
                );
            }
            500..=599 => {
                ctx.add_with(
                    "SERVER_ERROR_5XX",
                    Some(&page.url),
                    details([("status", page.opt("status"))]),
                    Some(code),
                    1,
                    None,
                );
            }
            _ => {}
        }
    }
}

/// `check_indexability`: robots-blocked URLs and non-indexable HTML that is
/// still linked.
pub fn check_indexability(ctx: &mut AuditContext) {
    for page in ctx.pages.clone() {
        if page
            .indexability_status
            .to_lowercase()
            .contains("blocked by robots")
        {
            ctx.add(
                "BLOCKED_BY_ROBOTS",
                Some(&page.url),
                details([
                    ("indexability_status", json!(page.indexability_status)),
                    ("inlinks", page.opt("inlinks")),
                ]),
            );
            // blocked AND internally linked => robots blocks crawl of a live,
            // linked page
            if page.num("inlinks").is_some_and(|n| n > 0.0) {
                ctx.add(
                    "IMPORTANT_URL_BLOCKED_BY_ROBOTS",
                    Some(&page.url),
                    details([
                        ("inlinks", page.opt("inlinks")),
                        ("indexability_status", json!(page.indexability_status)),
                    ]),
                );
            }
        }
        if page.is_html() && !page.is_indexable() {
            let inlinks = page.num("inlinks");
            if inlinks.is_some_and(|n| n > 0.0) && page.status_code == Some(200) {
                ctx.add(
                    "NON_INDEXABLE_LINKED",
                    Some(&page.url),
                    details([
                        ("indexability_status", json!(page.indexability_status)),
                        ("inlinks", page.opt("inlinks")),
                    ]),
                );
            }
        }
    }
}

/// `check_canonical_directives`: missing, cross-URL and non-indexable canonicals, plus page directives.
pub fn check_canonical_directives(ctx: &mut AuditContext) {
    let require_canonical = ctx
        .requirements
        .get("require_canonical")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let has_meta_keywords = ctx.has_column("meta_keywords");
    if require_canonical {
        let indexable: Vec<usize> = ctx
            .html_pages()
            .into_iter()
            .filter(|&i| ctx.pages[i].is_indexable())
            .collect();
        skip_for_body_unavailable(ctx, "CANONICAL_MISSING", &indexable);
    } else {
        // The run that turns the requirement off must say the check was not
        // evaluated rather than read as a site with canonicals everywhere.
        ctx.skip(
            "CANONICAL_MISSING",
            "requirements.require_canonical is false; the check was not evaluated",
        );
    }
    let mut by_norm: std::collections::HashMap<String, Vec<usize>> = Default::default();
    for (i, p) in ctx.pages.iter().enumerate() {
        by_norm.entry(norm_url(&p.url)).or_default().push(i);
    }
    let mut unavailable_targets = 0;
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        let canonical = page.text("canonical").to_owned();
        if page.is_indexable()
            && require_canonical
            && canonical.is_empty()
            && !page.body_unavailable()
        {
            ctx.add("CANONICAL_MISSING", Some(&page.url), Default::default());
        }
        if !canonical.is_empty() && norm_url(&canonical) != norm_url(&page.url) {
            ctx.add(
                "CANONICALISED",
                Some(&page.url),
                details([("canonical", json!(canonical))]),
            );
            let targets: Vec<usize> = by_norm
                .get(&norm_url(&canonical))
                .cloned()
                .unwrap_or_default();
            if targets.is_empty() {
                unavailable_targets += 1;
            }
            let codes: Vec<Option<i64>> =
                targets.iter().map(|&t| ctx.pages[t].status_code).collect();
            let is_redirect = codes
                .iter()
                .any(|c| c.is_some_and(|c| (300..=399).contains(&c)));
            let is_http_error = !codes.is_empty()
                && codes
                    .iter()
                    .all(|c| c.is_some_and(|c| (400..=599).contains(&c)));
            if !targets.is_empty()
                && !targets.iter().any(|&t| ctx.pages[t].is_indexable())
                && (codes
                    .iter()
                    .any(|c| c.is_some_and(|c| (200..=299).contains(&c)))
                    || (!is_redirect && !is_http_error))
            {
                let evidence: Vec<Value> = targets
                    .iter()
                    .take(20)
                    .map(|&t| {
                        let p = &ctx.pages[t];
                        json!({"url": p.url, "status_code": p.status_code, "indexability": p.indexability, "indexability_status": p.indexability_status})
                    })
                    .collect();
                let omitted = targets.len().saturating_sub(20);
                ctx.add(
                    "CANONICAL_NON_INDEXABLE",
                    Some(&page.url),
                    details([
                        ("canonical", json!(canonical)),
                        ("canonical_target_evidence", Value::Array(evidence)),
                        ("canonical_target_evidence_omitted", json!(omitted)),
                    ]),
                );
            }
        }
        let robots = robots_directives(&[
            page.rec.get("meta_robots").and_then(Value::as_str),
            page.rec.get("x_robots").and_then(Value::as_str),
        ]);
        if robots.iter().any(|d| d == "noindex") {
            ctx.add(
                "NOINDEX",
                Some(&page.url),
                details([("meta_robots", page.opt("meta_robots"))]),
            );
        } else if robots.iter().any(|d| d == "nofollow") && page.is_indexable() {
            ctx.add(
                "NOFOLLOW_PAGE",
                Some(&page.url),
                details([("meta_robots", page.opt("meta_robots"))]),
            );
        }
        if has_meta_keywords && !page.text("meta_keywords").is_empty() {
            ctx.add(
                "META_KEYWORDS_PRESENT",
                Some(&page.url),
                details([("value", page.opt("meta_keywords"))]),
            );
        }
    }
    if unavailable_targets > 0 {
        ctx.skip(
            "CANONICAL_NON_INDEXABLE",
            &format!("{unavailable_targets} canonical target(s) have unmeasured indexability; native probes may lack parsed HTML directives"),
        );
    }
    if !has_meta_keywords {
        ctx.skip(
            "META_KEYWORDS_PRESENT",
            "no Meta Keywords 1 column in Internal:All",
        );
    }
}

/// `check_redirect_type`: temporary (302/303/307) redirects.
pub fn check_redirect_type(ctx: &mut AuditContext) {
    for page in ctx.pages.clone() {
        let Some(code) = page.status_code.filter(|c| matches!(c, 302 | 303 | 307)) else {
            continue;
        };
        ctx.add_with(
            "BAD_REDIRECT_TYPE",
            Some(&page.url),
            details([
                ("redirect_type", Value::Null),
                ("redirect_url", page.opt("redirect_url")),
            ]),
            Some(code),
            1,
            None,
        );
    }
}

/// `_has_refresh_target`: a refresh redirects only when it names a non-empty `url=` target.
fn has_refresh_target(refresh: &str) -> bool {
    let lower = refresh.to_lowercase();
    let Some(at) = lower.find("url") else {
        return false;
    };
    let rest = lower[at + 3..].trim_start();
    let Some(rest) = rest.strip_prefix('=') else {
        return refresh_after(&lower[at + 3..]);
    };
    let rest = rest.trim_start();
    let target = match rest.chars().next() {
        Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or("").trim().to_owned(),
        _ => rest
            .split(|c: char| c == ';' || c.is_whitespace())
            .next()
            .unwrap_or("")
            .to_owned(),
    };
    !target.trim().is_empty()
}

fn refresh_after(rest: &str) -> bool {
    rest.to_lowercase().find("url").is_some_and(|_| {
        has_refresh_target(&format!(
            "url{}",
            rest.split_once("url").map(|x| x.1).unwrap_or("")
        ))
    })
}

/// `check_directives_extra`: the legacy robots directives and the refresh rows.
pub fn check_directives_extra(ctx: &mut AuditContext) {
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        let robots = robots_directives(&[
            page.rec.get("meta_robots").and_then(Value::as_str),
            page.rec.get("x_robots").and_then(Value::as_str),
        ]);
        for (token, check) in [
            ("noarchive", "NOARCHIVE"),
            ("nosnippet", "NOSNIPPET"),
            ("noimageindex", "NOIMAGEINDEX"),
            ("notranslate", "NOTRANSLATE"),
            ("noodp", "NOODP"),
            ("noydir", "NOYDIR"),
        ] {
            if robots.iter().any(|r| r == token) {
                ctx.add(check, Some(&page.url), Default::default());
            }
        }
        if let Some(unavailable) = robots.iter().find(|t| t.starts_with("unavailable_after")) {
            ctx.add(
                "UNAVAILABLE_AFTER",
                Some(&page.url),
                details([("directive", json!(unavailable))]),
            );
        }
        let refresh = page.text("meta_refresh");
        if !refresh.is_empty() && has_refresh_target(refresh) {
            ctx.add(
                "META_REFRESH_REDIRECT",
                Some(&page.url),
                details([("meta_refresh", json!(refresh))]),
            );
        }
        let http = page.text("http_refresh");
        if !http.is_empty() && has_refresh_target(http) {
            ctx.add(
                "HTTP_REFRESH_REDIRECT",
                Some(&page.url),
                details([("refresh_header", json!(http))]),
            );
        }
    }
    if !ctx.has_column("http_refresh") {
        ctx.skip(
            "HTTP_REFRESH_REDIRECT",
            "no Refresh response header evidence (native crawl only)",
        );
    }
}

/// `canonical_identity` pieces the cross-origin row compares: (scheme, host).
fn identity(url: &str) -> Option<(String, String)> {
    let s = spiderhead_parse::py_url::urlsplit(&norm_url(url)).ok()?;
    let host = spiderhead_parse::urlutil::hostname(url).ok().flatten()?;
    Some((s.scheme.to_lowercase(), host))
}

/// `check_canonical_annotations`: canonicals that change host or protocol.
pub fn check_canonical_annotations(ctx: &mut AuditContext) {
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        let canonical = page.text("canonical").to_owned();
        if canonical.is_empty() {
            continue;
        }
        let Ok(target) = spiderhead_parse::py_url::urlsplit(&canonical) else {
            continue;
        };
        if !matches!(target.scheme.as_str(), "http" | "https")
            || spiderhead_parse::urlutil::hostname(&canonical)
                .ok()
                .flatten()
                .is_none()
        {
            continue;
        }
        let (Some(src), Some(dst)) = (identity(&page.url), identity(&canonical)) else {
            continue;
        };
        let mut differences = Vec::new();
        if src.1 != dst.1 {
            differences.push("host");
        }
        let source_scheme = spiderhead_parse::py_url::urlsplit(&page.url)
            .map(|s| s.scheme)
            .unwrap_or_default();
        if source_scheme != target.scheme {
            differences.push("protocol");
        }
        if !differences.is_empty() {
            ctx.add(
                "CANONICAL_CROSS_ORIGIN",
                Some(&page.url),
                details([
                    ("canonical_target_url", json!(canonical)),
                    ("differences", json!(differences)),
                    ("reason", "declared canonical changes host or protocol; confirm intended consolidation".into()),
                ]),
            );
        }
    }
    // Raw canonical attributes need the stored head-annotation evidence; an
    // export has none and says so, a native crawl measures through
    // `context_items` rows and reports how much it could cover.
    let (declarations, eligible) = {
        let Some(scan) = ctx.scan.as_ref() else {
            ctx.skip(
                "CANONICAL_INVALID_ATTRIBUTE",
                "raw canonical attributes are unavailable in this export",
            );
            return;
        };
        let Ok(d) = spiderhead_store::head_links::active(scan) else {
            return;
        };
        (
            d,
            spiderhead_store::head_links::eligible_count(scan).unwrap_or(i64::MAX),
        )
    };
    let mut measured = 0i64;
    for (source, decls) in declarations {
        let Some(idx) = ctx.page_index(&source) else {
            continue;
        };
        let page = &ctx.pages[idx];
        if !page.is_html() || !page.is_2xx() {
            continue;
        }
        let Some(decls) = decls else {
            continue;
        };
        measured += i64::from(decls.len() < 100);
        let mut invalid: Vec<Value> = Vec::new();
        for declaration in &decls {
            let rel = declaration.get("rel").and_then(Value::as_str).unwrap_or("");
            if !canonical_declared(rel) {
                continue;
            }
            let has_alternate = ["hreflang", "lang", "media", "type"]
                .iter()
                .any(|k| declaration.contains_key(*k));
            let reason = if has_alternate {
                Some("canonical includes alternate-version attributes ignored for canonicalization")
            } else if !rel_has_canonical(rel) {
                Some("canonical relation uses a non-HTML whitespace or punctuation separator")
            } else if declaration
                .get("href")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                Some("canonical href is empty or missing")
            } else {
                let target = declaration
                    .get("target")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                (!valid_http_target(target))
                    .then_some("canonical href does not resolve to a valid HTTP(S) URL")
            };
            if let Some(reason) = reason {
                let mut d = declaration.clone();
                d.insert("reason".into(), Value::from(reason));
                invalid.push(Value::Object(d));
            }
        }
        if !invalid.is_empty() {
            ctx.add_with(
                "CANONICAL_INVALID_ATTRIBUTE",
                Some(&source),
                details([("annotations", Value::Array(invalid.clone()))]),
                None,
                invalid.len() as i64,
                None,
            );
        }
    }
    if measured == 0 || measured < eligible {
        ctx.skip(
            "CANONICAL_INVALID_ATTRIBUTE",
            "not measured completely by this native crawl: raw canonical annotations were unavailable or reached the annotation limit",
        );
    }
}

/// The `(?ai:canonical)` rel-token bounded by whitespace, `,` or `;` — the
/// declaration gate before attribute judgment.
fn canonical_declared(rel: &str) -> bool {
    rel.split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .any(|t| t.eq_ignore_ascii_case("canonical"))
}

/// `_rel_has("canonical")`: rel tokens split on HTML whitespace only.
fn rel_has_canonical(rel: &str) -> bool {
    rel.split([' ', '\t', '\r', '\n', '\x0c'])
        .any(|t| t.eq_ignore_ascii_case("canonical"))
}

/// `urlsplit(target)` resolves to scheme http/https + hostname, with Cthe reference
/// `.port` ValueError on a malformed or out-of-range port.
fn valid_http_target(target: &str) -> bool {
    let Ok(parts) = spiderhead_parse::py_url::urlsplit(target) else {
        return false;
    };
    if !matches!(parts.scheme.as_str(), "http" | "https") {
        return false;
    }
    let netloc = parts.netloc.as_str();
    let authority = netloc.rsplit('@').next().unwrap_or(netloc);
    let host = authority
        .trim_start_matches('[')
        .split([':', ']'])
        .next()
        .unwrap_or("");
    if host.is_empty() {
        return false;
    }
    if let Some((_, port)) = authority.split_once(':') {
        if port.parse::<u16>().is_err() {
            return false;
        }
    }
    true
}

/// `check_canonical_chain`: a canonical target that itself canonicalises onward.
pub fn check_canonical_chain(ctx: &mut AuditContext) {
    let (edges, has_any) = canonical_edges(ctx);
    if !has_any {
        ctx.skip("CANONICAL_CHAIN", "no Canonical column in Internal:All");
        return;
    }
    let html = ctx.html_pages();
    let mut by_norm: HashMap<String, usize> = Default::default();
    for (i, p) in ctx.pages.iter().enumerate() {
        let key = norm_url(&p.url);
        let e = by_norm.entry(key).or_insert(i);
        if !ctx.pages[*e].is_2xx() && p.is_2xx() {
            *e = i;
        }
    }
    let mut unmeasured = 0;
    for &i in &html {
        let page = ctx.pages[i].clone();
        let start = norm_url(&page.url);
        let Some(b) = edges.get(&start).cloned() else {
            continue;
        };
        if !edges.contains_key(&b) {
            let target = by_norm.get(&b).map(|&t| &ctx.pages[t]);
            let unmeasured_target = match target {
                Some(t) => t.body_unavailable(),
                None => ctx
                    .target_probes
                    .get(&b)
                    .is_none_or(|p| p.get("canonical").is_none_or(Value::is_null)),
            };
            if unmeasured_target {
                unmeasured += 1;
            }
            continue;
        }
        let mut path = vec![start.clone()];
        let mut seen: std::collections::HashSet<String> = [start.clone()].into();
        let mut cur = start;
        let mut is_loop = false;
        for _ in 0..8 {
            let Some(next) = edges.get(&cur).cloned() else {
                break;
            };
            if seen.contains(&next) {
                is_loop = true;
                break;
            }
            path.push(next.clone());
            seen.insert(next.clone());
            cur = next;
        }
        let chain: Vec<String> = path
            .iter()
            .map(|n| {
                by_norm
                    .get(n)
                    .map_or(n.clone(), |&t| ctx.pages[t].url.clone())
            })
            .collect();
        ctx.add(
            "CANONICAL_CHAIN",
            Some(&page.url),
            details([
                ("chain", json!(chain)),
                ("depth", json!(path.len() - 1)),
                ("loop", json!(is_loop)),
            ]),
        );
    }
    if unmeasured > 0 {
        ctx.skip("CANONICAL_CHAIN", &format!("{unmeasured} canonical target(s) lack measured canonical annotations; not measured by this native crawl or export"));
    }
}

// ---------------------------------------------------------------------------
// Canonical target machinery (the reference suite and
// the reference suite)
// ---------------------------------------------------------------------------

/// reference truthiness on a JSON cell (`if value`, `value or x`).
fn json_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// `_canonical_edges`: non-self canonical edges over HTML pages plus stored
/// target-probe edges; `has_any` tracks whether any canonical value exists.
fn canonical_edges(ctx: &AuditContext) -> (HashMap<String, String>, bool) {
    let mut edges = HashMap::new();
    let mut has_any = false;
    for i in ctx.html_pages() {
        let canonical = ctx.pages[i].text("canonical");
        if canonical.is_empty() {
            continue;
        }
        has_any = true;
        let (a, b) = (norm_url(&ctx.pages[i].url), norm_url(canonical));
        if a != b {
            edges.insert(a, b);
        }
    }
    for (key, payload) in &ctx.target_probes {
        let Some(canonical) = payload
            .get("canonical")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
        else {
            continue;
        };
        let own = payload.get("url").and_then(Value::as_str).unwrap_or("");
        let different = match (
            spiderhead_store::target_probes::canonical_identity(canonical),
            spiderhead_store::target_probes::canonical_identity(own),
        ) {
            (Ok(a), Ok(b)) => a != b,
            _ => norm_url(canonical) != *key,
        };
        if different {
            edges.insert(key.clone(), norm_url(canonical));
        }
    }
    (edges, has_any)
}

/// `ProbedTarget` (the reference suite): what a stored target probe reports.
pub(crate) struct ProbedTarget {
    pub(crate) url: String,
    pub(crate) status_code: i64,
    #[allow(dead_code)]
    pub(crate) redirect_url: String,
    pub(crate) indexability: Option<String>,
    pub(crate) indexability_status: Option<String>,
}

impl ProbedTarget {
    pub(crate) fn is_2xx(&self) -> bool {
        (200..300).contains(&self.status_code)
    }
}

/// `_probed_targets`: the stored probe for a normalized URL the crawl never
/// captured; empty when the probe answered no integer status.
pub(crate) fn probed_targets(ctx: &AuditContext, key: &str) -> Vec<ProbedTarget> {
    let Some(payload) = ctx.target_probes.get(key).and_then(Value::as_object) else {
        return Vec::new();
    };
    // reference `type(payload.get("status_code")) is not int` — a JSON bool is not an int.
    let Some(status) = payload.get("status_code").and_then(Value::as_i64) else {
        return Vec::new();
    };
    let observed = spiderhead_store::target_probes::indexability(payload);
    let indexability = match observed.get("state").map(String::as_str) {
        Some("indexable") => Some("Indexable".to_owned()),
        Some("non_indexable") => Some("Non-Indexable".to_owned()),
        _ => None,
    };
    vec![ProbedTarget {
        url: payload
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        status_code: status,
        redirect_url: payload
            .get("redirect_url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        indexability,
        indexability_status: observed.get("reason").cloned(),
    }]
}

/// The uniform view the canonical-target checks share: every crawled page
/// under the normalized key, or the stored probe when the crawl never
/// captured the URL (`pages_by_norm.get(key) or _probed_targets(ctx, key)`).
enum Target {
    /// Index into `ctx.pages`.
    Page(usize),
    /// A stored probe answer.
    Probe(ProbedTarget),
}

impl Target {
    fn url<'a>(&'a self, ctx: &'a AuditContext) -> &'a str {
        match self {
            Target::Page(i) => &ctx.pages[*i].url,
            Target::Probe(p) => &p.url,
        }
    }

    fn status_code(&self, ctx: &AuditContext) -> Option<i64> {
        match self {
            Target::Page(i) => ctx.pages[*i].status_code,
            Target::Probe(p) => Some(p.status_code),
        }
    }

    /// `redirect_map.get(url) or getattr(t, "redirect_url", "")`: a page's
    /// stored `redirect_url` rec field, or the probe's own redirect answer.
    fn redirect_url<'a>(&'a self, ctx: &'a AuditContext) -> &'a str {
        match self {
            Target::Page(i) => ctx.pages[*i].text("redirect_url"),
            Target::Probe(p) => &p.redirect_url,
        }
    }

    /// The `or t.url` tail of the redirect-evidence tuple.
    fn redirect_or_self<'a>(&'a self, ctx: &'a AuditContext) -> &'a str {
        let r = self.redirect_url(ctx);
        if r.is_empty() {
            self.url(ctx)
        } else {
            r
        }
    }

    fn is_2xx(&self, ctx: &AuditContext) -> bool {
        match self {
            Target::Page(i) => ctx.pages[*i].is_2xx(),
            Target::Probe(p) => p.is_2xx(),
        }
    }

    fn indexability(&self, ctx: &AuditContext) -> Value {
        match self {
            Target::Page(i) => {
                let v = &ctx.pages[*i].indexability;
                if v.is_empty() {
                    Value::Null
                } else {
                    json!(v)
                }
            }
            Target::Probe(p) => p.indexability.as_ref().map_or(Value::Null, |v| json!(v)),
        }
    }

    fn indexability_status(&self, ctx: &AuditContext) -> Value {
        match self {
            Target::Page(i) => {
                let v = &ctx.pages[*i].indexability_status;
                if v.is_empty() {
                    Value::Null
                } else {
                    json!(v)
                }
            }
            Target::Probe(p) => p
                .indexability_status
                .as_ref()
                .map_or(Value::Null, |v| json!(v)),
        }
    }
}

/// `pages_by_norm.get(key) or _probed_targets(ctx, key)`: every page under the
/// normalized key in crawl order, else the probe rows.
fn canonical_targets(ctx: &AuditContext, key: &str) -> Vec<Target> {
    let idxs: Vec<usize> = ctx
        .pages
        .iter()
        .enumerate()
        .filter(|(_, p)| norm_url(&p.url) == *key)
        .map(|(i, _)| i)
        .collect();
    if idxs.is_empty() {
        probed_targets(ctx, key)
            .into_iter()
            .map(Target::Probe)
            .collect()
    } else {
        idxs.into_iter().map(Target::Page).collect()
    }
}

/// `check_canonical_extra`: relative, repeated and fragment canonicals.
pub fn check_canonical_extra(ctx: &mut AuditContext) {
    let has_canonical_2 = ctx.has_column("canonical_2");
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        let canonical = page.text("canonical").to_owned();
        let lower = canonical.to_lowercase();
        if !canonical.is_empty()
            && !(lower.starts_with("http://")
                || lower.starts_with("https://")
                || lower.starts_with("//"))
        {
            ctx.add(
                "CANONICAL_RELATIVE",
                Some(&page.url),
                details([("canonical", json!(canonical))]),
            );
        }
        if has_canonical_2 && !page.text("canonical_2").is_empty() {
            ctx.add(
                "CANONICAL_MULTIPLE",
                Some(&page.url),
                details([
                    ("canonical_1", page.opt("canonical")),
                    ("canonical_2", json!(page.text("canonical_2"))),
                ]),
            );
        }
        if !canonical.is_empty()
            && spiderhead_parse::py_url::urlsplit(&canonical)
                .map(|p| !p.fragment.is_empty())
                .unwrap_or(false)
        {
            ctx.add(
                "CANONICAL_FRAGMENT",
                Some(&page.url),
                details([("canonical", json!(canonical))]),
            );
        }
    }
    if !has_canonical_2 {
        ctx.skip(
            "CANONICAL_MULTIPLE",
            "no Canonical Link Element 2 column in Internal:All",
        );
    }
}

/// `_canonical_policy_target_is_fetched`: this exact policy URL has a response
/// row (scheme/netloc/path/query/fragment identity, not the folded norm key).
fn canonical_policy_target_is_fetched(ctx: &AuditContext, url: &str) -> bool {
    let Ok(target) = spiderhead_parse::py_url::urlsplit(url) else {
        return false;
    };
    let target_path = if target.path.is_empty() {
        "/"
    } else {
        target.path.as_str()
    };
    let identity = (
        target.scheme.to_lowercase(),
        target.netloc.to_lowercase(),
        target_path.to_owned(),
        target.query.clone(),
        target.fragment.clone(),
    );
    let key = norm_url(url);
    for page in &ctx.pages {
        if norm_url(&page.url) != key {
            continue;
        }
        if page.status_code.is_none() {
            continue;
        }
        let Ok(candidate) = spiderhead_parse::py_url::urlsplit(&page.url) else {
            continue;
        };
        let candidate_path = if candidate.path.is_empty() {
            "/"
        } else {
            candidate.path.as_str()
        };
        let candidate_identity = (
            candidate.scheme.to_lowercase(),
            candidate.netloc.to_lowercase(),
            candidate_path.to_owned(),
            candidate.query,
            candidate.fragment,
        );
        if candidate_identity == identity {
            return true;
        }
    }
    false
}

/// `check_canonical_policy`: configured pagination/filter canonicals against
/// the fetched URL evidence.
pub fn check_canonical_policy(ctx: &mut AuditContext) {
    let policy = ctx
        .config
        .get("canonical_policy")
        .cloned()
        .unwrap_or_else(|| json!({}));
    for (category, check_id) in [
        ("pagination", "PAGINATION_CANONICAL_POLICY"),
        ("filters", "FILTER_CANONICAL_POLICY"),
    ] {
        if !ctx.enabled(check_id) {
            continue;
        }
        // `if not rules`: reference truthiness on the raw configured value —
        // missing, null, false, empty array/object/string/number all mean
        // "not configured".
        let rules_empty = policy.get(category).is_none_or(|r| !json_truthy(r));
        if rules_empty {
            ctx.skip(
                check_id,
                &format!("no {category} canonical policy configured"),
            );
            continue;
        }
        if !ctx.has_column("canonical") {
            ctx.skip(check_id, "no Canonical column in Internal:All");
            continue;
        }
        /// `(page index, matched rule, expected canonical, declared canonical)`.
        type Candidate = (usize, Map<String, Value>, String, Option<String>);
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut unavailable: Vec<(String, String)> = Vec::new();
        for i in ctx.html_pages() {
            let page = &ctx.pages[i];
            let Some(rule) = spiderhead_core::canonical_policy::matching_canonical_rule(
                &policy, category, &page.url,
            )
            .ok()
            .flatten()
            .cloned()
            .and_then(|v| v.as_object().cloned()) else {
                continue;
            };
            if page.body_unavailable() {
                unavailable.push((page.url.clone(), "source HTML body unavailable".to_owned()));
                continue;
            }
            let expected = if rule.get("policy").and_then(Value::as_str) == Some("self") {
                page.url.clone()
            } else {
                rule.get("target")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            let canonical = page.text("canonical");
            let canonical = (!canonical.is_empty()).then(|| canonical.to_owned());
            if !canonical_policy_target_is_fetched(ctx, &expected) {
                unavailable.push((
                    page.url.clone(),
                    format!("expected target absent from crawl evidence: {expected}"),
                ));
                continue;
            }
            if let Some(c) = &canonical {
                if !canonical_policy_target_is_fetched(ctx, c) {
                    unavailable.push((
                        page.url.clone(),
                        format!("declared canonical absent from crawl evidence: {c}"),
                    ));
                    continue;
                }
            }
            candidates.push((i, rule, expected, canonical));
        }
        if candidates.is_empty() && unavailable.is_empty() {
            ctx.skip(
                check_id,
                &format!("no crawled URLs matched the configured {category} patterns"),
            );
            continue;
        }
        if !unavailable.is_empty() {
            let examples: Vec<String> = unavailable
                .iter()
                .take(3)
                .map(|(u, r)| format!("{u}: {r}"))
                .collect();
            ctx.skip(
                check_id,
                &format!(
                    "canonical policy evidence unavailable for {} matched URL(s); examples: {}",
                    unavailable.len(),
                    examples.join("; ")
                ),
            );
            continue;
        }
        for (i, rule, expected, canonical) in candidates {
            let page = &ctx.pages[i];
            if let Some(c) = &canonical {
                if norm_url(c) == norm_url(&expected) {
                    continue;
                }
            }
            let url = page.url.clone();
            ctx.add_evidence(
                check_id,
                Some(&url),
                None,
                1,
                (
                    Vec::new(),
                    [
                        ("frame".to_owned(), json!("Internal:All")),
                        (
                            "canonical_column".to_owned(),
                            json!("Canonical Link Element 1"),
                        ),
                        ("expected_target_in_crawl".to_owned(), json!(true)),
                    ]
                    .into_iter()
                    .collect(),
                ),
                details([(
                    "canonical_policy",
                    json!({
                        "source_url": url,
                        "declared_canonical_url": canonical,
                        "expected_canonical_url": expected,
                        "policy": rule.get("policy").cloned().unwrap_or(Value::Null),
                        "matched_pattern": rule.get("pattern").cloned().unwrap_or(Value::Null),
                    }),
                )]),
            );
        }
    }
}

/// `check_canonical_to_redirect`: a canonical target that is itself a 3xx.
pub fn check_canonical_to_redirect(ctx: &mut AuditContext) {
    let (_, has_any) = canonical_edges(ctx);
    if !has_any {
        ctx.skip(
            "CANONICAL_TO_REDIRECT",
            "no Canonical column in Internal:All",
        );
        return;
    }
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        let canonical = page.text("canonical").to_owned();
        if canonical.is_empty() || norm_url(&canonical) == norm_url(&page.url) {
            continue;
        }
        let targets = canonical_targets(ctx, &norm_url(&canonical));
        if targets.is_empty() {
            continue;
        }
        if targets.iter().any(|t| t.is_2xx(ctx)) {
            continue;
        }
        let redirecting: Vec<(String, Option<i64>, String)> = targets
            .iter()
            .filter(|t| {
                matches!(t.status_code(ctx), Some(c) if (300..=399).contains(&c))
                    || !t.redirect_url(ctx).is_empty()
            })
            .map(|t| {
                (
                    t.url(ctx).to_owned(),
                    t.status_code(ctx),
                    t.redirect_or_self(ctx).to_owned(),
                )
            })
            .collect();
        let Some((_, code, rurl)) = redirecting.first() else {
            continue;
        };
        ctx.add(
            "CANONICAL_TO_REDIRECT",
            Some(&page.url),
            details([
                ("canonical", json!(canonical)),
                (
                    "canonical_status_code",
                    code.map_or(Value::Null, Value::from),
                ),
                ("redirect_url", json!(rurl)),
            ]),
        );
    }
}

/// `check_canonical_target_error`: a fetched canonical target answers 4xx/5xx.
pub fn check_canonical_target_error(ctx: &mut AuditContext) {
    if !ctx.has_column("canonical") {
        ctx.skip(
            "CANONICAL_TARGET_ERROR",
            "no Canonical column in Internal:All",
        );
        return;
    }
    if !ctx.has_column("status_code") {
        ctx.skip(
            "CANONICAL_TARGET_ERROR",
            "no Status Code column in Internal:All",
        );
        return;
    }
    let has_any = ctx
        .html_pages()
        .iter()
        .any(|&i| !ctx.pages[i].text("canonical").is_empty());
    if !has_any {
        ctx.skip(
            "CANONICAL_TARGET_ERROR",
            "no canonical values in Internal:All",
        );
        return;
    }
    let mut issues: Vec<(usize, String, Vec<Target>)> = Vec::new();
    let mut unavailable = 0i64;
    for i in ctx.html_pages() {
        let canonical = ctx.pages[i].text("canonical").to_owned();
        if canonical.is_empty() || norm_url(&canonical) == norm_url(&ctx.pages[i].url) {
            continue;
        }
        let targets = canonical_targets(ctx, &norm_url(&canonical));
        if targets.is_empty() {
            unavailable += 1;
            continue;
        }
        let codes: Vec<Option<i64>> = targets.iter().map(|t| t.status_code(ctx)).collect();
        if codes.iter().any(Option::is_none) {
            unavailable += 1;
            continue;
        }
        if codes
            .iter()
            .flatten()
            .any(|c| (200..300).contains(c) || (300..400).contains(c))
        {
            continue;
        }
        if codes.iter().flatten().all(|c| (400..600).contains(c)) {
            issues.push((i, canonical, targets));
        } else {
            unavailable += 1;
        }
    }
    if unavailable > 0 && issues.is_empty() {
        ctx.skip(
            "CANONICAL_TARGET_ERROR",
            &format!(
                "{unavailable} canonical target(s) were not captured with classifiable response status"
            ),
        );
        return;
    }
    for (i, canonical, targets) in issues {
        let mut codes: Vec<i64> = targets.iter().filter_map(|t| t.status_code(ctx)).collect();
        codes.sort_unstable();
        codes.dedup();
        let status = (codes.len() == 1).then_some(codes[0]);
        let responses: Vec<Value> = targets
            .iter()
            .take(20)
            .map(|t| {
                json!({
                    "url": t.url(ctx),
                    "status_code": t.status_code(ctx),
                    "indexability": t.indexability(ctx),
                    "indexability_status": t.indexability_status(ctx),
                })
            })
            .collect();
        let omitted = targets.len().saturating_sub(20) as i64;
        let page = &ctx.pages[i];
        let url = page.url.clone();
        let source_status = page.status_code;
        let source_indexability = page.indexability.clone();
        ctx.add_with(
            "CANONICAL_TARGET_ERROR",
            Some(&url),
            details([
                ("source_url", json!(url)),
                (
                    "source_status_code",
                    source_status.map_or(Value::Null, Value::from),
                ),
                ("source_indexability", json!(source_indexability)),
                ("canonical_target_url", json!(canonical)),
                ("canonical_target_responses", json!(responses)),
                ("canonical_target_responses_omitted", json!(omitted)),
                (
                    "status_coverage",
                    json!(if unavailable > 0 {
                        "partial"
                    } else {
                        "complete"
                    }),
                ),
                ("unclassified_target_count", json!(unavailable)),
            ]),
            status,
            1,
            None,
        );
    }
}

/// `_PAGINATION_PATH_RE`: recognizable page-number paths excluded from the group.
fn pagination_path(path: &str) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(?:^|/)(?:page|paged)[/-]\d+(?:/|$)").unwrap())
        .is_match(path)
}

/// `split_port`: reference `urlsplit().port` — `Ok(None)` when absent, `Err` when
/// malformed or out of range (the `ValueError` path both helpers catch).
fn split_port(netloc: &str) -> Result<Option<u16>, ()> {
    let authority = netloc.rsplit('@').next().unwrap_or(netloc);
    if let Some(rest) = authority.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            return Err(());
        };
        let tail = &rest[end + 1..];
        if tail.is_empty() {
            return Ok(None);
        }
        let Some(port) = tail.strip_prefix(':') else {
            return Err(());
        };
        return port.parse::<u16>().map(Some).map_err(|_| ());
    }
    match authority.rsplit_once(':') {
        Some((_, p)) if !p.is_empty() => p.parse::<u16>().map(Some).map_err(|_| ()),
        _ => Ok(None),
    }
}

/// `_homepage_url`: an HTTP(S) origin root without query, fragment or userinfo.
fn homepage_url(url: &str) -> bool {
    let Ok(parts) = spiderhead_parse::py_url::urlsplit(url) else {
        return false;
    };
    matches!(parts.scheme.to_lowercase().as_str(), "http" | "https")
        && spiderhead_parse::urlutil::hostname(url)
            .ok()
            .flatten()
            .is_some()
        && (parts.path.is_empty() || parts.path == "/")
        && parts.query.is_empty()
        && parts.fragment.is_empty()
        && !parts.netloc.contains('@')
}

/// `_same_home_origin`: same scheme, hostname and effective port.
fn same_home_origin(source: &str, target: &str) -> bool {
    let (Ok(s), Ok(t)) = (
        spiderhead_parse::py_url::urlsplit(source),
        spiderhead_parse::py_url::urlsplit(target),
    ) else {
        return false;
    };
    let (Ok(sp), Ok(tp)) = (split_port(&s.netloc), split_port(&t.netloc)) else {
        return false;
    };
    let default = |scheme: &str| match scheme.to_lowercase().as_str() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    let sh = spiderhead_parse::urlutil::hostname(source)
        .ok()
        .flatten()
        .unwrap_or_default()
        .to_lowercase();
    let th = spiderhead_parse::urlutil::hostname(target)
        .ok()
        .flatten()
        .unwrap_or_default()
        .to_lowercase();
    s.scheme.to_lowercase() == t.scheme.to_lowercase()
        && sh == th
        && sp.or_else(|| default(&s.scheme)) == tp.or_else(|| default(&t.scheme))
}

/// `check_canonical_homepage_group`: 3+ distinct indexable pages across 2+
/// top-level sections all canonicalized to the fetched homepage.
pub fn check_canonical_homepage_group(ctx: &mut AuditContext) {
    let missing: Vec<&str> = ["canonical", "status_code", "indexability", "title", "h1"]
        .iter()
        .filter(|f| !ctx.has_column(f))
        .copied()
        .collect();
    if !missing.is_empty() {
        ctx.skip(
            "CANONICAL_HOMEPAGE_GROUP",
            &format!(
                "missing supporting Internal:All columns: {}",
                missing.join(", ")
            ),
        );
        return;
    }
    let pages = ctx.html_pages();
    if !ctx
        .indexable_html_pages()
        .iter()
        .any(|&i| !ctx.pages[i].text("canonical").is_empty())
    {
        ctx.skip(
            "CANONICAL_HOMEPAGE_GROUP",
            "no canonical values in fetched indexable HTML",
        );
        return;
    }
    let configured_policy = ctx.config.get("canonical_policy").cloned();
    // `isinstance(policy, dict) and any(policy.get(key) for key in ...)`.
    let has_policy = configured_policy.as_ref().is_some_and(|p| {
        p.is_object()
            && ["pagination", "filters"]
                .iter()
                .any(|k| p.get(*k).is_some_and(json_truthy))
    });
    let policy = has_policy.then_some(configured_policy).flatten();

    let mut grouped: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    let mut unavailable_home_targets = 0i64;
    let mut unavailable_sources = 0i64;
    for &i in &pages {
        let page = &ctx.pages[i];
        let canonical = page.text("canonical");
        if canonical.is_empty()
            || !homepage_url(canonical)
            || !same_home_origin(&page.url, canonical)
        {
            continue;
        }
        let Ok(source_parts) = spiderhead_parse::py_url::urlsplit(&page.url) else {
            continue;
        };
        if source_parts.path.is_empty()
            || source_parts.path == "/"
            || !source_parts.query.is_empty()
            || !source_parts.fragment.is_empty()
        {
            continue;
        }
        if pagination_path(&source_parts.path) {
            continue;
        }
        if let Some(p) = &policy {
            if ["pagination", "filters"].iter().any(|category| {
                spiderhead_core::canonical_policy::matching_canonical_rule(p, category, &page.url)
                    .is_ok_and(|rule| rule.is_some())
            }) {
                continue;
            }
        }
        if page.indexability.trim().is_empty() {
            unavailable_sources += 1;
            continue;
        }
        if !page.is_indexable() {
            continue;
        }
        let section = source_parts
            .path
            .split('/')
            .find(|s| !s.is_empty())
            .unwrap_or("")
            .to_owned();
        if section.is_empty() {
            continue;
        }
        let title = page.text("title").trim().to_owned();
        let h1 = page.text("h1").trim().to_owned();
        if title.is_empty() || h1.is_empty() || page.body_unavailable() {
            unavailable_sources += 1;
            continue;
        }
        let home_key = norm_url(canonical);
        let home_rows: Vec<usize> = ctx
            .pages
            .iter()
            .enumerate()
            .filter(|(_, p)| norm_url(&p.url) == home_key)
            .map(|(i, _)| i)
            .collect();
        let home = home_rows
            .iter()
            .copied()
            .find(|&t| ctx.pages[t].is_2xx() && ctx.pages[t].is_indexable());
        let Some(home) = home else {
            if home_rows.is_empty()
                || home_rows.iter().any(|&t| {
                    ctx.pages[t].status_code.is_none()
                        || (ctx.pages[t].is_2xx() && ctx.pages[t].indexability.trim().is_empty())
                })
            {
                unavailable_home_targets += 1;
            }
            continue;
        };
        let home_page = &ctx.pages[home];
        let home_title = caseless::default_case_fold_str(home_page.text("title").trim());
        let home_h1 = caseless::default_case_fold_str(home_page.text("h1").trim());
        if home_title.is_empty() || home_h1.is_empty() || home_page.body_unavailable() {
            unavailable_home_targets += 1;
            continue;
        }
        if caseless::default_case_fold_str(&title) == home_title
            || caseless::default_case_fold_str(&h1) == home_h1
        {
            continue;
        }
        grouped.entry(home_key).or_default().push((i, section));
    }

    let mut found = false;
    let mut sorted: Vec<(String, Vec<(usize, String)>)> = grouped.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (home_key, candidates) in sorted {
        let mut by_url: std::collections::BTreeMap<String, (usize, String)> = Default::default();
        for (i, section) in candidates {
            by_url.insert(ctx.pages[i].url.clone(), (i, section));
        }
        let candidates: Vec<(usize, String)> = by_url.into_values().collect();
        let titles: std::collections::HashSet<String> = candidates
            .iter()
            .map(|(i, _)| caseless::default_case_fold_str(ctx.pages[*i].text("title").trim()))
            .collect();
        let headings: std::collections::HashSet<String> = candidates
            .iter()
            .map(|(i, _)| caseless::default_case_fold_str(ctx.pages[*i].text("h1").trim()))
            .collect();
        let sections: std::collections::BTreeSet<String> = candidates
            .iter()
            .map(|(_, s)| caseless::default_case_fold_str(s))
            .collect();
        if candidates.len() < 3 || sections.len() < 2 || titles.len() < 3 || headings.len() < 3 {
            continue;
        }
        let home_rows: Vec<usize> = ctx
            .pages
            .iter()
            .enumerate()
            .filter(|(_, p)| norm_url(&p.url) == home_key)
            .map(|(i, _)| i)
            .collect();
        let Some(home) = home_rows
            .iter()
            .copied()
            .find(|&t| ctx.pages[t].is_2xx() && ctx.pages[t].is_indexable())
        else {
            continue;
        };
        let home_page = &ctx.pages[home];
        let home_url = home_page.url.clone();
        let home_status = home_page.status_code;
        let home_indexability = home_page.indexability.clone();
        let source_count = candidates.len() as i64;
        let section_list: Vec<Value> = sections.iter().map(|s| json!(s)).collect();
        let (title_count, heading_count) = (titles.len() as i64, headings.len() as i64);
        for (i, section) in candidates {
            let page = &ctx.pages[i];
            let url = page.url.clone();
            let code = page.status_code;
            let source_title = page.opt("title");
            let source_h1 = page.opt("h1");
            ctx.add_with(
                "CANONICAL_HOMEPAGE_GROUP",
                Some(&url),
                details([
                    ("canonical_target_url", json!(home_url)),
                    (
                        "canonical_target_status_code",
                        home_status.map_or(Value::Null, Value::from),
                    ),
                    ("canonical_target_indexability", json!(home_indexability)),
                    ("source_path_section", json!(section)),
                    ("source_title", source_title),
                    ("source_h1", source_h1),
                    ("source_count", json!(source_count)),
                    ("source_sections", json!(section_list)),
                    ("source_titles_distinct", json!(title_count)),
                    ("source_h1_distinct", json!(heading_count)),
                    ("minimum_sources", json!(3)),
                    ("minimum_sections", json!(2)),
                    (
                        "evidence_rule",
                        json!("fetched indexable sources have distinct non-empty titles and H1s, span multiple top-level path sections, and exclude query/pagination URLs"),
                    ),
                    (
                        "unavailable_home_target_count",
                        json!(unavailable_home_targets),
                    ),
                    (
                        "unavailable_source_evidence_count",
                        json!(unavailable_sources),
                    ),
                ]),
                code,
                1,
                None,
            );
            if let Some(issue) = ctx.issues.last_mut() {
                issue.locations = vec![json!({
                    "source_url": url,
                    "canonical_target_url": home_url,
                })];
            }
        }
        found = true;
    }
    if (unavailable_home_targets > 0 || unavailable_sources > 0) && !found {
        ctx.skip(
            "CANONICAL_HOMEPAGE_GROUP",
            &format!(
                "{unavailable_home_targets} homepage target(s) and {unavailable_sources} source page(s) lack complete evidence"
            ),
        );
    }
}

/// `check_unlinked_canonical`: a canonical target no hyperlink ever points to.
pub fn check_unlinked_canonical(ctx: &mut AuditContext) {
    let (edges, has_any) = canonical_edges(ctx);
    if !has_any {
        ctx.skip("UNLINKED_CANONICAL", "no Canonical column in Internal:All");
        return;
    }
    if !ctx
        .pages
        .iter()
        .any(|p| p.rec.get("inlinks").is_some_and(|v| !v.is_null()))
    {
        ctx.skip("UNLINKED_CANONICAL", "no Inlinks column in Internal:All");
        return;
    }
    let mut by_norm_rep: HashMap<String, usize> = HashMap::new();
    for (i, p) in ctx.pages.iter().enumerate() {
        let key = norm_url(&p.url);
        let e = by_norm_rep.entry(key).or_insert(i);
        if !ctx.pages[*e].is_2xx() && p.is_2xx() {
            *e = i;
        }
    }
    let mut sources_by_target: HashMap<String, Vec<String>> = HashMap::new();
    for (source, target) in &edges {
        sources_by_target
            .entry(target.clone())
            .or_default()
            .push(source.clone());
    }
    for (target_norm, source_norms) in &sources_by_target {
        let Some(&t) = by_norm_rep.get(target_norm) else {
            continue;
        };
        let target = &ctx.pages[t];
        if target.num("crawl_depth") == Some(0.0) {
            continue;
        }
        let inlinks = target.opt("inlinks");
        if inlinks.is_null() || inlinks.as_f64().is_some_and(|n| n > 0.0) {
            continue;
        }
        let mut sources: Vec<String> = source_norms
            .iter()
            .map(|n| {
                by_norm_rep
                    .get(n)
                    .map_or(n.clone(), |&i| ctx.pages[i].url.clone())
            })
            .collect();
        sources.sort();
        let url = target.url.clone();
        ctx.add(
            "UNLINKED_CANONICAL",
            Some(&url),
            details([("canonicalized_from", json!(sources))]),
        );
    }
}

/// `check_ajax_crawling_scheme`: hash-bang URLs and `meta fragment` markers.
pub fn check_ajax_crawling_scheme(ctx: &mut AuditContext) {
    let has_outlinks = ctx.has_column("ajax_scheme_outlinks");
    let has_meta = ctx.has_column("meta_fragment");
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        if has_outlinks {
            // `rec.get(...) or 0`: reference truthiness on the raw cell.
            let declared = page
                .rec
                .get("ajax_scheme_outlinks")
                .filter(|v| json_truthy(v))
                .cloned()
                .unwrap_or(Value::from(0));
            let self_uses = spiderhead_parse::parser::shared::uses_ajax_crawling_scheme(&page.url);
            if json_truthy(&declared) || self_uses {
                ctx.add(
                    "AJAX_CRAWLING_SCHEME_URL",
                    Some(&page.url),
                    details([
                        ("own_url_uses_scheme", json!(self_uses)),
                        ("outlinks_using_scheme", declared),
                    ]),
                );
            }
        }
        if has_meta {
            let fragment = page.text("meta_fragment").trim().to_owned();
            if fragment == "!" {
                ctx.add(
                    "AJAX_CRAWLING_SCHEME_META_FRAGMENT",
                    Some(&page.url),
                    details([("meta_fragment", json!(fragment))]),
                );
            }
        }
    }
    if !has_outlinks {
        ctx.skip(
            "AJAX_CRAWLING_SCHEME_URL",
            "no AJAX-scheme URL evidence (native crawl only)",
        );
    }
    if !has_meta {
        ctx.skip(
            "AJAX_CRAWLING_SCHEME_META_FRAGMENT",
            "no <meta name=\"fragment\"> evidence (native crawl only)",
        );
    }
}

// ---------------------------------------------------------------------------
// `check_redirect_chains` (export path — the stored scan resolves hop evidence
// with indexed SQL lookups and returns early, exactly like the oracle).
// ---------------------------------------------------------------------------

/// `DEFAULT_HOP_CAP` (the reference suite): the bounded-walk limit.
const DEFAULT_HOP_CAP: i64 = 20;

/// `resolve_redirect_chains`: walk every redirecting URL to its terminus.
fn resolve_redirect_chains(
    map: &indexmap::IndexMap<String, String>,
    hop_cap: i64,
) -> indexmap::IndexMap<String, (&'static str, i64, Option<String>)> {
    let mut results = indexmap::IndexMap::new();
    for start in map.keys() {
        let mut chain = vec![start.clone()];
        let mut current = start.clone();
        let mut outcome: Option<(&'static str, i64, Option<String>)> = None;
        loop {
            let Some(target) = map.get(&current) else {
                let hops = chain.len() as i64 - 1;
                outcome = Some((
                    if hops >= 2 { "chain" } else { "single" },
                    hops,
                    Some(current.clone()),
                ));
                break;
            };
            if chain.contains(target) {
                outcome = Some(("loop", chain.len() as i64, None));
                break;
            }
            if chain.len() as i64 > hop_cap {
                break;
            }
            chain.push(target.clone());
            current = target.clone();
        }
        let outcome = outcome.unwrap_or(("unresolved", chain.len() as i64 - 1, None));
        results.insert(start.clone(), outcome);
    }
    results
}

/// `redirect_hop_path`: every URL the walk from `start` visits, in order.
fn redirect_hop_path(
    map: &indexmap::IndexMap<String, String>,
    start: &str,
    hop_cap: i64,
) -> Vec<String> {
    let mut path = vec![start.to_owned()];
    let mut seen: std::collections::HashSet<String> = [start.to_owned()].into();
    while let Some(target) = map.get(path.last().unwrap_or(&String::new())).cloned() {
        if seen.contains(&target) {
            path.push(target);
            break;
        }
        if path.len() as i64 > hop_cap {
            break;
        }
        seen.insert(target.clone());
        path.push(target);
    }
    path
}

/// `check_redirect_chains`: resolve chains and loops over `ctx.redirect_map`.
/// On a stored scan the native evidence path emits them instead (early return).
pub fn check_redirect_chains(ctx: &mut AuditContext) {
    if ctx.scan.is_some() {
        return;
    }
    // A present non-empty Redirects:Redirect Chains export frame would be read
    // here; neither the gen frames nor a native stored scan carry one.
    if !ctx.has_column("redirect_url") {
        ctx.skip(
            "REDIRECT_CHAIN",
            "no redirect data (Internal:All has no Redirect URL column)",
        );
        ctx.skip(
            "REDIRECT_LOOP",
            "no redirect data (Internal:All has no Redirect URL column)",
        );
        return;
    }
    let mut redirect_map: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
    for page in &ctx.pages {
        let target = page.text("redirect_url");
        if !target.is_empty() {
            redirect_map.insert(page.url.clone(), target.to_owned());
        }
    }
    let hop_cap = ctx
        .thresholds
        .get("redirect_hop_cap")
        .map_or(DEFAULT_HOP_CAP, |v| v as i64);
    let resolved = resolve_redirect_chains(&redirect_map, hop_cap);
    for (start, (kind, hops, final_url)) in &resolved {
        if *kind == "single" {
            continue;
        }
        let path = redirect_hop_path(&redirect_map, start, hop_cap);
        // Detail order is the oracle's dict order: hops, final_url, the
        // unresolved flag (when present), path, then the inbound evidence
        // (`_inbound_detail(None, start)` — no all_inlinks export here).
        let mut d: Map<String, Value> = Map::new();
        d.insert("hops".into(), json!(hops));
        d.insert(
            "final_url".into(),
            final_url.clone().map_or(Value::Null, Value::from),
        );
        if *kind == "unresolved" {
            d.insert("unresolved".into(), json!(true));
        }
        d.insert("path".into(), json!(path));
        d.insert("inbound_links".into(), Value::Null);
        d.insert(
            "inbound_links_reason".into(),
            json!("no all_inlinks export"),
        );
        match *kind {
            "loop" => ctx.add("REDIRECT_LOOP", Some(start), d),
            _ => ctx.add("REDIRECT_CHAIN", Some(start), d),
        }
    }
}
