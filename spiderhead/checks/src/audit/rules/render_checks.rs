//! Raw-versus-rendered findings from retained browser captures
//! (reference the reference suite).

use serde_json::{json, Value};

use super::details;
use crate::audit::AuditContext;

/// Element-difference checks in registry order (used for the shared skips).
const ELEMENT_CHECKS: [&str; 10] = [
    "NOINDEX_ONLY_IN_RAW_HTML",
    "NOFOLLOW_ONLY_IN_RAW_HTML",
    "CANONICAL_MISMATCH",
    "CANONICAL_ONLY_IN_RENDERED_HTML",
    "PAGE_TITLE_ONLY_IN_RENDERED_HTML",
    "PAGE_TITLE_UPDATED_BY_JAVASCRIPT",
    "META_DESCRIPTION_ONLY_IN_RENDERED_HTML",
    "META_DESCRIPTION_UPDATED_BY_JAVASCRIPT",
    "H1_ONLY_IN_RENDERED_HTML",
    "H1_UPDATED_BY_JAVASCRIPT",
];

const NETWORK_CHECK: &str = "RENDER_RESOURCE_BLOCKED";

/// Check codes a rule name maps to before the raw-directive refinement.
fn rule_checks(rule: Option<&str>) -> Vec<&'static str> {
    match rule {
        Some("noindex_nofollow_raw_only") => {
            vec!["NOINDEX_ONLY_IN_RAW_HTML", "NOFOLLOW_ONLY_IN_RAW_HTML"]
        }
        Some("canonical_mismatch") => vec!["CANONICAL_MISMATCH"],
        Some("canonical_only_after_render") => vec!["CANONICAL_ONLY_IN_RENDERED_HTML"],
        _ => Vec::new(),
    }
}

/// Check code for a `(field, kind)` pair of a field-level difference.
fn field_check(field: Option<&str>, kind: Option<&str>) -> Option<&'static str> {
    Some(match (field?, kind?) {
        ("title", "rendered_only") => "PAGE_TITLE_ONLY_IN_RENDERED_HTML",
        ("title", "changed") => "PAGE_TITLE_UPDATED_BY_JAVASCRIPT",
        ("description", "rendered_only") => "META_DESCRIPTION_ONLY_IN_RENDERED_HTML",
        ("description", "changed") => "META_DESCRIPTION_UPDATED_BY_JAVASCRIPT",
        ("h1", "rendered_only") => "H1_ONLY_IN_RENDERED_HTML",
        ("h1", "changed") => "H1_UPDATED_BY_JAVASCRIPT",
        _ => return None,
    })
}

