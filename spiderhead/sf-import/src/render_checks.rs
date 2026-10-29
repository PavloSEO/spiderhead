//! Raw-versus-rendered findings from retained browser captures.
//!
//! Findings come only from the renderer evidence stored with a scan; without a
//! scan connection every check is reported as skipped, never as a pass.

use serde_json::{json, Map, Value};

use crate::context::{AddOpts, AuditContext, ContextError};
use crate::pyvalue::{str_or_dumps, truthy};

/// Element differences that concern a rule rather than one field.
fn rule_checks(rule: &str) -> &'static [&'static str] {
    match rule {
        "noindex_nofollow_raw_only" => &["NOINDEX_ONLY_IN_RAW_HTML", "NOFOLLOW_ONLY_IN_RAW_HTML"],
        "canonical_mismatch" => &["CANONICAL_MISMATCH"],
        "canonical_only_after_render" => &["CANONICAL_ONLY_IN_RENDERED_HTML"],
        _ => &[],
    }
}

/// Check raised for a `(field, kind)` difference.
fn field_check(field: &str, kind: &str) -> Option<&'static str> {
    Some(match (field, kind) {
        ("title", "rendered_only") => "PAGE_TITLE_ONLY_IN_RENDERED_HTML",
        ("title", "changed") => "PAGE_TITLE_UPDATED_BY_JAVASCRIPT",
        ("description", "rendered_only") => "META_DESCRIPTION_ONLY_IN_RENDERED_HTML",
        ("description", "changed") => "META_DESCRIPTION_UPDATED_BY_JAVASCRIPT",
        ("h1", "rendered_only") => "H1_ONLY_IN_RENDERED_HTML",
        ("h1", "changed") => "H1_UPDATED_BY_JAVASCRIPT",
        _ => return None,
    })
}

/// Every element-difference check, in registry order.
pub const ELEMENT_CHECKS: [&str; 10] = [
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

/// Element checks plus the blocked-resource check.
pub fn all_checks() -> Vec<&'static str> {
    let mut v = Vec::from(ELEMENT_CHECKS);
    v.push("RENDER_RESOURCE_BLOCKED");
    v
}

/// Emit element differences; true when the retained contract was malformed.
fn element_findings(
    ctx: &mut AuditContext,
    url: &str,
    evidence: &Value,
) -> Result<bool, ContextError> {
    let mut malformed = false;
    let empty = Vec::new();
    let entries = evidence
        .get("rules")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    for entry in entries {
        let Some(diff) = entry.get("diff").filter(|d| d.is_object()) else {
            malformed = true;
            continue;
        };
        let rule = entry.get("rule").and_then(Value::as_str).unwrap_or("");
        let field = diff.get("field").and_then(Value::as_str).unwrap_or("");
        let kind = diff.get("kind").and_then(Value::as_str).unwrap_or("");
        let mut check_ids: Vec<&str> = rule_checks(rule).to_vec();
        if rule == "noindex_nofollow_raw_only" {
            let raw = diff
                .get("raw")
                .filter(|v| truthy(v))
                .map(str_or_dumps)
                .unwrap_or_default();
            let directives: Vec<String> = raw
                .to_lowercase()
                .replace(',', " ")
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            check_ids = [
                ("noindex", "NOINDEX_ONLY_IN_RAW_HTML"),
                ("nofollow", "NOFOLLOW_ONLY_IN_RAW_HTML"),
            ]
            .into_iter()
            .filter(|(token, _)| directives.iter().any(|d| d == token))
            .map(|(_, code)| code)
            .collect();
        }
        if let Some(c) = field_check(field, kind) {
            check_ids.push(c);
        }
        for check_id in check_ids {
            let mut d = Map::new();
            d.insert(
                "field".into(),
                diff.get("field").cloned().unwrap_or(Value::Null),
            );
            d.insert(
                "raw".into(),
                diff.get("raw").cloned().unwrap_or(Value::Null),
            );
            d.insert(
                "rendered".into(),
                diff.get("rendered").cloned().unwrap_or(Value::Null),
            );
            ctx.add(
                check_id,
                AddOpts {
                    target_url: Some(url.to_owned()),
                    details: d,
                    ..AddOpts::default()
                },
            )?;
        }
    }
    Ok(malformed)
}

