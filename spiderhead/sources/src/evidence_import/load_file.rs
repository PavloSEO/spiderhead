//! Split out of `evidence_import`.

use super::*;

pub(super) fn detect_kind(path: &Path, raw: &[u8]) -> Result<&'static str, CfError> {
    let suffix = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match suffix.as_str() {
        "csv" => return Ok("csv"),
        "xlsx" => return Ok("xlsx"),
        "json" => return Ok("json"),
        "jsonl" | "ndjson" => {
            return Err(err("JSONL row streams are not a supported evidence shape"))
        }
        _ => {}
    }
    let stripped = raw.iter().find(|b| !b.is_ascii_whitespace());
    if stripped == Some(&b'{') || stripped == Some(&b'[') {
        return Ok("json");
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Err(err(format!(
        "cannot determine evidence kind for '{}'; use .csv, .xlsx or .json",
        name.replace('\'', "\\'")
    )))
}

pub(super) fn read_csv(path: &Path) -> Result<(Vec<Value>, Vec<String>), CfError> {
    let raw = std::fs::read(path).map_err(|e| err(e.to_string()))?;
    let text = String::from_utf8(if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        raw[3..].to_vec()
    } else {
        raw
    })
    .map_err(|_| err("CSV is not decodable as UTF-8"))?;
    let mut reader = csv::Reader::from_reader(text.as_bytes());
    let columns: Vec<String> = reader
        .headers()
        .map_err(|e| err(e.to_string()))?
        .iter()
        .map(|h| h.to_owned())
        .collect();
    if columns.len() > MAX_COLUMNS {
        return Err(err(format!("CSV exceeds the {MAX_COLUMNS}-column bound")));
    }
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| err(e.to_string()))?;
        let mut row = Map::new();
        for (index, cell) in record.iter().enumerate() {
            if let Some(name) = columns.get(index) {
                row.insert(name.clone(), json!(cell));
            }
        }
        rows.push(Value::Object(row));
        if rows.len() > MAX_ROWS {
            return Err(err(format!("CSV exceeds the {MAX_ROWS}-row bound")));
        }
    }
    Ok((rows, columns))
}

pub(super) fn read_xlsx(
    path: &Path,
    sheet: Option<&str>,
) -> Result<(Vec<Value>, Vec<String>), CfError> {
    let mut workbook = calamine::open_workbook_auto(path)
        .map_err(|e| err(format!("cannot read XLSX workbook: {e}")))?;
    let names = Reader::sheet_names(&workbook);
    let name = match sheet {
        Some(want) => {
            if !names.iter().any(|n| n == want) {
                return Err(err(
                    "sheet {want:?} not found in workbook; available sheets: {names:?}",
                ));
            }
            want.to_owned()
        }
        None => names.first().cloned().unwrap_or_default(),
    };
    let range = Reader::worksheet_range(&mut workbook, &name)
        .map_err(|e| err(format!("cannot read XLSX workbook: {e}")))?;
    let mut iter = range.rows();
    let header = iter
        .next()
        .ok_or_else(|| err("XLSX sheet has no header row"))?;
    let columns: Vec<String> = header
        .iter()
        .enumerate()
        .map(|(index, cell)| match cell {
            calamine::Data::Empty => format!("column_{}", index + 1),
            other => match json_safe_cell(other.clone()) {
                Value::String(s) => s,
                v => pystr::repr_value(&v),
            },
        })
        .collect();
    if columns.len() > MAX_COLUMNS {
        return Err(err(format!("XLSX exceeds the {MAX_COLUMNS}-column bound")));
    }
    if columns.iter().collect::<BTreeSet<_>>().len() != columns.len() {
        return Err(err("XLSX header contains duplicate column names"));
    }
    let mut rows = Vec::new();
    for raw_row in iter {
        let mut row = Map::new();
        for (index, name) in columns.iter().enumerate() {
            let cell = raw_row
                .get(index)
                .map(|c| json_safe_cell(c.clone()))
                .unwrap_or(Value::Null);
            row.insert(name.clone(), cell);
        }
        if row.values().all(Value::is_null) {
            continue;
        }
        rows.push(Value::Object(row));
        if rows.len() > MAX_ROWS {
            return Err(err(format!("XLSX exceeds the {MAX_ROWS}-row bound")));
        }
    }
    Ok((rows, columns))
}

