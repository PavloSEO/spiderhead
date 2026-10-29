//! Readers for backlink import sources: CSV exports, WAT extracts and provider results.

use super::*;

/// Decode a CSV file: 64 MiB cap, UTF-16 or UTF-8 BOM accepted, delimiter sniffed from
/// `,`, `;` or tab, header required, and rows wider than the header fail.
pub(super) fn read_csv(path: &Path) -> Result<Vec<Json>, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    if data.len() > MAX_IMPORT_BYTES {
        return Err("backlink import exceeds 64 MiB".to_string());
    }
    let text = if data.starts_with(b"\xff\xfe") || data.starts_with(b"\xfe\xff") {
        let (decoded, _) = if data.starts_with(b"\xff\xfe") {
            encoding_rs::UTF_16LE.decode_without_bom_handling(&data[2..])
        } else {
            encoding_rs::UTF_16BE.decode_without_bom_handling(&data[2..])
        };
        decoded.into_owned()
    } else {
        let stripped = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&data);
        String::from_utf8_lossy(stripped).into_owned()
    };
    // Pick the delimiter that splits the first line into the most columns.
    let first_line = text.lines().next().unwrap_or("");
    let delimiter = [',', ';', '\t']
        .iter()
        .max_by_key(|d| first_line.matches(**d).count())
        .copied()
        .unwrap_or(',') as u8;
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(false)
        .from_reader(text.as_bytes());
    let headers = reader.headers().map_err(|e| e.to_string())?.clone();
    if headers.is_empty() {
        return Err("CSV import requires a header".to_string());
    }
    let mut rows = Vec::new();
    for record in reader.records() {
        if rows.len() >= MAX_ROWS {
            return Err("backlink import exceeds 100000 rows".to_string());
        }
        let record =
            record.map_err(|_| "CSV row contains more columns than its header".to_string())?;
        let mut obj = Map::new();
        for (header, field) in headers.iter().zip(record.iter()) {
            obj.insert(header.to_string(), json!(field));
        }
        rows.push(json!(Json::Object(obj)));
    }
    Ok(rows)
}

/// Read a Common Crawl WAT extract of `a@/href` links.
pub(super) fn wat_rows(path: &Path) -> Result<Vec<Json>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_IMPORT_BYTES as u64 {
        return Err("Common Crawl WAT extract exceeds 64 MiB".to_string());
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let envelope = serde_json::from_str::<Json>(line)
            .map_err(|e| e.to_string())?
            .get("Envelope")
            .cloned()
            .unwrap_or(Json::Null);
        let donor = envelope["WARC-Header-Metadata"]["WARC-Target-URI"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let links =
            &envelope["Payload-Metadata"]["HTTP-Response-Metadata"]["HTML-Metadata"]["Links"];
        let Some(list) = links.as_array() else {
            return Err("Common Crawl WAT Links must be a list".to_string());
        };
        for link in list {
            if link["path"].as_str().unwrap_or("").to_lowercase() != "a@/href" {
                continue;
            }
            let target = Url::parse(&donor)
                .ok()
                .and_then(|b| b.join(link["url"].as_str().unwrap_or("")).ok())
                .map(|u| u.to_string())
                .unwrap_or_default();
            rows.push(json!({
                "source_url": donor,
                "target_url": target,
                "anchor": link["text"].as_str().unwrap_or(""),
            }));
            if rows.len() > MAX_ROWS {
                return Err("Common Crawl WAT extract exceeds 100000 links".to_string());
            }
        }
    }
    Ok(rows)
}

/// Explicit free-source request. The Yandex and Bing Webmaster adapters live in the
/// data-sources crate; this entry point reports them unavailable rather than skipping them silently.
pub fn fetch_backlink_source(source: &str, _domain: &str, _request: &Json) -> Result<Json, String> {
    if !matches!(source, "yandex_webmaster" | "bing_webmaster") {
        return Err("free backlink source must be yandex_webmaster or bing_webmaster".into());
    }
    Err("backlink provider adapters are not ported to Rust yet".to_string())
}

/// Extract URL-pair rows from a supplied result object.
pub(super) fn source_rows(item: &Json) -> Result<(Vec<Json>, Json), String> {
    let result = if item.get("result").is_some() {
        item["result"].clone()
    } else {
        item.clone()
    };
    if !result.is_object() {
        return Err("source result must be an object".to_string());
    }
    let state = result["state"].as_str().unwrap_or("partial");
    if result["ok"].as_bool() == Some(false)
        || ["failed", "skipped", "not_configured", "unavailable"].contains(&state)
    {
        let reason = result
            .get("error")
            .cloned()
            .or_else(|| result.get("reason").cloned())
            .unwrap_or(Json::Null);
        return Ok((
            Vec::new(),
            json!({"state": state, "complete": false, "reason": reason}),
        ));
    }
    let operation = result
        .get("operation")
        .cloned()
        .or_else(|| item.get("operation").cloned());
    let op_str = operation.as_ref().and_then(|v| v.as_str()).unwrap_or("");
    if [
        "backlinks_summary",
        "history",
        "domain_pages_summary",
        "external_links_history",
    ]
    .contains(&op_str)
        || (item["source"].as_str() == Some("bing_webmaster") && op_str == "links")
    {
        let aggregate = result
            .get("data")
            .cloned()
            .or_else(|| result.get("items").cloned())
            .or_else(|| result.get("rows").cloned())
            .unwrap_or(Json::Null);
        return Ok((
            Vec::new(),
            json!({
                "state": "unavailable",
                "complete": false,
                "reason": "source returned aggregate metrics without URL pairs",
                "aggregate": aggregate,
            }),
        ));
    }
    let mut rows = result
        .get("rows")
        .cloned()
        .or_else(|| result.get("items").cloned());
    let data = result.get("data").cloned();
    if rows.is_none() {
        if let Some(data) = &data {
            if data.is_object() {
                rows = data
                    .get("links")
                    .cloned()
                    .or_else(|| data.get("items").cloned());
            } else if data.is_array() {
                rows = data.clone().into();
            }
        }
    }
    let Some(rows) = rows else {
        return Ok((
            Vec::new(),
            json!({
                "state": "unavailable",
                "complete": false,
                "reason": "source returned aggregate metrics without URL pairs",
                "aggregate": data.unwrap_or(Json::Null),
            }),
        ));
    };
    let Some(list) = rows.as_array() else {
        return Err("source rows must be a list of objects".to_string());
    };
    if !list.iter().all(|r| r.is_object()) {
        return Err("source rows must be a list of objects".to_string());
    }
    if list.len() > MAX_ROWS {
        return Err("source exceeds 100000 rows".to_string());
    }
    let complete = (item["complete"].as_bool().unwrap_or(state == "complete"))
        && result["truncated"].as_bool() != Some(true);
    Ok((
        list.clone(),
        json!({
            "state": if complete { "complete" } else { "partial" },
            "complete": complete,
            "rows_supplied": true,
        }),
    ))
}
