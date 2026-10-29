//! Bounded, offline comparison of retained scan body observations.
//!
//! Both sides are `&Connection`; no reader dispatch is needed.

mod difflib;

use crate::bodies::{decode_entity, read_body, DOCUMENT_CANDIDATE};
use crate::error::StoreError;
use crate::scan_artifact::ScanError;
use rusqlite::types::{ToSqlOutput, Value as Sql, ValueRef};
use rusqlite::{Connection, ToSql};
use serde_json::{json, Map, Value};

/// Largest body the comparison may read (64 MiB).
pub const MAX_BYTES: i64 = 67108864;

/// Largest line count the comparison may split (one million).
pub const MAX_LINES: i64 = 1000000;

/// Sorted for a stable output order.
pub const REPRESENTATIONS: &[&str] = &["legacy_fragment", "rendered", "static"];

type Row = Map<String, Value>;

/// A JSON value bound as an SQL parameter (`null`, integer, real or text).
pub(crate) struct Param<'a>(pub(crate) &'a Value);

impl ToSql for Param<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Owned(match self.0 {
            Value::Null => Sql::Null,
            Value::Bool(b) => Sql::Integer(i64::from(*b)),
            Value::Number(n) => n
                .as_i64()
                .map_or_else(|| Sql::Real(n.as_f64().unwrap_or(0.0)), Sql::Integer),
            Value::String(s) => Sql::Text(s.clone()),
            other => Sql::Text(other.to_string()),
        }))
    }
}

/// Run `sql` and return each row as a column-name keyed map.
/// BLOB cells become `null`: they have no JSON form and no caller selects one.
pub fn rows(con: &Connection, sql: &str, values: &[Value]) -> Result<Vec<Row>, StoreError> {
    let mut stmt = con.prepare(sql)?;
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|n| (*n).to_owned())
        .collect();
    let params: Vec<Param> = values.iter().map(Param).collect();
    let mut cursor = stmt.query(rusqlite::params_from_iter(params))?;
    let mut out = Vec::new();
    while let Some(r) = cursor.next()? {
        let mut row = Map::new();
        for (i, name) in names.iter().enumerate() {
            row.insert(
                name.clone(),
                match r.get_ref(i)? {
                    ValueRef::Null | ValueRef::Blob(_) => Value::Null,
                    ValueRef::Integer(v) => json!(v),
                    ValueRef::Real(v) => json!(v),
                    ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                },
            );
        }
        out.push(row);
    }
    Ok(out)
}

const DOC_JOINS: &str = "LEFT JOIN pages p ON p.url_id=d.url_id \
    LEFT JOIN context_items i ON i.kind='resource_inventory' AND i.item_key='document:'||d.document_id ";

/// Select the retained document of `(url, representation[, variant_key])`, or say why not.
pub fn observation(
    con: &Connection,
    url: &str,
    representation: &str,
    variant_key: Option<&str>,
) -> Result<(Option<Row>, Option<String>), StoreError> {
    let identity = [json!(url), json!(representation)];
    let variants = rows(
        con,
        &format!(
            "SELECT DISTINCT r.variant_key FROM documents d JOIN urls u ON u.url_id=d.url_id \
             LEFT JOIN responses r ON r.response_id=d.source_response_id {DOC_JOINS}\
             WHERE u.url=? AND d.representation=? AND {DOCUMENT_CANDIDATE} LIMIT 2"
        ),
        &identity,
    )?;
    if variants.is_empty() {
        if representation != "static" {
            return Ok((None, Some("body observation is missing".into())));
        }
        return resource_observation(con, url, variant_key);
    }
    let (clause, values) = match variant_key {
        Some(key) => (
            " AND r.variant_key=?",
            vec![identity[0].clone(), identity[1].clone(), json!(key)],
        ),
        None if variants.len() > 1 => {
            return Ok((
                None,
                Some("multiple HTTP variants are available; variant_key is required".into()),
            ))
        }
        None => ("", identity.to_vec()),
    };
    let found = rows(
        con,
        &format!(
            "SELECT d.document_id,d.representation,d.body_sha256,d.fidelity,d.body_state,d.body_reason,\
             d.source_response_id,r.variant_key,r.content_type,d.renderer_json,\
             'page' AS purpose FROM documents d \
             JOIN urls u ON u.url_id=d.url_id LEFT JOIN responses r ON r.response_id=d.source_response_id \
             {DOC_JOINS}WHERE u.url=? AND d.representation=?{clause} AND {DOCUMENT_CANDIDATE} \
             ORDER BY (p.document_id=d.document_id) DESC,(i.item_key IS NOT NULL) DESC,\
             d.document_id DESC LIMIT 1"
        ),
        &values,
    )?;
    Ok(match found.into_iter().next() {
        Some(row) => (Some(row), None),
        None => (None, Some("requested HTTP variant is missing".into())),
    })
}

