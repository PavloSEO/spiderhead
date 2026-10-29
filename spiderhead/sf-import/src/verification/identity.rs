//! Source and observation identity: content digests, policy fingerprints, scan
//! UUIDs, and the offline gap that rejects stale or same-scan "after" evidence.

use super::iso::generated_at;
use super::*;
use sha2::{Digest, Sha256};
use spiderhead_core::common::{canonical_json, python_json};

pub fn digest(value: &Value) -> String {
    to_hex(&Sha256::digest(canonical_json(value).as_bytes()))
}

/// The stored results-affecting crawl-policy identity, if complete.
/// Stored results-affecting crawl-policy identity, when the run retains one.
pub fn results_policy_fingerprint(document: &Object) -> Option<String> {
    let config = get_mapping(document, "run").and_then(|run| get_mapping(run, "crawl_config"))?;
    let raw = python_json(&Value::Object(config.clone()));
    Some(to_hex(&Sha256::digest(raw.as_bytes()))[..16].to_owned())
}

/// The retained scan identity, when an audit actually records one.
/// The retained scan identity, when an audit actually records one.
pub fn scan_identity(document: &Object) -> Option<String> {
    let contract =
        get_mapping(document, "summary").and_then(|s| get_mapping(s, "evidence_contract"));
    let run = get_mapping(document, "run");
    let value = contract
        .and_then(|c| c.get("scan_uuid"))
        .filter(|v| truthy(v))
        .or_else(|| run.and_then(|r| r.get("scan_uuid")));
    match value {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Microseconds since the epoch for an aware `datetime`, or `None`.
///
/// Parses the `datetime.fromisoformat` grammar the caller reaches after rewriting `Z` as
/// `+00:00`: `YYYY-MM-DD[*HH:MM[:SS[.digits]]][+HH:MM[:SS[.digits]]]`, where `*` is any
/// Refuse an offline "fix" without a distinct, later observation identity.
pub fn offline_observation_gap(before: &Object, after: &Object) -> Option<&'static str> {
    let (first, second) = (scan_identity(before), scan_identity(after));
    if first.is_none() || second.is_none() {
        return Some(
            "baseline or after audit lacks a recorded scan UUID; a fresh observation is unproven",
        );
    }
    if first == second {
        return Some(
            "after audit has the same scan UUID as baseline; no new observation is proven",
        );
    }
    match (generated_at(before), generated_at(after)) {
        (None, _) | (_, None) => {
            Some("baseline or after audit lacks a timezone-aware generated_at timestamp")
        }
        (Some(b), Some(a)) if a <= b => {
            Some("after audit generated_at is not later than the baseline observation")
        }
        _ => None,
    }
}

/// `source_identity` for either source kind.
pub fn source_identity(source: &AuditSource<'_>) -> Result<Object, VerifyError> {
    match *source {
        AuditSource::Reader(reader) => {
            let Some(header) = reader.header.as_object() else {
                return fail("audit.v2 source has no validated header");
            };
            let sha256 = reader.sha256.as_str();
            let valid = sha256.len() == 64
                && sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if !valid {
                return fail("audit.v2 source has no valid content hash");
            }
            let binding_uuid = reader
                .binding
                .as_object()
                .and_then(|b| b.get("scan_uuid"))
                .cloned()
                .unwrap_or(Value::Null);
            let run = get_mapping(header, "run");
            let summary = get_mapping(header, "summary");
            let contract = summary.and_then(|s| get_mapping(s, "evidence_contract"));
            if truthy(&binding_uuid) {
                let mismatch = [run, contract]
                    .into_iter()
                    .flatten()
                    .filter_map(|m| m.get("scan_uuid"))
                    .any(|v| truthy(v) && !py_eq(v, &binding_uuid));
                if mismatch {
                    return fail(
                        "audit.v2 header scan identity differs from its validated binding",
                    );
                }
            }
            let mut out = Object::new();
            out.insert("audit_sha256".into(), Value::from(sha256));
            out.insert(
                "scan_uuid".into(),
                binding_uuid
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| scan_identity(header))
                    .map_or(Value::Null, Value::from),
            );
            out.insert(
                "generated_at".into(),
                run.and_then(|r| r.get("generated_at"))
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            out.insert(
                "results_policy_fingerprint".into(),
                results_policy_fingerprint(header).map_or(Value::Null, Value::from),
            );
            Ok(out)
        }
        AuditSource::Document(document) => {
            let run = get_mapping(document, "run");
            let mut out = Object::new();
            out.insert(
                "audit_sha256".into(),
                Value::from(digest(&Value::Object((*document).clone()))),
            );
            out.insert(
                "scan_uuid".into(),
                scan_identity(document).map_or(Value::Null, Value::from),
            );
            out.insert(
                "generated_at".into(),
                run.and_then(|r| r.get("generated_at"))
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            out.insert(
                "results_policy_fingerprint".into(),
                results_policy_fingerprint(document).map_or(Value::Null, Value::from),
            );
            Ok(out)
        }
    }
}
