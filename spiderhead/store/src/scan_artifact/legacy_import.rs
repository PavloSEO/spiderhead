//! Legacy `pages.jsonl` / `links.jsonl` import into scan.v1.

use super::*;

/// Legacy render-blocking field: a JSON array string, or newline-separated text; null stays null.
pub fn render_blocking_resources(value: &Value) -> Result<Option<Vec<String>>, StoreError> {
    let text = match value {
        Value::Null => return Ok(None),
        Value::String(s) => s,
        _ => return Err(bad("pages.render_blocking: expected a string or null")),
    };
    let resources: Vec<Value> = if text.trim_start().starts_with(['[', '{']) {
        match loads(text, "pages.render_blocking")? {
            Value::Array(items) => items,
            _ => {
                return Err(bad(
                    "pages.render_blocking: expected a nonempty-string resource list",
                ))
            }
        }
    } else {
        text.lines().map(|l| Value::String(l.to_owned())).collect()
    };
    let mut out = Vec::new();
    for item in resources {
        match item {
            Value::String(s) if !s.is_empty() => out.push(s),
            _ => {
                return Err(bad(
                    "pages.render_blocking: expected a nonempty-string resource list",
                ))
            }
        }
    }
    Ok(Some(out))
}

/// A `context_items` row.
pub(super) fn context_item_fields(
    item: &Map<String, Value>,
) -> Result<(String, String, String, String, String), StoreError> {
    Ok((
        field_str(item, "payload_json")?.to_owned(),
        field_str(item, "item_key")?.to_owned(),
        field_str(item, "payload_version")?.to_owned(),
        field_str(item, "completeness")?.to_owned(),
        field_str(item, "reason")?.to_owned(),
    ))
}

/// Check one legacy render-blocking context row against its page and its availability.
pub fn validate_legacy_render_blocking_context(
    con: &Connection,
    item: &Map<String, Value>,
) -> Result<(), StoreError> {
    let (payload_json, key, payload_version, completeness, reason) = context_item_fields(item)?;
    let payload = loads(&payload_json, "legacy render-blocking measurement")?;
    let key_ok = key.strip_prefix("page:").is_some_and(|digits| {
        let b = digits.as_bytes();
        !b.is_empty()
            && b.len() <= 19
            && (b[0] as char).is_ascii_digit()
            && b[0] != b'0'
            && b.iter().all(u8::is_ascii_digit)
    });
    let payload_obj = payload.as_object();
    let shape_ok = payload_obj.is_some_and(|o| {
        o.len() == 2
            && o.get("schema_version").and_then(Value::as_str) == Some("legacy_render_blocking.v1")
            && o.get("resources").is_some_and(|r| {
                r.is_null()
                    || r.as_array().is_some_and(|items| {
                        items
                            .iter()
                            .all(|u| u.as_str().is_some_and(|s| !s.is_empty()))
                    })
            })
    });
    if payload_version != "scan_context.v1" || !key_ok || !shape_ok {
        return Err(bad("legacy render-blocking measurement context is invalid"));
    }
    let resources_null =
        payload_obj.is_some_and(|o| o.get("resources").is_some_and(Value::is_null));
    let expected_state = if resources_null {
        "unavailable"
    } else {
        "complete"
    };
    let expected_reason = if resources_null {
        "legacy field records no render-blocking measurement"
    } else {
        ""
    };
    if completeness != expected_state || reason != expected_reason {
        return Err(bad(
            "legacy render-blocking measurement availability disagrees",
        ));
    }
    let page_id: i64 = key[5..]
        .parse()
        .map_err(|_| bad("legacy render-blocking measurement references an unknown page"))?;
    let known = con
        .prepare("SELECT 1 FROM pages WHERE url_id=?")?
        .exists([page_id])?;
    if !known {
        return Err(bad(
            "legacy render-blocking measurement references an unknown page",
        ));
    }
    Ok(())
}

