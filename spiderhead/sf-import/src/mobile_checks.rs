//! Mobile layout findings from explicitly captured native render evidence
//! (#1016).
//!
//! local port of the reference suite::
//! validate_mobile_layout_evidence` until the checks crate lands a shared
//! version — the predicate must stay byte-for-byte aligned with the oracle.

use rusqlite::params;
use serde_json::Value;

use crate::context::{AuditContext, ContextError};
use crate::normalize::Object;

/// The three check ids this evidence can fire.
pub const CHECKS: [&str; 3] = [
    "MOBILE_CONTENT_OVERFLOW",
    "MOBILE_TEXT_TOO_SMALL",
    "MOBILE_TARGET_TOO_SMALL",
];

/// `len(details[...]) > MAX_DETAILS` bound in the validator.
const MAX_DETAILS: usize = 20;

/// `check_mobile_render_layout`: emit mobile findings only from retained,
/// measured rendered-DOM geometry.
pub fn check_mobile_render_layout(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let Some(con) = ctx.scan_con.as_ref() else {
        for check_id in CHECKS {
            ctx.skip(
                check_id,
                "requires retained native rendered mobile layout evidence",
            );
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
            .map_err(crate::context::disk::DiskError::from)?;
        let collected = stmt
            .query_map(params![], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(crate::context::disk::DiskError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(crate::context::disk::DiskError::from)?;
        collected
    };
    let mut measured = 0i64;
    let mut partial = 0i64;
    let mut unavailable = 0i64;
    for (url, raw) in rows {
        let evidence = serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|renderer| renderer.get("mobile_layout").cloned())
            .filter(Value::is_object);
        let Some(evidence) = evidence else {
            continue;
        };
        if !validate_mobile_layout_evidence(&evidence) {
            unavailable += 1;
            continue;
        }
        measured += 1;
        partial += i64::from(evidence.get("state").and_then(Value::as_str) == Some("partial"));
        if evidence.get("content_overflow").and_then(Value::as_bool) == Some(true) {
            let mut details = Object::new();
            details.insert(
                "overflow_px".into(),
                evidence
                    .get("content_overflow_px")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            details.insert(
                "viewport_width_px".into(),
                evidence
                    .get("viewport_width_px")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            details.insert(
                "document_width_px".into(),
                evidence
                    .get("document_width_px")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            details.insert(
                "elements".into(),
                evidence
                    .get("details")
                    .and_then(Value::as_object)
                    .and_then(|d| d.get("overflow_elements"))
                    .cloned()
                    .unwrap_or_else(|| Value::Array(Vec::new())),
            );
            ctx.add(
                "MOBILE_CONTENT_OVERFLOW",
                crate::context::AddOpts {
                    target_url: Some(url.clone()),
                    details,
                    ..crate::context::AddOpts::default()
                },
            )?;
        }
        if evidence
            .get("small_text_count_lower_bound")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            > 0
        {
            let mut details = Object::new();
            details.insert(
                "count_lower_bound".into(),
                evidence["small_text_count_lower_bound"].clone(),
            );
            details.insert(
                "threshold_px".into(),
                evidence
                    .get("thresholds")
                    .and_then(Value::as_object)
                    .and_then(|t| t.get("small_text_below_px"))
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            details.insert(
                "elements".into(),
                evidence
                    .get("details")
                    .and_then(Value::as_object)
                    .and_then(|d| d.get("small_text_elements"))
                    .cloned()
                    .unwrap_or_else(|| Value::Array(Vec::new())),
            );
            ctx.add(
                "MOBILE_TEXT_TOO_SMALL",
                crate::context::AddOpts {
                    target_url: Some(url.clone()),
                    details,
                    ..crate::context::AddOpts::default()
                },
            )?;
        }
        if evidence
            .get("small_target_count_lower_bound")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            > 0
        {
            let mut details = Object::new();
            details.insert(
                "count_lower_bound".into(),
                evidence["small_target_count_lower_bound"].clone(),
            );
            details.insert(
                "threshold_px".into(),
                evidence
                    .get("thresholds")
                    .and_then(Value::as_object)
                    .and_then(|t| t.get("small_target_below_px"))
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            details.insert(
                "elements".into(),
                evidence
                    .get("details")
                    .and_then(Value::as_object)
                    .and_then(|d| d.get("small_target_elements"))
                    .cloned()
                    .unwrap_or_else(|| Value::Array(Vec::new())),
            );
            ctx.add(
                "MOBILE_TARGET_TOO_SMALL",
                crate::context::AddOpts {
                    target_url: Some(url),
                    details,
                    ..crate::context::AddOpts::default()
                },
            )?;
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
        for check_id in CHECKS {
            ctx.skip(check_id, reason);
        }
    }
    Ok(())
}

// -- evidence shape validation ------------------------------------------------

/// reference `type(x) is int` — a JSON integer, never a float or a bool.
fn is_strict_int(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Number(n)) if n.is_i64() || n.is_u64())
}

/// `_valid_number`: `type in {int, float}` and `math.isfinite` — bool excluded.
fn valid_number(v: Option<&Value>) -> bool {
    v.and_then(Value::as_f64).is_some_and(f64::is_finite)
}

/// `_valid_text_detail`.
fn valid_text_detail(v: &Value) -> bool {
    let Some(d) = v.as_object() else {
        return false;
    };
    d.get("tag").is_some_and(Value::is_string)
        && d.get("text").is_some_and(Value::is_string)
        && valid_number(d.get("font_size_px"))
        && is_strict_int(d.get("x"))
        && is_strict_int(d.get("y"))
}

/// `_valid_target_detail`.
fn valid_target_detail(v: &Value) -> bool {
    let Some(d) = v.as_object() else {
        return false;
    };
    d.get("tag").is_some_and(Value::is_string)
        && d.get("role").is_some_and(Value::is_string)
        && d.get("text").is_some_and(Value::is_string)
        && is_strict_int(d.get("width_px"))
        && is_strict_int(d.get("height_px"))
        && is_strict_int(d.get("x"))
        && is_strict_int(d.get("y"))
        && d.get("width_px")
            .and_then(Value::as_i64)
            .is_some_and(|n| n >= 0)
        && d.get("height_px")
            .and_then(Value::as_i64)
            .is_some_and(|n| n >= 0)
}

/// `_valid_overflow_detail`.
fn valid_overflow_detail(v: &Value) -> bool {
    let Some(d) = v.as_object() else {
        return false;
    };
    d.get("tag").is_some_and(Value::is_string)
        && d.get("id").is_some_and(Value::is_string)
        && d.get("class_name").is_some_and(Value::is_string)
        && is_strict_int(d.get("right_px"))
        && is_strict_int(d.get("width_px"))
        && is_strict_int(d.get("y"))
        && d.get("width_px")
            .and_then(Value::as_i64)
            .is_some_and(|n| n >= 0)
}

/// `validate_mobile_layout_evidence`: reject malformed persisted layout
/// records instead of reading them as zero.
fn validate_mobile_layout_evidence(value: &Value) -> bool {
    let Some(v) = value.as_object() else {
        return false;
    };
    if v.get("schema").and_then(Value::as_str) != Some("mobile_layout.v1") {
        return false;
    }
    let state = v.get("state").and_then(Value::as_str);
    if !matches!(state, Some("complete") | Some("partial"))
        || !v.get("reason").is_some_and(Value::is_string)
    {
        return false;
    }
    for name in [
        "viewport_width_px",
        "document_width_px",
        "content_overflow_px",
        "small_text_count_lower_bound",
        "small_target_count_lower_bound",
    ] {
        if !is_strict_int(v.get(name)) || v[name].as_i64().is_some_and(|n| n < 0) {
            return false;
        }
    }
    if v["viewport_width_px"].as_i64() == Some(0) {
        return false;
    }
    let overflow_px = v["content_overflow_px"].as_i64().unwrap_or(0);
    let expected = (v["document_width_px"].as_i64().unwrap_or(0)
        - v["viewport_width_px"].as_i64().unwrap_or(0))
    .max(0);
    if overflow_px != expected
        || !v.get("content_overflow").is_some_and(Value::is_boolean)
        || v["content_overflow"].as_bool() != Some(overflow_px > 0)
    {
        return false;
    }
    let Some(thresholds) = v.get("thresholds").and_then(Value::as_object) else {
        return false;
    };
    for key in ["small_text_below_px", "small_target_below_px"] {
        if !is_strict_int(thresholds.get(key)) || thresholds[key].as_i64().is_some_and(|n| n <= 0) {
            return false;
        }
    }
    let Some(details) = v.get("details").and_then(Value::as_object) else {
        return false;
    };
    for name in [
        "overflow_elements",
        "small_text_elements",
        "small_target_elements",
    ] {
        match details.get(name) {
            Some(Value::Array(items)) if items.len() <= MAX_DETAILS => {}
            _ => return false,
        }
    }
    let small_text_elements = details["small_text_elements"]
        .as_array()
        .map_or(0, Vec::len) as i64;
    if v["small_text_count_lower_bound"].as_i64().unwrap_or(0) < small_text_elements {
        return false;
    }
    let small_target_elements = details["small_target_elements"]
        .as_array()
        .map_or(0, Vec::len) as i64;
    if v["small_target_count_lower_bound"].as_i64().unwrap_or(0) < small_target_elements {
        return false;
    }
    if !details["overflow_elements"]
        .as_array()
        .is_some_and(|items| items.iter().all(valid_overflow_detail))
    {
        return false;
    }
    if !details["small_text_elements"]
        .as_array()
        .is_some_and(|items| items.iter().all(valid_text_detail))
    {
        return false;
    }
    details["small_target_elements"]
        .as_array()
        .is_some_and(|items| items.iter().all(valid_target_detail))
}