/// `str(value or "")` for the raw robots directive text.
fn py_str_or_empty(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Emit element differences; returns whether the retained contract was malformed.
fn element_findings(ctx: &mut AuditContext, url: &str, evidence: &Value) -> bool {
    let mut malformed = false;
    let rules = match evidence.get("rules") {
        Some(Value::Array(items)) => items.as_slice(),
        _ => &[],
    };
    for entry in rules {
        let Some(diff) = entry.get("diff").and_then(Value::as_object) else {
            malformed = true;
            continue;
        };
        let rule = entry.get("rule").and_then(Value::as_str);
        let field = diff.get("field");
        let kind = diff.get("kind");
        let mut check_ids = rule_checks(rule);
        if rule == Some("noindex_nofollow_raw_only") {
            let raw = py_str_or_empty(diff.get("raw"))
                .to_lowercase()
                .replace(',', " ");
            let tokens: Vec<&str> = raw.split_whitespace().collect();
            check_ids = [
                ("noindex", "NOINDEX_ONLY_IN_RAW_HTML"),
                ("nofollow", "NOFOLLOW_ONLY_IN_RAW_HTML"),
            ]
            .into_iter()
            .filter(|(token, _)| tokens.contains(token))
            .map(|(_, code)| code)
            .collect();
        }
        if let Some(code) = field_check(field.and_then(Value::as_str), kind.and_then(Value::as_str))
        {
            check_ids.push(code);
        }
        for code in check_ids {
            ctx.add(
                code,
                Some(url),
                details([
                    ("field", field.cloned().unwrap_or(Value::Null)),
                    ("raw", diff.get("raw").cloned().unwrap_or(Value::Null)),
                    (
                        "rendered",
                        diff.get("rendered").cloned().unwrap_or(Value::Null),
                    ),
                ]),
            );
        }
    }
    malformed
}

/// Emit blocked requests; returns whether the retained bounded log was partial.
fn network_findings(ctx: &mut AuditContext, url: &str, evidence: &Value) -> bool {
    let mut partial = evidence.get("state").and_then(Value::as_str) == Some("partial");
    let resources = match evidence.get("resources") {
        Some(Value::Array(items)) => items.as_slice(),
        _ => &[],
    };
    for resource in resources {
        let Some(target) = resource.get("url").and_then(Value::as_str) else {
            partial = true;
            continue;
        };
        let get = |key: &str| resource.get(key).cloned().unwrap_or(Value::Null);
        ctx.add(
            NETWORK_CHECK,
            Some(target),
            details([
                ("source_page", json!(url)),
                ("resource_type", get("resource_type")),
                ("reason", get("reason")),
                ("status_code", get("status_code")),
            ]),
        );
    }
    partial
}

/// Rendered pages' `(url, renderer_json)` rows, URL order, as the audit reads them.
pub(crate) fn rendered_documents(con: &rusqlite::Connection) -> Vec<(String, String)> {
    let sql = "SELECT u.url,d.renderer_json FROM pages p JOIN urls u ON u.url_id=p.url_id \
               JOIN documents d ON d.document_id=p.document_id \
               WHERE p.representation='rendered' AND d.renderer_json IS NOT NULL ORDER BY u.url";
    let Ok(mut stmt) = con.prepare(sql) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    else {
        return Vec::new();
    };
    rows.flatten().collect()
}

/// Emit differences and blocked requests only from retained rendered evidence.
pub fn check_rendered_elements(ctx: &mut AuditContext) {
    let Some(con) = ctx.scan.take() else {
        for code in ELEMENT_CHECKS.iter().chain([&NETWORK_CHECK]) {
            ctx.skip(code, "requires retained native rendered evidence");
        }
        return;
    };
    let (mut element_measured, mut element_unavailable, mut element_partial) = (0, 0, false);
    let (mut network_measured, mut network_unavailable, mut network_partial) = (0, 0, false);
    for (url, raw) in rendered_documents(&con) {
        let renderer = serde_json::from_str::<Value>(&raw)
            .ok()
            .filter(Value::is_object);
        let elements = renderer
            .as_ref()
            .and_then(|r| r.get("raw_rendered_evidence"));
        let network = renderer.as_ref().and_then(|r| r.get("network_evidence"));
        match elements {
            Some(e)
                if e.is_object() && e.get("state").and_then(Value::as_str) == Some("complete") =>
            {
                element_measured += 1;
                element_partial |= element_findings(ctx, &url, e);
            }
            _ => element_unavailable += 1,
        }
        match network {
            Some(n)
                if n.is_object()
                    && matches!(
                        n.get("state").and_then(Value::as_str),
                        Some("complete" | "partial")
                    ) =>
            {
                network_measured += 1;
                network_partial |= network_findings(ctx, &url, n);
            }
            _ => network_unavailable += 1,
        }
    }
    if element_measured == 0 {
        let reason = if element_unavailable == 0 {
            "no retained raw-versus-rendered element comparisons were available"
        } else {
            "retained raw-versus-rendered element comparisons were unavailable"
        };
        for code in ELEMENT_CHECKS {
            ctx.skip(code, reason);
        }
    } else if element_unavailable > 0 || element_partial {
        for code in ELEMENT_CHECKS {
            ctx.skip(
                code,
                "one or more raw-versus-rendered comparisons were partial or unavailable",
            );
        }
    }
    if network_measured == 0 {
        ctx.skip(
            NETWORK_CHECK,
            "no retained rendered network log was available",
        );
    } else if network_unavailable > 0 || network_partial {
        ctx.skip(
            NETWORK_CHECK,
            "one or more rendered network logs were partial or unavailable",
        );
    }
    ctx.scan = Some(con);
}