/// Emit blocked-request findings; true when the retained log was partial.
fn network_findings(
    ctx: &mut AuditContext,
    url: &str,
    evidence: &Value,
) -> Result<bool, ContextError> {
    let mut partial = evidence.get("state").and_then(Value::as_str) == Some("partial");
    let empty = Vec::new();
    for resource in evidence
        .get("resources")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
    {
        let Some(target) = resource.get("url").and_then(Value::as_str) else {
            partial = true;
            continue;
        };
        let mut d = Map::new();
        d.insert("source_page".into(), json!(url));
        for key in ["resource_type", "reason", "status_code"] {
            d.insert(
                key.into(),
                resource.get(key).cloned().unwrap_or(Value::Null),
            );
        }
        ctx.add(
            "RENDER_RESOURCE_BLOCKED",
            AddOpts {
                target_url: Some(target.to_owned()),
                details: d,
                ..AddOpts::default()
            },
        )?;
    }
    Ok(partial)
}

/// Emit differences and blocked requests only from retained rendered evidence.
pub fn check_rendered_elements(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let element_ids = ELEMENT_CHECKS;
    let Some(con) = ctx.scan_con.as_ref() else {
        for id in all_checks() {
            ctx.skip(id, "requires retained native rendered evidence");
        }
        return Ok(());
    };
    let rows: Vec<(String, String)> = {
        let mut stmt = con
            .prepare(
                "SELECT u.url,d.renderer_json FROM pages p JOIN urls u ON u.url_id=p.url_id \
                 JOIN documents d ON d.document_id=p.document_id \
                 WHERE p.representation='rendered' AND d.renderer_json IS NOT NULL ORDER BY u.url",
            )
            .map_err(|e| ContextError::Value(e.to_string()))?;
        let mapped = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| ContextError::Value(e.to_string()))?;
        mapped
            .collect::<Result<_, _>>()
            .map_err(|e| ContextError::Value(e.to_string()))?
    };
    let (mut el_ok, mut el_missing, mut el_partial) = (0, 0, false);
    let (mut net_ok, mut net_missing, mut net_partial) = (0, 0, false);
    for (url, raw_json) in rows {
        let renderer: Option<Value> = serde_json::from_str(&raw_json)
            .ok()
            .filter(Value::is_object);
        let elements = renderer
            .as_ref()
            .and_then(|r| r.get("raw_rendered_evidence"))
            .cloned();
        let network = renderer
            .as_ref()
            .and_then(|r| r.get("network_evidence"))
            .cloned();
        match elements.filter(|e| e.get("state").and_then(Value::as_str) == Some("complete")) {
            Some(e) => {
                el_ok += 1;
                el_partial |= element_findings(ctx, &url, &e)?;
            }
            None => el_missing += 1,
        }
        match network.filter(|n| {
            matches!(
                n.get("state").and_then(Value::as_str),
                Some("complete" | "partial")
            )
        }) {
            Some(n) => {
                net_ok += 1;
                net_partial |= network_findings(ctx, &url, &n)?;
            }
            None => net_missing += 1,
        }
    }
    if el_ok == 0 {
        let reason = if el_missing == 0 {
            "no retained raw-versus-rendered element comparisons were available"
        } else {
            "retained raw-versus-rendered element comparisons were unavailable"
        };
        for id in &element_ids {
            ctx.skip(id, reason);
        }
    } else if el_missing > 0 || el_partial {
        for id in &element_ids {
            ctx.skip(
                id,
                "one or more raw-versus-rendered comparisons were partial or unavailable",
            );
        }
    }
    if net_ok == 0 {
        ctx.skip(
            "RENDER_RESOURCE_BLOCKED",
            "no retained rendered network log was available",
        );
    } else if net_missing > 0 || net_partial {
        ctx.skip(
            "RENDER_RESOURCE_BLOCKED",
            "one or more rendered network logs were partial or unavailable",
        );
    }
    Ok(())
}