/// Return a retained static JS/CSS response when no page document exists.
pub fn resource_observation(
    con: &Connection,
    url: &str,
    variant_key: Option<&str>,
) -> Result<(Option<Row>, Option<String>), StoreError> {
    let associated = "EXISTS(SELECT 1 FROM resource_refs rr WHERE rr.response_id=r.response_id) \
        OR NOT EXISTS(SELECT 1 FROM resource_refs rr WHERE rr.resource_url_id=r.request_url_id \
        AND rr.response_id IS NOT NULL)";
    let variants = rows(
        con,
        &format!(
            "SELECT DISTINCT r.variant_key FROM responses r JOIN urls u ON u.url_id=r.request_url_id \
             WHERE u.url=? AND r.purpose IN ('script','stylesheet') AND ({associated}) LIMIT 2"
        ),
        &[json!(url)],
    )?;
    if variants.is_empty() {
        return Ok((None, Some("body observation is missing".into())));
    }
    let (clause, values) = match variant_key {
        Some(key) => (" AND r.variant_key=?", vec![json!(url), json!(key)]),
        None if variants.len() > 1 => {
            return Ok((
                None,
                Some("multiple HTTP variants are available; variant_key is required".into()),
            ))
        }
        None => ("", vec![json!(url)]),
    };
    let found = rows(
        con,
        &format!(
            "SELECT NULL AS document_id,'static' AS representation,r.body_sha256,\
             r.body_fidelity AS fidelity,r.body_state,r.body_reason,r.response_id AS source_response_id,\
             r.variant_key,r.content_type,NULL AS renderer_json,r.purpose FROM responses r \
             JOIN urls u ON u.url_id=r.request_url_id WHERE u.url=? \
             AND r.purpose IN ('script','stylesheet') AND ({associated}){clause} \
             ORDER BY (EXISTS(SELECT 1 FROM resource_refs rr WHERE rr.response_id=r.response_id)) DESC,\
             r.response_id DESC LIMIT 1"
        ),
        &values,
    )?;
    Ok(match found.into_iter().next() {
        Some(row) => (Some(row), None),
        None => (None, Some("requested HTTP variant is missing".into())),
    })
}

/// The public part of an observation row.
pub fn summary(row: Option<&Row>) -> Option<Row> {
    let row = row?;
    let mut out = Map::new();
    for (key, source) in [
        ("document_id", "document_id"),
        ("variant_key", "variant_key"),
        ("fidelity", "fidelity"),
        ("body_state", "body_state"),
        ("body_reason", "body_reason"),
        ("sha256", "body_sha256"),
        ("content_type", "content_type"),
    ] {
        out.insert(key.into(), row.get(source).cloned().unwrap_or(Value::Null));
    }
    Some(out)
}

