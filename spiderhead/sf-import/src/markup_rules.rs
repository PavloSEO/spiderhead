//! Findings from versioned document markup, not inferred export columns.
//!
//! per URL from the retained native scan; unmeasured documents stay
//! unavailable.

use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use spiderhead_parse::markup::{A11Y_CODES, AMP_CODES};
use std::collections::HashSet;

use crate::context::disk::DiskError;
use crate::context::{AddOpts, AuditContext, ContextError};
use crate::normalize::{norm_url, Object};
use crate::pyvalue::{str_or_dumps, truthy};

/// The structured-data check ids this evidence can fire.
pub const SCHEMA_CODES: [&str; 3] = [
    "STRUCTURED_DATA_SCHEMA_ERROR",
    "STRUCTURED_DATA_SCHEMA_WARNING",
    "STRUCTURED_DATA_RICH_RESULT_REQUIRED_MISSING",
];

/// AMP runtime/boilerplate codes scoped to an amphtml pairing (#1576): a
/// desktop page declares an AMP target with `<link rel="amphtml">`, and the
/// target's own retained markup is what these read. A page that declares html
/// amp for its own sake, with no pairing behind it, is judged by the
/// `AMP_CODES` source-structural checks and never by these.
pub const PAIRING_AMP_CODES: [&str; 2] = ["AMP_SCRIPT_MISSING", "AMP_BOILERPLATE_INVALID"];

/// The same structural fact, renamed once it sits inside a pairing: the
/// report says which half of the pairing is broken, not that an AMP document
/// exists somewhere with a defect.
fn pairing_source_code(code: &str) -> &str {
    match code {
        "AMP_INVALID_RUNTIME" => "AMP_SCRIPT_MISSING",
        "AMP_INVALID_BOILERPLATE" => "AMP_BOILERPLATE_INVALID",
        other => other,
    }
}

fn all_codes() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = AMP_CODES.to_vec();
    v.push("AMP_MISSING_HTML_ATTRIBUTE");
    v.extend(A11Y_CODES);
    v.extend(SCHEMA_CODES);
    v
}

/// `(*AMP_CODES, "AMP_MISSING_HTML_ATTRIBUTE")`.
fn amp_scope() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = AMP_CODES.to_vec();
    v.push("AMP_MISSING_HTML_ATTRIBUTE");
    v
}

/// The `missing context_items row` probe shared by both evidence kinds.
const MISSING_ROW_SQL: &str = "SELECT 1 FROM pages p LEFT JOIN documents d ON d.document_id=p.document_id \
     LEFT JOIN context_items c ON c.kind=? AND c.item_key='page:'||p.url_id||':document:'||d.document_id||':representation:'||d.representation \
     WHERE p.status_code BETWEEN 200 AND 299 AND lower(p.content_type) LIKE '%html%' \
     AND c.item_key IS NULL LIMIT 1";

/// Static source markup owns AMP syntax; a serialized rendered DOM may repair
/// missing head/body tags and drop the original doctype.
const MARKUP_ROWS_SQL: &str = "SELECT u.url,c.payload_json FROM pages p JOIN urls u USING(url_id) \
     LEFT JOIN documents d ON d.document_id=(SELECT MAX(newer.document_id) \
     FROM documents newer WHERE newer.url_id=p.url_id AND newer.representation='static') \
     LEFT JOIN context_items c ON c.kind='markup_evidence' \
     AND c.item_key='page:'||d.url_id||':document:'||d.document_id||':representation:'||d.representation \
     WHERE p.status_code BETWEEN 200 AND 299 AND lower(p.content_type) LIKE '%html%'";

const DETAIL_ROWS_SQL: &str = "SELECT u.url,c.kind,c.payload_json FROM pages p JOIN urls u USING(url_id) \
     JOIN documents d ON d.document_id=p.document_id \
     JOIN context_items c ON c.item_key='page:'||d.url_id||':document:'||d.document_id||':representation:'||d.representation \
     WHERE c.kind IN ('markup_evidence','structured_evidence') \
     AND p.status_code BETWEEN 200 AND 299 AND lower(p.content_type) LIKE '%html%'";

/// Every row the check reads, collected while `scan_con` is borrowed —
/// `ctx.add` needs `&mut ctx`, so queries cannot interleave with findings.
struct Rows {
    /// An eligible page lacks a `markup_evidence` row.
    missing_markup: bool,
    /// An eligible page lacks a `structured_evidence` row.
    missing_structured: bool,
    /// `(url, payload_json)` from `MARKUP_ROWS_SQL`.
    markup: Vec<(String, Option<String>)>,
    /// `(url, kind, payload_json)` from `DETAIL_ROWS_SQL`.
    detail: Vec<(String, String, Option<String>)>,
}