pub(super) fn read_json(raw: &[u8]) -> Result<(String, Vec<Value>, Value, Value), CfError> {
    let body: Value = serde_json::from_slice(raw).map_err(|e| {
        let text = String::from_utf8_lossy(raw);
        let message = crate::pyjson::decode_error(&text).unwrap_or_else(|| e.to_string());
        err(format!("JSON evidence is not parseable: {message}"))
    })?;
    if let Value::Object(map) = &body {
        if map.len() == 2
            && map.contains_key("evidence")
            && map.contains_key("result")
            && map["evidence"].is_object()
            && map["evidence"].get("format") == Some(&json!(EVIDENCE_FORMAT))
            && map["result"].is_object()
        {
            let result = &map["result"];
            let rows = result.get("rows");
            let valid = match rows {
                Some(Value::Array(items)) => {
                    items.len() <= MAX_ROWS && items.iter().all(Value::is_object)
                }
                _ => false,
            };
            if !valid {
                return Err(err(
                    "saved provider operation does not contain bounded joinable rows",
                ));
            }
            return Ok((
                "provider_evidence".to_owned(),
                rows.and_then(Value::as_array).cloned().unwrap_or_default(),
                map["evidence"].clone(),
                result.clone(),
            ));
        }
    }
    let rows = if let Value::Object(map) = &body {
        match map.get("rows") {
            Some(Value::Array(items)) => items.clone(),
            _ => {
                return Err(err(
                    "JSON evidence must be a row array, an object with a rows array, \
                     or a saved seohead.provider-evidence.v1 envelope",
                ))
            }
        }
    } else if let Value::Array(items) = &body {
        items.clone()
    } else {
        return Err(err(
            "JSON evidence must be a row array, an object with a rows array, \
             or a saved seohead.provider-evidence.v1 envelope",
        ));
    };
    if rows.len() > MAX_ROWS || !rows.iter().all(Value::is_object) {
        return Err(err("JSON evidence requires a bounded list of row objects"));
    }
    Ok(("json".to_owned(), rows, Value::Null, Value::Null))
}

/// `load_import_file(path, *, sheet)` — bounded offline file load.
pub fn load_import_file(path: &str, sheet: Option<&str>) -> Result<Value, CfError> {
    let source = Path::new(path);
    let meta = source
        .symlink_metadata()
        .map_err(|_| err("evidence file must be a regular file"))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(err("evidence file must be a regular file"));
    }
    let size = meta.len();
    if size > MAX_FILE_BYTES {
        return Err(err(format!(
            "evidence file exceeds the {MAX_FILE_BYTES}-byte bound"
        )));
    }
    let raw = std::fs::read(source).map_err(|e| err(e.to_string()))?;
    let mut kind = detect_kind(source, &raw)?.to_owned();
    let mut file_meta = json!({
        "name": source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        "sha256": sha256(&raw),
        "bytes": size,
    });
    let (rows, columns, mut envelope, mut result);
    match kind.as_str() {
        "csv" => {
            let (r, c) = read_csv(source)?;
            rows = r;
            columns = c;
            envelope = Value::Null;
            result = Value::Null;
        }
        "xlsx" => {
            let (r, c) = read_xlsx(source, sheet)?;
            rows = r;
            columns = c;
            envelope = Value::Null;
            result = Value::Null;
        }
        _ => {
            let (k, r, e, res) = read_json(&raw)?;
            kind = k;
            rows = r;
            envelope = e;
            result = res;
            let mut names = BTreeSet::new();
            for row in &rows {
                if let Value::Object(map) = row {
                    names.extend(map.keys().cloned());
                }
            }
            columns = names.into_iter().take(MAX_COLUMNS + 1).collect();
            if columns.len() > MAX_COLUMNS {
                return Err(err(format!(
                    "JSON evidence exceeds the {MAX_COLUMNS}-column bound"
                )));
            }
        }
    }
    if kind == "provider_evidence" {
        use spiderhead_core::filesystem::StatCompat;
        if !crate::credentials::is_private_mode(meta.mode()) {
            return Err(err(
                "saved provider evidence must live in a private-mode file",
            ));
        }
    }
    file_meta["kind"] = json!(kind);
    if envelope.is_null() {
        envelope = Value::Null;
    }
    if result.is_null() {
        result = Value::Null;
    }
    Ok(json!({
        "kind": kind,
        "rows": rows,
        "columns": columns,
        "envelope": envelope,
        "result": result,
        "file": file_meta,
    }))
}