/// Scan the `pages.jsonl` of a legacy directory into `pages` and its context rows.
pub fn import_pages(
    con: &Connection,
    source: &Path,
    limitations: &mut Vec<String>,
    inputs: &mut Vec<Map<String, Value>>,
) -> Result<(), StoreError> {
    let mut names: BTreeSet<String> = columns_of("pages")?
        .into_iter()
        .map(|c| c.name)
        .filter(|n| !matches!(n.as_str(), "url_id" | "page_ordinal" | "document_id"))
        .collect();
    for json in PAGE_JSON_COLUMNS {
        names.remove(*json);
    }
    names.extend(PAGE_RECORD_KEYS.iter().map(|s| (*s).to_owned()));
    let late: BTreeSet<&str> = LATE_PAGE_FIELDS.iter().map(|(n, _)| *n).collect();
    let optional: BTreeSet<&str> = OPTIONAL_PAGE_RECORD_FIELDS.iter().copied().collect();
    let required: BTreeSet<String> = names
        .iter()
        .filter(|n| !late.contains(n.as_str()) && !optional.contains(n.as_str()))
        .cloned()
        .collect();

    for (ordinal, record) in jsonl(&source.join("pages.jsonl"), limitations, inputs)?.enumerate() {
        let mut record = record?;
        let keys: BTreeSet<String> = record.keys().cloned().collect();
        let missing = required.iter().any(|k| !keys.contains(k));
        let extra = keys
            .iter()
            .any(|k| !names.contains(k) && k != "render_blocking" && k != "attempts");
        if missing || extra {
            let diff: Vec<&String> = keys.symmetric_difference(&names).collect();
            return Err(bad(format!(
                "pages.jsonl: fields differ from scan.v1: {diff:?}"
            )));
        }
        let has_render_blocking = record.contains_key("render_blocking");
        let render_resources =
            render_blocking_resources(&record.remove("render_blocking").unwrap_or(Value::Null))?;
        record.remove("attempts");
        for (name, _) in LATE_PAGE_FIELDS {
            if record.get(*name).is_some_and(Value::is_null) {
                return Err(bad(format!(
                    "pages.{name}: present fields must have their recorded type"
                )));
            }
            record.entry((*name).to_owned()).or_insert(Value::Null);
        }
        let mut row = record;

        let alternates = row.remove("hreflang").unwrap_or(Value::Null);
        if !alternates.is_null() {
            hreflang(&alternates)?;
        }
        row.insert("hreflang_json".into(), dump_or_null(&alternates));

        let outline = row.remove("heading_outline").unwrap_or(Value::Null);
        if !outline.is_null() {
            heading_outline(&outline)?;
        }
        row.insert("heading_outline_json".into(), dump_or_null(&outline));

        let invalid_targets = row.remove("invalid_link_targets").unwrap_or(Value::Null);
        if !invalid_targets.is_null() {
            invalid_link_targets(&invalid_targets)?;
        }
        row.insert(
            "invalid_link_targets_json".into(),
            dump_or_null(&invalid_targets),
        );

        row.entry("transport_upgraded".to_owned())
            .or_insert(Value::Bool(false));
        for key in [
            "sentence_count",
            "avg_words_per_sentence",
            "flesch",
            "readability",
        ] {
            row.entry(key.to_owned()).or_insert(Value::Null);
        }

        let placement = row.remove("link_placement").unwrap_or(Value::Null);
        if !placement.is_null() {
            link_placement(&placement)?;
        }
        row.insert("link_placement_json".into(), dump_or_null(&placement));

        let signals = row.remove("trust_signals").unwrap_or(Value::Null);
        if !signals.is_null() {
            trust_signals(&signals)?;
        }
        row.insert("trust_signals_json".into(), dump_or_null(&signals));

        let duplicates = row.remove("duplicate_ids").unwrap_or(Value::Null);
        if !duplicates.is_null() {
            duplicate_ids(&duplicates)?;
        }
        row.insert("duplicate_ids_json".into(), dump_or_null(&duplicates));

        let canonical_chain = row.remove("canonical_chain").unwrap_or(Value::Null);
        if !canonical_chain.is_null()
            && !canonical_chain
                .as_array()
                .is_some_and(|hops| hops.iter().all(Value::is_object))
        {
            return Err(bad("pages.canonical_chain must be a list of objects"));
        }
        row.insert(
            "canonical_chain_json".into(),
            dump_or_null(&canonical_chain),
        );

        let url_text = row
            .remove("url")
            .and_then(|v| v.as_str().map(str::to_owned))
            .ok_or_else(|| bad("a page/link URL must be a nonempty string"))?;
        let url_id = url(con, &url_text)?;
        row.insert("url_id".into(), Value::from(url_id));
        row.insert("page_ordinal".into(), Value::from(ordinal as i64));

        let chain = row.remove("redirect_chain").unwrap_or(Value::Null);
        if !chain
            .as_array()
            .is_some_and(|hops| hops.iter().all(Value::is_object))
        {
            return Err(bad("pages.redirect_chain must be a list of objects"));
        }
        row.insert("redirect_chain_json".into(), Value::String(dump(&chain)));

        for key in PAGE_BOOLS {
            let value = row
                .get(*key)
                .cloned()
                .ok_or_else(|| bad(format!("pages.{key}: missing field")))?;
            if !value.is_null() {
                let Value::Bool(b) = value else {
                    return Err(bad(format!("pages.{key}: expected boolean")));
                };
                row.insert((*key).to_owned(), Value::from(i64::from(b)));
            }
        }
        for key in PAGE_NONNEGATIVE_INTS {
            let value = row
                .get(*key)
                .cloned()
                .ok_or_else(|| bad(format!("pages.{key}: missing field")))?;
            if !value.is_null() && !value.as_i64().is_some_and(|v| v >= 0) {
                return Err(bad(format!("pages.{key}: expected a nonnegative integer")));
            }
        }
        insert(con, "pages", &row)?;

        if has_render_blocking {
            let resources_null = render_resources.is_none();
            let mut payload = Map::new();
            payload.insert(
                "schema_version".into(),
                Value::from("legacy_render_blocking.v1"),
            );
            payload.insert(
                "resources".into(),
                match render_resources {
                    Some(list) => Value::Array(list.into_iter().map(Value::String).collect()),
                    None => Value::Null,
                },
            );
            let mut item = Map::new();
            item.insert("kind".into(), Value::from("legacy_render_blocking"));
            item.insert("item_key".into(), Value::String(format!("page:{url_id}")));
            item.insert("payload_version".into(), Value::from("scan_context.v1"));
            item.insert(
                "payload_json".into(),
                Value::String(dump(&Value::Object(payload))),
            );
            item.insert(
                "completeness".into(),
                Value::from(if resources_null {
                    "unavailable"
                } else {
                    "complete"
                }),
            );
            item.insert(
                "reason".into(),
                Value::from(if resources_null {
                    "legacy field records no render-blocking measurement"
                } else {
                    ""
                }),
            );
            insert(con, "context_items", &item)?;
        }
    }
    Ok(())
}

