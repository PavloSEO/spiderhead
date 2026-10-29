//! Findings from versioned document markup evidence (reference the reference suite).

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use crate::audit::{norm_url, AuditContext};

const AMP_CODES: [&str; 7] = [
    "AMP_INVALID_DOCTYPE",
    "AMP_MISSING_HEAD",
    "AMP_MISSING_BODY",
    "AMP_INVALID_CHARSET",
    "AMP_INVALID_VIEWPORT",
    "AMP_INVALID_RUNTIME",
    "AMP_INVALID_BOILERPLATE",
];
const A11Y_CODES: [&str; 5] = [
    "A11Y_HTML_LANG_MISSING",
    "A11Y_FORM_LABEL_MISSING",
    "A11Y_IMAGE_ALT_MISSING",
    "A11Y_HEADING_ORDER",
    "A11Y_MAIN_LANDMARK_MISSING",
];
const SCHEMA_CODES: [&str; 3] = [
    "STRUCTURED_DATA_SCHEMA_ERROR",
    "STRUCTURED_DATA_SCHEMA_WARNING",
    "STRUCTURED_DATA_RICH_RESULT_REQUIRED_MISSING",
];
const PAIRING: [&str; 2] = ["AMP_SCRIPT_MISSING", "AMP_BOILERPLATE_INVALID"];