/// Assemble a result record.
#[allow(clippy::too_many_arguments)]
pub fn result(
    status: &str,
    reason: &str,
    url: &str,
    representation: &str,
    variant_key: Option<&str>,
    left: Option<&Row>,
    right: Option<&Row>,
    left_representation: Option<&str>,
    right_representation: Option<&str>,
) -> Row {
    let mut out = Map::new();
    out.insert("status".into(), json!(status));
    out.insert("reason".into(), json!(reason));
    out.insert("url".into(), json!(url));
    out.insert("representation".into(), json!(representation));
    out.insert(
        "left_representation".into(),
        json!(left_representation.unwrap_or(representation)),
    );
    out.insert(
        "right_representation".into(),
        json!(right_representation.unwrap_or(representation)),
    );
    out.insert("variant_key".into(), json!(variant_key));
    out.insert("left".into(), json!(summary(left)));
    out.insert("right".into(), json!(summary(right)));
    out
}

fn text_of<'a>(row: &'a Row, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

/// Decode a retained body by its recorded fidelity.
pub fn text(raw: &[u8], row: &Row) -> Result<String, StoreError> {
    match text_of(row, "fidelity") {
        Some("entity_bytes") => {
            Ok(decode_entity(raw, text_of(row, "content_type").unwrap_or(""))?.0)
        }
        Some("serialized_dom") => String::from_utf8(raw.to_vec())
            .map_err(|e| StoreError::Invalid(format!("utf-8 codec can't decode body: {e}"))),
        Some("reencoded_text") => Ok(String::from_utf8_lossy(raw).into_owned()),
        _ => Err(StoreError::ScanError(ScanError {
            message: "body diff cannot decode an unavailable fidelity".into(),
        })),
    }
}

/// Whether the observation is text a line diff can read.
pub fn textual(row: &Row) -> bool {
    if matches!(
        text_of(row, "fidelity"),
        Some("serialized_dom" | "reencoded_text")
    ) {
        return true;
    }
    let content_type = text_of(row, "content_type")
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim_matches(crate::bodies::py_isspace)
        .to_lowercase();
    content_type.starts_with("text/")
        || [
            "application/javascript",
            "application/ecmascript",
            "application/json",
            "application/xhtml+xml",
            "application/xml",
        ]
        .contains(&content_type.as_str())
}

/// Accept only the complete, normalized renderer record written by the corpus layer.
pub fn renderer_provenance(row: &Row) -> Result<Option<Row>, StoreError> {
    let Some(raw) = text_of(row, "renderer_json") else {
        return Ok(None);
    };
    let Ok(Value::Object(value)) = serde_json::from_str::<Value>(raw) else {
        return Ok(None);
    };
    let nonempty = |k: &str| {
        value
            .get(k)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
    };
    let int = |k: &str| value.get(k).is_some_and(|v| v.is_i64() || v.is_u64());
    let settings = value.get("settings").and_then(Value::as_object);
    let ok = nonempty("engine")
        && value.get("engine_version").is_some_and(Value::is_string)
        && nonempty("engine_version")
        && settings.is_some_and(|s| {
            s.get("navigation").is_some_and(Value::is_object)
                && s.get("transforms").is_some_and(Value::is_object)
        })
        && value.get("navigation_transform") == Some(&json!("direct"))
        && int("navigation_url_id")
        && int("final_url_id");
    if !ok {
        return Ok(None);
    }
    let mut kept = settings.cloned().unwrap_or_default();
    // Observed route events vary per capture; they are not renderer policy.
    kept.shift_remove("navigation_evidence");
    let mut out = Map::new();
    out.insert("engine".into(), value["engine"].clone());
    out.insert("engine_version".into(), value["engine_version"].clone());
    out.insert("settings".into(), Value::Object(kept));
    out.insert(
        "navigation_transform".into(),
        value["navigation_transform"].clone(),
    );
    Ok(Some(out))
}

/// Validate the explicit byte and line ceilings.
pub fn limits(max_bytes: i64, max_lines: i64) -> Result<(), StoreError> {
    if !(1..=MAX_BYTES).contains(&max_bytes) {
        return Err(StoreError::Invalid(
            "max_bytes must be a positive finite body-diff limit".into(),
        ));
    }
    if !(1..=MAX_LINES).contains(&max_lines) {
        return Err(StoreError::Invalid(
            "max_lines must be a positive finite body-diff line limit".into(),
        ));
    }
    Ok(())
}

