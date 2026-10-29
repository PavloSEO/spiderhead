//! Mobile layout findings from explicitly captured render evidence
//! (reference the reference suite and the reference suite validation).

use serde_json::{json, Value};

use super::details;
use super::render_checks::rendered_documents;
use crate::audit::AuditContext;

const CHECKS: [&str; 3] = [
    "MOBILE_CONTENT_OVERFLOW",
    "MOBILE_TEXT_TOO_SMALL",
    "MOBILE_TARGET_TOO_SMALL",
];
const MAX_DETAILS: usize = 20;

/// A JSON integer (not a float, not a bool), like reference `type(v) is int`.
fn int(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64)
}

fn is_str(value: &Value, key: &str) -> bool {
    value.get(key).is_some_and(Value::is_string)
}

fn valid_text_detail(v: &Value) -> bool {
    v.is_object()
        && is_str(v, "tag")
        && is_str(v, "text")
        && v.get("font_size_px")
            .is_some_and(|n| n.is_number() && n.as_f64().is_some_and(f64::is_finite))
        && int(v.get("x")).is_some()
        && int(v.get("y")).is_some()
}

fn valid_target_detail(v: &Value) -> bool {
    v.is_object()
        && is_str(v, "tag")
        && is_str(v, "role")
        && is_str(v, "text")
        && int(v.get("width_px")).is_some_and(|n| n >= 0)
        && int(v.get("height_px")).is_some_and(|n| n >= 0)
        && int(v.get("x")).is_some()
        && int(v.get("y")).is_some()
}

fn valid_overflow_detail(v: &Value) -> bool {
    v.is_object()
        && is_str(v, "tag")
        && is_str(v, "id")
        && is_str(v, "class_name")
        && int(v.get("right_px")).is_some()
        && int(v.get("width_px")).is_some_and(|n| n >= 0)
        && int(v.get("y")).is_some()
}

/// `validate_mobile_layout_evidence`: reject malformed persisted layout records.
pub fn validate_mobile_layout_evidence(value: &Value) -> bool {
    if value.get("schema").and_then(Value::as_str) != Some("mobile_layout.v1")
        || !matches!(
            value.get("state").and_then(Value::as_str),
            Some("complete" | "partial")
        )
        || !is_str(value, "reason")
    {
        return false;
    }
    let mut nums = [0i64; 5];
    for (slot, name) in nums.iter_mut().zip([
        "viewport_width_px",
        "document_width_px",
        "content_overflow_px",
        "small_text_count_lower_bound",
        "small_target_count_lower_bound",
    ]) {
        match int(value.get(name)) {
            Some(n) if n >= 0 => *slot = n,
            _ => return false,
        }
    }
    let [viewport, document, overflow_px, text_count, target_count] = nums;
    if viewport == 0 || overflow_px != (document - viewport).max(0) {
        return false;
    }
    match value.get("content_overflow").and_then(Value::as_bool) {
        Some(flag) if flag == (overflow_px > 0) => {}
        _ => return false,
    }
    let Some(thresholds) = value.get("thresholds").filter(|t| t.is_object()) else {
        return false;
    };
    if ["small_text_below_px", "small_target_below_px"]
        .iter()
        .any(|k| !int(thresholds.get(*k)).is_some_and(|n| n > 0))
    {
        return false;
    }
    let Some(info) = value.get("details").filter(|d| d.is_object()) else {
        return false;
    };
    let list = |name: &str| -> Option<&Vec<Value>> {
        info.get(name)
            .and_then(Value::as_array)
            .filter(|l| l.len() <= MAX_DETAILS)
    };
    let (Some(over), Some(text), Some(target)) = (
        list("overflow_elements"),
        list("small_text_elements"),
        list("small_target_elements"),
    ) else {
        return false;
    };
    text_count >= text.len() as i64
        && target_count >= target.len() as i64
        && over.iter().all(valid_overflow_detail)
        && text.iter().all(valid_text_detail)
        && target.iter().all(valid_target_detail)
}

/// Emit mobile findings only from retained, measured rendered-DOM geometry.
pub fn check_mobile_render_layout(ctx: &mut AuditContext) {
    let Some(con) = ctx.scan.take() else {
        for code in CHECKS {
            ctx.skip(
                code,
                "requires retained native rendered mobile layout evidence",
            );
        }
        return;
    };
    let (mut measured, mut partial, mut unavailable) = (0, 0, 0);
    for (url, raw) in rendered_documents(&con) {
        let evidence = serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|r| r.get("mobile_layout").cloned());
        let Some(evidence) = evidence.filter(Value::is_object) else {
            continue;
        };
        if !validate_mobile_layout_evidence(&evidence) {
            unavailable += 1;
            continue;
        }
        measured += 1;
        if evidence.get("state").and_then(Value::as_str) == Some("partial") {
            partial += 1;
        }
        let info = evidence.get("details").cloned().unwrap_or(Value::Null);
        let elements = |key: &str| info.get(key).cloned().unwrap_or_else(|| json!([]));
        let threshold = |key: &str| {
            evidence
                .get("thresholds")
                .and_then(|t| t.get(key))
                .cloned()
                .unwrap_or(Value::Null)
        };
        if evidence.get("content_overflow") == Some(&Value::Bool(true)) {
            ctx.add(
                "MOBILE_CONTENT_OVERFLOW",
                Some(&url),
                details([
                    ("overflow_px", evidence["content_overflow_px"].clone()),
                    ("viewport_width_px", evidence["viewport_width_px"].clone()),
                    ("document_width_px", evidence["document_width_px"].clone()),
                    ("elements", elements("overflow_elements")),
                ]),
            );
        }
        if int(evidence.get("small_text_count_lower_bound")).unwrap_or(0) > 0 {
            ctx.add(
                "MOBILE_TEXT_TOO_SMALL",
                Some(&url),
                details([
                    (
                        "count_lower_bound",
                        evidence["small_text_count_lower_bound"].clone(),
                    ),
                    ("threshold_px", threshold("small_text_below_px")),
                    ("elements", elements("small_text_elements")),
                ]),
            );
        }
        if int(evidence.get("small_target_count_lower_bound")).unwrap_or(0) > 0 {
            ctx.add(
                "MOBILE_TARGET_TOO_SMALL",
                Some(&url),
                details([
                    (
                        "count_lower_bound",
                        evidence["small_target_count_lower_bound"].clone(),
                    ),
                    ("threshold_px", threshold("small_target_below_px")),
                    ("elements", elements("small_target_elements")),
                ]),
            );
        }
    }
    let reason = if measured == 0 && unavailable == 0 {
        "mobile layout was not requested for any rendered page"
    } else if measured == 0 {
        "rendered mobile layout evidence was unavailable"
    } else if partial > 0 || unavailable > 0 {
        "one or more mobile measurements were partial or unavailable"
    } else {
        ""
    };
    if !reason.is_empty() {
        for code in CHECKS {
            ctx.skip(code, reason);
        }
    }
    ctx.scan = Some(con);
}