/// JSON text of a present value, SQL NULL for an absent one.
pub(super) fn dump_or_null(value: &Value) -> Value {
    if value.is_null() {
        Value::Null
    } else {
        Value::String(dump(value))
    }
}

/// Import `links.jsonl` as `legacy_unknown` link evidence.
pub fn import_links(
    con: &Connection,
    source: &Path,
    limitations: &mut Vec<String>,
    inputs: &mut Vec<Map<String, Value>>,
) -> Result<(), StoreError> {
    let fields: BTreeSet<&str> = LINK_FIELDS.iter().copied().collect();
    for record in jsonl(&source.join("links.jsonl"), limitations, inputs)? {
        let mut record = record?;
        let keys: BTreeSet<&str> = record.keys().map(String::as_str).collect();
        let allowed = fields.iter().all(|f| keys.contains(f))
            && keys
                .iter()
                .all(|k| fields.contains(k) || *k == "anchor_source");
        if !allowed {
            let diff: Vec<&&str> = keys.symmetric_difference(&fields).collect();
            return Err(bad(format!(
                "links.jsonl: fields differ from scan.v1: {diff:?}"
            )));
        }
        match record.get("anchor_source") {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) if s.is_empty() || s == "alt" => {}
            _ => return Err(bad("links.anchor_source: expected '' or 'alt'")),
        }
        record
            .entry("anchor_source".to_owned())
            .or_insert_with(|| Value::from(""));
        let source_text = record
            .remove("source")
            .and_then(|v| v.as_str().map(str::to_owned))
            .ok_or_else(|| bad("a page/link URL must be a nonempty string"))?;
        let destination_text = record
            .remove("destination")
            .and_then(|v| v.as_str().map(str::to_owned))
            .ok_or_else(|| bad("a page/link URL must be a nonempty string"))?;
        let source_id = url(con, &source_text)?;
        record.insert("source_url_id".into(), Value::from(source_id));
        record.insert(
            "destination_url_id".into(),
            Value::from(url(con, &destination_text)?),
        );
        let ordinal: i64 = con.query_row(
            "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM links WHERE source_url_id=? AND evidence_representation='legacy_unknown'",
            [source_id],
            |r| r.get(0),
        )?;
        record.insert("ordinal".into(), Value::from(ordinal));
        record.insert(
            "evidence_representation".into(),
            Value::from("legacy_unknown"),
        );
        let Some(Value::Bool(nofollow)) = record.get("nofollow").cloned() else {
            return Err(bad("links.nofollow: expected boolean"));
        };
        record.insert("nofollow".into(), Value::from(i64::from(nofollow)));
        let rel = record.remove("rel").unwrap_or(Value::Null);
        if !rel
            .as_array()
            .is_some_and(|tokens| tokens.iter().all(Value::is_string))
        {
            return Err(bad("links.rel: expected a string list"));
        }
        record.insert("rel_json".into(), Value::String(dump(&rel)));
        insert(con, "links", &record)?;
    }
    Ok(())
}
