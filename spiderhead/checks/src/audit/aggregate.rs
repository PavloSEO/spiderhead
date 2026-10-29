//! Finding aggregation: de-duplication, stable order, ids, fingerprints and page back-links
//! (reference the reference suite, in-memory path).

use std::collections::{BTreeMap, HashMap};

use sha1::{Digest, Sha1};

use super::context::AuditContext;
use super::model::Issue;

/// reference `json.dumps(str, ensure_ascii=True)` — ASCII-safe quoting of one string.
fn py_json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x7f => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

/// `_fingerprint`: first 12 hex digits of SHA-1 over `check|target_url|status_code`,
/// plus `|json.dumps(dedupe_key[2:])` for image-targeted findings.
fn fingerprint(issue: &Issue) -> String {
    let target = issue.target_url.as_deref().unwrap_or("None");
    let status = issue
        .status_code
        .map_or("None".to_owned(), |s| s.to_string());
    let mut basis = format!("{}|{target}|{status}", issue.check);
    if issue.image_source().is_some() {
        let (_check, _url, source, scope) = issue.dedupe_key();
        let scope_json = if scope.is_empty() {
            "null".to_owned()
        } else {
            py_json_string(&scope)
        };
        basis = format!("{basis}|[{}, {scope_json}]", py_json_string(&source));
    }
    let digest = Sha1::digest(basis.as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// `_dedupe`: a repeated dedupe identity merges into its first occurrence.
fn dedupe(issues: Vec<Issue>) -> Vec<Issue> {
    let mut seen: HashMap<(String, String, String, String), usize> = HashMap::new();
    let mut out: Vec<Issue> = Vec::new();
    for issue in issues {
        let key = issue.dedupe_key();
        if issue.target_url.is_some() && seen.contains_key(&key) {
            let at = seen[&key];
            out[at].merge(&issue);
            continue;
        }
        seen.insert(key, out.len());
        out.push(issue);
    }
    out
}

fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 0,
        "warning" => 1,
        "notice" => 2,
        _ => 3,
    }
}

/// Dedupe, sort by (severity, check, target), number the findings and link them to their pages.
pub fn aggregate(ctx: &mut AuditContext) {
    let mut issues = dedupe(std::mem::take(&mut ctx.issues));
    issues.sort_by(|a, b| {
        (
            severity_rank(&a.severity),
            &a.check,
            a.target_url
                .as_deref()
                .map_or("None".to_owned(), str::to_owned),
        )
            .cmp(&(
                severity_rank(&b.severity),
                &b.check,
                b.target_url
                    .as_deref()
                    .map_or("None".to_owned(), str::to_owned),
            ))
    });
    let by_url: BTreeMap<String, usize> = ctx
        .pages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.url.clone(), i))
        .collect();
    for (n, issue) in issues.iter_mut().enumerate() {
        let id = format!("ISSUE-{:06}", n + 1);
        issue.fingerprint = Some(fingerprint(issue));
        if let Some(&page) = issue.target_url.as_ref().and_then(|u| by_url.get(u)) {
            let page = &mut ctx.pages[page];
            if !page.issues.contains(&issue.check) {
                page.issues.push(issue.check.clone());
            }
            page.issue_ids.push(id.clone());
        }
        issue.id = Some(id);
    }
    ctx.issues = issues;
}
