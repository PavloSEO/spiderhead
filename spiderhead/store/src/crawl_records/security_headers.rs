//! Missing security headers on HTML pages, read from a stored crawl's own response evidence (#1013).

use crate::error::StoreError;
use serde_json::{Map, Value};

pub const REDIRECT_HOP_PROBE_VERSION: &str = "redirect_hop_probe.v1";

/// `context_items.kind` under which the opt-in host-variant HSTS probe is retained.
pub const REDIRECT_HOP_PROBE_KIND: &str = "redirect_hop_probe";

fn non_empty_str(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::String(s)) if !s.is_empty())
}

fn keys_are(m: &Map<String, Value>, keys: &[&str]) -> bool {
    m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k))
}

fn hop_ok(hop: &Value) -> bool {
    let Value::Object(h) = hop else { return false };
    keys_are(h, &["url", "status", "location", "hsts"])
        && non_empty_str(h.get("url"))
        && h["status"]
            .as_i64()
            .is_some_and(|s| [301, 302, 303, 307, 308].contains(&s))
        && non_empty_str(h.get("location"))
        && h["hsts"].is_string()
}

fn variant_ok(variant: &Value) -> bool {
    let Value::Object(v) = variant else {
        return false;
    };
    keys_are(
        v,
        &[
            "variant",
            "url",
            "reachable",
            "error",
            "hops",
            "status",
            "final_url",
        ],
    ) && v["variant"].is_string()
        && non_empty_str(v.get("url"))
        && v["reachable"].is_boolean()
        && v["error"].is_string()
        && v["hops"]
            .as_array()
            .is_some_and(|hops| hops.iter().all(hop_ok))
        && (v["status"].is_null() || v["status"].as_i64().is_some())
        && (v["final_url"].is_null() || v["final_url"].is_string())
}

/// Outcome of re-judging a retained redirect-hop probe (`evaluate_redirect_hops`).
pub struct RedirectHopVerdict {
    /// `"measured"`, or `"no_redirect_hops"` when no variant answered with a redirect.
    pub state: &'static str,
    /// Redirect hops observed across all probed host variants.
    pub hops_measured: i64,
    /// Hops whose `hsts` value is absent or trims to empty; each entry is the
    /// hop fields merged under its variant's `variant` label.
    pub missing: Vec<Value>,
}

/// Judge probe observations with a pure function that requires no network: a
/// redirect response without a non-empty `hsts` header is the defect, while a
/// final 2xx without it is MISSING_HSTS's business.
pub fn evaluate_redirect_hops(probe: &Value) -> RedirectHopVerdict {
    let mut missing = Vec::new();
    let mut hops_measured: i64 = 0;
    let empty = Vec::new();
    for variant in probe
        .get("variants")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
    {
        for hop in variant
            .get("hops")
            .and_then(Value::as_array)
            .unwrap_or(&empty)
        {
            hops_measured += 1;
            // Any non-string value (null, numbers) counts as non-empty text; only an absent or blank string fails.
            let fails = match hop.get("hsts") {
                Some(Value::String(s)) => s.trim().is_empty(),
                None => true,
                Some(_) => false,
            };
            if fails {
                let mut m = Map::new();
                m.insert(
                    "variant".into(),
                    variant.get("variant").cloned().unwrap_or(Value::Null),
                );
                if let Value::Object(h) = hop {
                    for (k, v) in h {
                        m.insert(k.clone(), v.clone());
                    }
                }
                missing.push(Value::Object(m));
            }
        }
    }
    RedirectHopVerdict {
        state: if hops_measured == 0 {
            "no_redirect_hops"
        } else {
            "measured"
        },
        hops_measured,
        missing,
    }
}

/// Reject a retained probe row the audit could not have written (#1414).
pub fn validate_redirect_hop_probe(
    item: &Map<String, Value>,
    payload: &Value,
) -> Result<(), StoreError> {
    let Value::Object(p) = payload else {
        return Err(StoreError::Invalid(
            "native redirect-hop probe context is invalid".into(),
        ));
    };
    let ok = keys_are(p, &["schema_version", "url", "variants"])
        && p["schema_version"].as_str() == Some(REDIRECT_HOP_PROBE_VERSION)
        && non_empty_str(p.get("url"))
        && p["variants"]
            .as_array()
            .is_some_and(|v| !v.is_empty() && v.iter().all(variant_ok))
        && item.get("item_key").and_then(Value::as_str) == Some("run")
        && item.get("completeness").and_then(Value::as_str) == Some("complete")
        && matches!(item.get("reason"), Some(Value::String(r)) if r.is_empty());
    if ok {
        Ok(())
    } else {
        Err(StoreError::Invalid(
            "native redirect-hop probe context is invalid".into(),
        ))
    }
}