fn gather(ctx: &AuditContext) -> Result<Option<Rows>, ContextError> {
    let Some(con) = ctx.scan_con.as_ref() else {
        return Ok(None);
    };
    let mut rows = Rows {
        missing_markup: false,
        missing_structured: false,
        markup: Vec::new(),
        detail: Vec::new(),
    };
    for (kind, target) in [
        ("markup_evidence", &mut rows.missing_markup),
        ("structured_evidence", &mut rows.missing_structured),
    ] {
        *target = con
            .query_row(MISSING_ROW_SQL, params![kind], |_| Ok(()))
            .optional()
            .map_err(DiskError::from)?
            .is_some();
    }
    rows.markup = {
        let mut stmt = con.prepare(MARKUP_ROWS_SQL).map_err(DiskError::from)?;
        let collected = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .map_err(DiskError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(DiskError::from)?;
        collected
    };
    rows.detail = {
        let mut stmt = con.prepare(DETAIL_ROWS_SQL).map_err(DiskError::from)?;
        let collected = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(DiskError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(DiskError::from)?;
        collected
    };
    Ok(Some(rows))
}

/// `check_document_markup`: read one active document per URL; unmeasured
/// documents stay unavailable.
pub fn check_document_markup(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let all = all_codes();
    if ctx.scan_con.is_none() {
        for code in all.iter().copied().chain(PAIRING_AMP_CODES) {
            ctx.skip(
                code,
                "document-bound markup evidence requires a retained native crawl",
            );
        }
        return Ok(());
    }
    let mut targets: HashSet<String> = HashSet::new();
    for handle in ctx.html_pages() {
        let amphtml = handle.map(|p| {
            p.metrics
                .get("_record")
                .and_then(Value::as_object)
                .and_then(|r| r.get("amphtml"))
                .cloned()
        });
        if let Some(v) = amphtml.filter(truthy) {
            targets.insert(norm_url(Some(&str_or_dumps(&v)))?);
        }
    }
    let Some(Rows {
        missing_markup,
        missing_structured,
        markup,
        detail,
    }) = gather(ctx)?
    else {
        return Ok(());
    };
    // The pairing codes share one fate, so their verdicts are settled here
    // and filled in by the loop below rather than by the generic tail skip.
    let mut pairing_measured = false;
    let mut pairing_blocked: Option<&'static str> = None;
    let mut measured: HashSet<&'static str> = HashSet::new();
    let mut unavailable: HashSet<&'static str> = HashSet::new();
    // Older captures may have no context row for an otherwise eligible page.
    // A measured sibling is not proof that those pages were checked too.
    if missing_markup {
        unavailable.extend(A11Y_CODES);
    }
    if missing_structured {
        unavailable.extend(SCHEMA_CODES);
    }
    for (url, payload_json) in markup {
        let targeted = targets.contains(&norm_url(Some(&url))?);
        let Some(raw) = payload_json else {
            unavailable.extend(amp_scope());
            if targeted {
                pairing_blocked =
                    Some("no retained markup evidence for the AMP page the pairing declares");
            }
            continue;
        };
        let payload: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        let facts = payload.get("facts").cloned();
        if payload.get("state").and_then(Value::as_str) == Some("unavailable")
            || !facts.as_ref().is_some_and(Value::is_object)
        {
            unavailable.extend(amp_scope());
            if targeted {
                pairing_blocked =
                    Some("no retained markup evidence for the AMP page the pairing declares");
            }
            continue;
        }
        let facts = facts.unwrap_or_default();
        let amp = facts.get("amp").cloned().unwrap_or(Value::Null);
        let declared = amp
            .get("declared")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !declared && !targeted {
            continue;
        }
        measured.insert("AMP_MISSING_HTML_ATTRIBUTE");
        let mut provenance = Object::new();
        provenance.insert(
            "document_id".into(),
            payload
                .get("source_document_id")
                .cloned()
                .unwrap_or(Value::Null),
        );
        provenance.insert("representation".into(), Value::from("static"));
        provenance.insert("method".into(), Value::from("static_html_structure"));
        provenance.insert(
            "full_validator_state".into(),
            amp.get("validator_state").cloned().unwrap_or(Value::Null),
        );
        provenance.insert(
            "full_validator_reason".into(),
            amp.get("validator_reason").cloned().unwrap_or(Value::Null),
        );
        if !declared {
            ctx.add(
                "AMP_MISSING_HTML_ATTRIBUTE",
                AddOpts {
                    target_url: Some(url),
                    details: provenance,
                    ..AddOpts::default()
                },
            )?;
            unavailable.extend(AMP_CODES);
            // A target that does not carry the html amp attribute is not
            // measured for AMP structure: the attribute finding above is the
            // verdict, and the runtime/boilerplate codes stay unmeasured
            // rather than clean.
            pairing_blocked = Some(
                "the AMP page declared by rel=amphtml does not carry the html amp \
                 attribute, so its AMP structure was not measured",
            );
            continue;
        }
        measured.extend(AMP_CODES);
        if targeted {
            pairing_measured = true;
        }
        if let Some(errors) = amp.get("errors").and_then(Value::as_array) {
            for error in errors {
                let code = error
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let code = if targeted {
                    pairing_source_code(code).to_owned()
                } else {
                    code.to_owned()
                };
                let mut details = provenance.clone();
                if let Value::Object(e) = error {
                    for (k, v) in e {
                        details.insert(k.clone(), v.clone());
                    }
                }
                ctx.add(
                    &code,
                    AddOpts {
                        target_url: Some(url.clone()),
                        details,
                        ..AddOpts::default()
                    },
                )?;
            }
        }
    }

    if !pairing_measured {
        let pairing_reason = if targets.is_empty() {
            "no page declares an AMP target (rel=amphtml)"
        } else {
            pairing_blocked
                .unwrap_or("the AMP page declared by rel=amphtml was not captured by the crawl")
        };
        for code in PAIRING_AMP_CODES {
            ctx.skip(code, pairing_reason);
        }
    }

    for (url, kind, raw) in detail {
        let payload: Value =
            serde_json::from_str(raw.as_deref().unwrap_or("null")).unwrap_or(Value::Null);
        let mut provenance = Object::new();
        provenance.insert(
            "document_id".into(),
            payload
                .get("source_document_id")
                .cloned()
                .unwrap_or(Value::Null),
        );
        provenance.insert(
            "representation".into(),
            payload
                .get("representation")
                .cloned()
                .unwrap_or(Value::Null),
        );
        if kind == "markup_evidence" {
            if payload.get("state").and_then(Value::as_str) == Some("unavailable") {
                unavailable.extend(A11Y_CODES);
                continue;
            }
            measured.extend(A11Y_CODES);
            let issues = payload
                .get("facts")
                .and_then(|f| f.get("accessibility"))
                .and_then(|a| a.get("issues"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for issue in issues {
                let code = issue
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let mut details = provenance.clone();
                details.insert("method".into(), Value::from("static_html"));
                if let Value::Object(i) = &issue {
                    for (k, v) in i {
                        details.insert(k.clone(), v.clone());
                    }
                }
                ctx.add(
                    &code,
                    AddOpts {
                        target_url: Some(url.clone()),
                        occurrences_count: issue.get("count").and_then(Value::as_i64),
                        details,
                        ..AddOpts::default()
                    },
                )?;
            }
        } else {
            let validation = payload.get("validation").cloned();
            let Some(validation) = validation.filter(|v| v.is_object()) else {
                unavailable.extend(SCHEMA_CODES);
                continue;
            };
            let state = payload.get("state").and_then(Value::as_str);
            let incomplete = matches!(state, Some("unavailable") | Some("unsupported_context"));
            if incomplete {
                unavailable.extend(SCHEMA_CODES);
            }
            measured.extend(&SCHEMA_CODES[..2]);
            for (field, code) in [
                ("entity_errors", SCHEMA_CODES[0]),
                ("entity_warnings", SCHEMA_CODES[1]),
            ] {
                let mut count = validation.get(field).and_then(Value::as_i64).unwrap_or(0);
                // Scalar/empty JSON parses syntactically but defines no graph
                // entities. Its explicit structural failure is still a
                // finding.
                if field == "entity_errors" && count == 0 && state == Some("structurally_invalid") {
                    count = 1;
                }
                if count != 0 {
                    let mut details = provenance.clone();
                    details.insert("validation".into(), validation.clone());
                    ctx.add(
                        code,
                        AddOpts {
                            target_url: Some(url.clone()),
                            occurrences_count: Some(count),
                            details,
                            ..AddOpts::default()
                        },
                    )?;
                }
            }
            let features = validation
                .get("feature_checks")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if incomplete {
                // Required fields may be in unsupported RDFa graph carriers.
                continue;
            }
            if features.is_empty() {
                unavailable.insert(SCHEMA_CODES[2]);
            } else {
                measured.insert(SCHEMA_CODES[2]);
            }
            let missing: Vec<Value> = features
                .iter()
                .filter(|f| {
                    f.get("local_requirements_state").and_then(Value::as_str)
                        == Some("missing_required_properties")
                })
                .cloned()
                .collect();
            if !missing.is_empty() {
                let mut details = provenance.clone();
                details.insert("features".into(), Value::Array(missing));
                details.insert(
                    "scope".into(),
                    Value::from("bundled local requirements; Google display is not guaranteed"),
                );
                ctx.add(
                    SCHEMA_CODES[2],
                    AddOpts {
                        target_url: Some(url.clone()),
                        details,
                        ..AddOpts::default()
                    },
                )?;
            }
        }
    }
    for code in all {
        if unavailable.contains(code) || !measured.contains(code) {
            ctx.skip(
                code,
                if unavailable.contains(code) {
                    "one or more native documents lack measured markup evidence"
                } else {
                    "no applicable document-bound native markup evidence"
                },
            );
        }
    }
    Ok(())
}