/// Split text into lines without keeping line endings.
pub(crate) fn splitlines(text: &str) -> Vec<&str> {
    const ENDS: [char; 10] = [
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ];
    let mut out = Vec::new();
    let (mut start, mut chars) = (0, text.char_indices().peekable());
    while let Some((i, c)) = chars.next() {
        if !ENDS.contains(&c) {
            continue;
        }
        out.push(&text[start..i]);
        start = i + c.len_utf8();
        if c == '\r' && chars.peek().is_some_and(|(_, n)| *n == '\n') {
            chars.next();
            start += 1;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

fn decoded_size(con: &Connection, sha: &Value) -> Result<Option<i64>, StoreError> {
    Ok(rows(
        con,
        "SELECT decoded_bytes FROM bodies WHERE sha256=?",
        std::slice::from_ref(sha),
    )?
    .first()
    .and_then(|r| r["decoded_bytes"].as_i64()))
}

/// Compare exactly matched retained bodies without fetching or SEO scoring.
#[allow(clippy::too_many_arguments)]
pub fn body_diff(
    left: &Connection,
    right: &Connection,
    url: &str,
    variant_key: Option<&str>,
    representation: &str,
    text: bool,
    max_bytes: i64,
    max_lines: i64,
    left_representation: Option<&str>,
    right_representation: Option<&str>,
) -> Result<Row, StoreError> {
    let invalid = |m: &str| Err(StoreError::Invalid(m.into()));
    if url.is_empty() {
        return invalid("url must be a nonempty string");
    }
    if !REPRESENTATIONS.contains(&representation) {
        return invalid("representation must be static, rendered, or legacy_fragment");
    }
    let (lrep, rrep) = (
        left_representation.unwrap_or(representation),
        right_representation.unwrap_or(representation),
    );
    if !REPRESENTATIONS.contains(&lrep) || !REPRESENTATIONS.contains(&rrep) {
        return invalid(
            "left and right representations must be static, rendered, or legacy_fragment",
        );
    }
    limits(max_bytes, max_lines)?;
    let cross = lrep != rrep;
    let (left_row, left_problem) = observation(
        left,
        url,
        lrep,
        if cross && lrep == "rendered" {
            None
        } else {
            variant_key
        },
    )?;
    let (right_row, right_problem) = observation(
        right,
        url,
        rrep,
        if cross && rrep == "rendered" {
            None
        } else {
            variant_key
        },
    )?;
    let shown = if cross { "cross-representation" } else { lrep };
    let make = |status: &str, reason: &str, lr: Option<&Row>, rr: Option<&Row>| {
        result(
            status,
            reason,
            url,
            shown,
            variant_key,
            lr,
            rr,
            Some(lrep),
            Some(rrep),
        )
    };
    let done =
        |status: &str, reason: &str| make(status, reason, left_row.as_ref(), right_row.as_ref());

    if left_problem.is_some() || right_problem.is_some() {
        let any_variant = [&left_problem, &right_problem]
            .iter()
            .any(|p| p.as_deref().is_some_and(|s| s.contains("variant")));
        let reason = [("left", &left_problem), ("right", &right_problem)]
            .iter()
            .filter_map(|(side, p)| p.as_ref().map(|p| format!("{side}: {p}")))
            .collect::<Vec<_>>()
            .join("; ");
        return Ok(done(
            if any_variant {
                "not_comparable"
            } else {
                "missing_evidence"
            },
            &reason,
        ));
    }
    let (Some(l), Some(r)) = (left_row.as_ref(), right_row.as_ref()) else {
        return invalid("observation unexpectedly absent");
    };
    if variant_key.is_none()
        && !(cross && (lrep == "rendered" || rrep == "rendered"))
        && l["variant_key"] != r["variant_key"]
    {
        return Ok(done(
            "not_comparable",
            "HTTP variants differ between selected observations; variant_key is required",
        ));
    }
    for (side, row) in [("left", l), ("right", r)] {
        if text_of(row, "body_state") != Some("complete") || row["body_sha256"].is_null() {
            let show = |k: &str| text_of(row, k).unwrap_or("None").to_owned();
            return Ok(done(
                "missing_evidence",
                &format!(
                    "{side} body is {}/{}",
                    show("body_state"),
                    show("body_reason")
                ),
            ));
        }
    }
    if l["fidelity"] != r["fidelity"] && !(cross && textual(l) && textual(r)) {
        return Ok(done(
            "not_comparable",
            "body fidelity differs between selected observations",
        ));
    }
    if l["purpose"] != r["purpose"] {
        return Ok(done("not_comparable", "body purpose differs between scans"));
    }
    let rendered: Vec<(&str, &Row)> = [("left", l, lrep), ("right", r, rrep)]
        .into_iter()
        .filter(|(_, _, sel)| *sel == "rendered")
        .map(|(side, row, _)| (side, row))
        .collect();
    if !rendered.is_empty() {
        let provenance: Vec<(&str, Option<Row>)> = rendered
            .iter()
            .map(|(side, row)| renderer_provenance(row).map(|p| (*side, p)))
            .collect::<Result<_, _>>()?;
        if provenance.iter().any(|(_, p)| p.is_none()) {
            let sides = provenance
                .iter()
                .filter(|(_, p)| p.is_none())
                .map(|(s, _)| *s)
                .collect::<Vec<_>>()
                .join(", ");
            return Ok(done(
                "not_comparable",
                &format!("rendered renderer provenance is unavailable for {sides}"),
            ));
        }
        if provenance.len() == 2 && provenance[0].1 != provenance[1].1 {
            return Ok(done(
                "not_comparable",
                "rendered renderer provenance differs between scans",
            ));
        }
    }
    let unchanged = l["body_sha256"] == r["body_sha256"];
    let mut output = if unchanged {
        done("unchanged", "body hashes match")
    } else {
        done("changed", "body hashes differ")
    };
    if !text || unchanged {
        return Ok(output);
    }
    let mut refuse = |reason: &str| {
        output.insert("status".into(), json!("not_comparable"));
        output.insert("reason".into(), json!(reason));
    };
    if !textual(l) || !textual(r) {
        refuse("text diff requires textual body fidelity");
        return Ok(output);
    }
    let (lsize, rsize) = (
        decoded_size(left, &l["body_sha256"])?,
        decoded_size(right, &r["body_sha256"])?,
    );
    if lsize.is_none_or(|s| s > max_bytes) || rsize.is_none_or(|s| s > max_bytes) {
        refuse("text diff exceeds max_bytes before materialization");
        return Ok(output);
    }
    let left_raw = read_body(left, text_of(l, "body_sha256").unwrap_or(""), max_bytes)?;
    let right_raw = read_body(right, text_of(r, "body_sha256").unwrap_or(""), max_bytes)?;
    let (left_text, right_text) = (self::text(&left_raw, l)?, self::text(&right_raw, r)?);
    let (left_lines, right_lines) = (splitlines(&left_text), splitlines(&right_text));
    if left_lines.len() as i64 > max_lines || right_lines.len() as i64 > max_lines {
        refuse("text diff exceeds max_lines before materialization");
        return Ok(output);
    }
    let mut diff: Vec<Value> = Vec::new();
    let mut used = 0_i64;
    for line in difflib::unified_diff(&left_lines, &right_lines, "left", "right") {
        let size = line.len() as i64 + 1;
        if diff.len() as i64 >= max_lines || used + size > max_bytes {
            refuse("text diff output exceeds configured limits");
            return Ok(output);
        }
        used += size;
        diff.push(json!(line));
    }
    output.insert("text_diff".into(), Value::Array(diff));
    Ok(output)
}

#[cfg(test)]
mod tests;