fn merged(parts: &[&Map<String, Value>]) -> Map<String, Value> {
    let mut out = Map::new();
    for p in parts {
        for (k, v) in *p {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// `check_document_markup`: AMP structure, accessibility and structured-data validation from stored evidence.
pub fn check_document_markup(ctx: &mut AuditContext) {
    let all: Vec<&str> = AMP_CODES
        .iter()
        .chain(["AMP_MISSING_HTML_ATTRIBUTE"].iter())
        .chain(A11Y_CODES.iter())
        .chain(SCHEMA_CODES.iter())
        .copied()
        .collect();
    let Some(con) = ctx.scan.take() else {
        for code in all.iter().chain(PAIRING.iter()) {
            ctx.skip(
                code,
                "document-bound markup evidence requires a retained native crawl",
            );
        }
        return;
    };
    let targets: BTreeSet<String> = ctx
        .html_pages()
        .into_iter()
        .map(|i| ctx.pages[i].text("amphtml").to_owned())
        .filter(|a| !a.is_empty())
        .map(|a| norm_url(&a))
        .collect();
    let mut measured: BTreeSet<&str> = BTreeSet::new();
    let mut unavailable: BTreeSet<&str> = BTreeSet::new();
    let (mut pairing_measured, mut pairing_blocked): (bool, Option<&str>) = (false, None);
    for (kind, codes) in [
        ("markup_evidence", A11Y_CODES.to_vec()),
        ("structured_evidence", SCHEMA_CODES.to_vec()),
    ] {
        let missing = con
            .query_row(
                "SELECT 1 FROM pages p LEFT JOIN documents d ON d.document_id=p.document_id \
                 LEFT JOIN context_items c ON c.kind=?1 AND c.item_key='page:'||p.url_id||':document:'||d.document_id||':representation:'||d.representation \
                 WHERE p.status_code BETWEEN 200 AND 299 AND lower(p.content_type) LIKE '%html%' AND c.item_key IS NULL LIMIT 1",
                [kind],
                |_| Ok(()),
            )
            .is_ok();
        if missing {
            unavailable.extend(codes);
        }
    }
    let rows = |sql: &str| -> Vec<(String, String, Option<String>)> {
        con.prepare(sql)
            .ok()
            .and_then(|mut s| {
                s.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                })
                .ok()
                .map(|r| r.flatten().collect())
            })
            .unwrap_or_default()
    };
    let amp_rows = rows(
        "SELECT u.url,'markup_evidence',c.payload_json FROM pages p JOIN urls u USING(url_id) \
         LEFT JOIN documents d ON d.document_id=(SELECT MAX(newer.document_id) FROM documents newer WHERE newer.url_id=p.url_id AND newer.representation='static') \
         LEFT JOIN context_items c ON c.kind='markup_evidence' AND c.item_key='page:'||d.url_id||':document:'||d.document_id||':representation:'||d.representation \
         WHERE p.status_code BETWEEN 200 AND 299 AND lower(p.content_type) LIKE '%html%'",
    );
    const NO_AMP: &str = "no retained markup evidence for the AMP page the pairing declares";
    for (url, _, payload) in amp_rows {
        let targeted = targets.contains(&norm_url(&url));
        let payload: Option<Value> = payload.and_then(|t| serde_json::from_str(&t).ok());
        let Some(payload) =
            payload.filter(|p| p["state"] != "unavailable" && p["facts"].is_object())
        else {
            unavailable.extend(AMP_CODES);
            unavailable.insert("AMP_MISSING_HTML_ATTRIBUTE");
            if targeted {
                pairing_blocked = Some(NO_AMP);
            }
            continue;
        };
        let amp = &payload["facts"]["amp"];
        if amp["declared"] != json!(true) && !targeted {
            continue;
        }
        measured.insert("AMP_MISSING_HTML_ATTRIBUTE");
        let prov = details_of([
            ("document_id", payload["source_document_id"].clone()),
            ("representation", json!("static")),
            ("method", json!("static_html_structure")),
            ("full_validator_state", amp["validator_state"].clone()),
            ("full_validator_reason", amp["validator_reason"].clone()),
        ]);
        if amp["declared"] != json!(true) {
            ctx.add("AMP_MISSING_HTML_ATTRIBUTE", Some(&url), prov);
            unavailable.extend(AMP_CODES);
            pairing_blocked = Some("the AMP page declared by rel=amphtml does not carry the html amp attribute, so its AMP structure was not measured");
            continue;
        }
        measured.extend(AMP_CODES);
        pairing_measured = pairing_measured || targeted;
        for error in amp["errors"].as_array().into_iter().flatten() {
            let mut code = error["code"].as_str().unwrap_or("").to_owned();
            if targeted {
                code = match code.as_str() {
                    "AMP_INVALID_RUNTIME" => "AMP_SCRIPT_MISSING".into(),
                    "AMP_INVALID_BOILERPLATE" => "AMP_BOILERPLATE_INVALID".into(),
                    _ => code,
                };
            }
            let err = error.as_object().cloned().unwrap_or_default();
            ctx.add(&code, Some(&url), merged(&[&prov, &err]));
        }
    }
    if !pairing_measured {
        let reason = if targets.is_empty() {
            "no page declares an AMP target (rel=amphtml)"
        } else {
            pairing_blocked
                .unwrap_or("the AMP page declared by rel=amphtml was not captured by the crawl")
        };
        for code in PAIRING {
            ctx.skip(code, reason);
        }
    }
    let evidence = rows(
        "SELECT u.url,c.kind,c.payload_json FROM pages p JOIN urls u USING(url_id) JOIN documents d ON d.document_id=p.document_id \
         JOIN context_items c ON c.item_key='page:'||d.url_id||':document:'||d.document_id||':representation:'||d.representation \
         WHERE c.kind IN ('markup_evidence','structured_evidence') AND p.status_code BETWEEN 200 AND 299 AND lower(p.content_type) LIKE '%html%'",
    );
    for (url, kind, payload) in evidence {
        let Some(payload) = payload.and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
            continue;
        };
        let prov = details_of([
            ("document_id", payload["source_document_id"].clone()),
            ("representation", payload["representation"].clone()),
        ]);
        if kind == "markup_evidence" {
            if payload["state"] == "unavailable" {
                unavailable.extend(A11Y_CODES);
                continue;
            }
            measured.extend(A11Y_CODES);
            for issue in payload["facts"]["accessibility"]["issues"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let extra = issue.as_object().cloned().unwrap_or_default();
                let method = details_of([("method", json!("static_html"))]);
                ctx.add_with(
                    issue["code"].as_str().unwrap_or(""),
                    Some(&url),
                    merged(&[&prov, &method, &extra]),
                    None,
                    issue["count"].as_i64().unwrap_or(1),
                    None,
                );
            }
        } else {
            let validation = &payload["validation"];
            if !validation.is_object() {
                unavailable.extend(SCHEMA_CODES);
                continue;
            }
            let incomplete = matches!(
                payload["state"].as_str(),
                Some("unavailable" | "unsupported_context")
            );
            if incomplete {
                unavailable.extend(SCHEMA_CODES);
            }
            measured.extend(&SCHEMA_CODES[..2]);
            for (field, code) in [
                ("entity_errors", SCHEMA_CODES[0]),
                ("entity_warnings", SCHEMA_CODES[1]),
            ] {
                let mut count = validation[field].as_i64().unwrap_or(0);
                if field == "entity_errors"
                    && count == 0
                    && payload["state"] == "structurally_invalid"
                {
                    count = 1;
                }
                if count != 0 {
                    ctx.add_with(
                        code,
                        Some(&url),
                        merged(&[&prov, &details_of([("validation", validation.clone())])]),
                        None,
                        count,
                        None,
                    );
                }
            }
            if incomplete {
                continue;
            }
            let features = validation["feature_checks"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if features.is_empty() {
                unavailable.insert(SCHEMA_CODES[2]);
            } else {
                measured.insert(SCHEMA_CODES[2]);
            }
            let missing: Vec<Value> = features
                .into_iter()
                .filter(|f| f["local_requirements_state"] == "missing_required_properties")
                .collect();
            if !missing.is_empty() {
                ctx.add(
                    SCHEMA_CODES[2],
                    Some(&url),
                    merged(&[
                        &prov,
                        &details_of([
                            ("features", Value::Array(missing)),
                            (
                                "scope",
                                json!(
                                    "bundled local requirements; Google display is not guaranteed"
                                ),
                            ),
                        ]),
                    ]),
                );
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
    ctx.scan = Some(con);
}

fn details_of<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}
