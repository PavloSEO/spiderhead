//! Export inputs: a scan artifact or an Export Analyzer audit document, opened as a [`Source`].

use super::*;

/// Keys of the run block copied into the provenance of an audit document.
pub const RUN_PROVENANCE_KEYS: &[&str] = &[
    "project",
    "input_mode",
    "source",
    "exports_dir",
    "sf_version_detected",
    "generated_at",
    "profile",
    "crawl_valid",
    "crawl_invalid_reason",
    "crawl_partial",
    "crawl_stopped_reason",
    "exports_used",
    "exports_missing",
];

/// Record data behind a source.
enum Backend {
    Scan {
        con: Connection,
        reader: Option<Box<AuditV2Reader>>,
        issues: Vec<Value>,
    },
    Document {
        pages: Vec<Value>,
        issues: Vec<Value>,
    },
}

/// One opened export input: metadata blocks plus per-type record streams.
pub struct Source {
    /// Run identity and writer state.
    pub provenance: Map<String, Value>,
    /// Check inventory.
    pub checks: Value,
    /// Declared capabilities.
    pub capabilities: Value,
    /// Declared limitations.
    pub limitations: Value,
    /// Per-record-type state.
    pub record_states: IndexMap<String, Value>,
    /// Saved audit run totals.
    pub run_statistics: Value,
    /// Field catalogue per record type.
    pub catalogs: IndexMap<String, Vec<String>>,
    /// Row counts per record type.
    pub counts: IndexMap<String, Option<i64>>,
    backend: Backend,
}

impl Source {
    /// All records of one type as stored (before projection).
    pub fn records(&self, name: &str) -> Result<Vec<Record>, StoreError> {
        self.record_iter(name)?.collect()
    }

    /// Lazily yield records of one type; scan backends page through bounded SQL
    /// windows instead of materializing every row.
    pub fn record_iter<'a>(&'a self, name: &str) -> Result<RecordStream<'a>, StoreError> {
        let from_values = |items: &'a [Value]| -> RecordStream<'a> {
            Box::new(items.iter().filter_map(|v| v.as_object().cloned().map(Ok)))
        };
        Ok(match (&self.backend, name) {
            (Backend::Scan { con, .. }, "pages") => Box::new(Windowed::pages(con)),
            (Backend::Scan { con, .. }, "links") => Box::new(Windowed::links(con)),
            (
                Backend::Scan {
                    reader: Some(r), ..
                },
                "findings",
            ) => Box::new(
                r.iter_collection("/issues")?
                    .into_iter()
                    .filter_map(|v| v.as_object().cloned().map(Ok)),
            ),
            (Backend::Scan { issues, .. }, "findings") => from_values(issues),
            (Backend::Document { pages, .. }, "pages") => from_values(pages),
            (Backend::Document { issues, .. }, "findings") => from_values(issues),
            _ => Box::new(std::iter::empty()),
        })
    }
}

/// Rows materialized per window when lazily streaming scan records.
const RECORD_WINDOW: i64 = 256;

/// Bounded-window cursor over `pages`/`links` scan rows.
struct Windowed<'a> {
    con: &'a Connection,
    pages: bool,
    buffer: std::vec::IntoIter<Record>,
    offset: i64,
    done: bool,
}

impl<'a> Windowed<'a> {
    fn pages(con: &'a Connection) -> Self {
        Self {
            con,
            pages: true,
            buffer: Vec::new().into_iter(),
            offset: 0,
            done: false,
        }
    }

    fn links(con: &'a Connection) -> Self {
        Self {
            con,
            pages: false,
            buffer: Vec::new().into_iter(),
            offset: 0,
            done: false,
        }
    }
}

impl Iterator for Windowed<'_> {
    type Item = Result<Record, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(record) = self.buffer.next() {
                return Some(Ok(record));
            }
            if self.done {
                return None;
            }
            let window = match if self.pages {
                crate::exports::page_rows_limited(self.con, None, Some(RECORD_WINDOW), self.offset)
            } else {
                crate::exports::link_rows_limited(self.con, Some(RECORD_WINDOW), self.offset)
            } {
                Ok(window) => window,
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            };
            self.offset += window.len() as i64;
            self.done = (window.len() as i64) < RECORD_WINDOW;
            self.buffer = window.into_iter();
        }
    }
}

fn mapping<'a>(value: &'a Value, key: &str) -> Option<&'a Map<String, Value>> {
    value.get(key).and_then(Value::as_object)
}

/// Summarize which checks ran, were skipped, disabled, or stayed unaccounted.
///
/// The inventory comes from the saved audit's own declarations through the shared
/// `capability_rows` contract, so an export states exactly what the audit states.
pub fn check_coverage(document: &Value) -> Result<Value, StoreError> {
    let empty = Map::new();
    let run = mapping(document, "run").unwrap_or(&empty);
    let coverage = document
        .get("summary")
        .and_then(|s| mapping(s, "check_coverage"));
    let doc_map: Map<String, Value> = document.as_object().cloned().unwrap_or_default();
    let (mut ran, mut skipped, mut disabled, mut unaccounted) = (vec![], vec![], vec![], vec![]);
    for row in crate::evidence_contract::capability_rows(&doc_map)? {
        let get = |k: &str| row.get(k).cloned().unwrap_or(Value::Null);
        match row.get("capability").and_then(Value::as_str).unwrap_or("") {
            "finding_recorded" | "ran_without_finding" => ran.push(get("check")),
            "skipped" => skipped.push(json!({"id": get("check"), "reason": get("reason")})),
            "disabled" => disabled.push(json!({"id": get("check"), "reason": get("reason")})),
            _ => unaccounted.push(get("check")),
        }
    }
    let partial = run.get("crawl_partial").is_some_and(truthy);
    let (state, reason, ratio, totals) = match coverage {
        Some(c) => {
            let totals: Map<String, Value> = [
                "checks_total",
                "checks_fired",
                "checks_skipped",
                "checks_disabled",
                "checks_silent",
            ]
            .iter()
            .map(|n| (n.to_string(), c.get(*n).cloned().unwrap_or(Value::Null)))
            .collect();
            (
                if partial { "partial" } else { "complete" },
                if partial { "crawl was partial; clean results cover only the saved scope" } else { "" },
                c.get("coverage").cloned().unwrap_or(Value::Null),
                Value::Object(totals),
            )
        }
        None => (
            "unavailable",
            "saved audit declares no check_coverage summary; only finding-bearing checks are identifiable",
            Value::Null,
            Value::Null,
        ),
    };
    Ok(json!({
        "state": state, "reason": reason, "coverage": ratio, "totals": totals,
        "ran": ran, "skipped": skipped, "disabled": disabled, "unaccounted": unaccounted,
    }))
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// State block of the saved audit document: present with its identity, or unavailable.
pub fn audit_presence(row: Option<&Map<String, Value>>) -> Value {
    match row {
        None => json!({"state": "unavailable", "reason": "scan retains no saved audit document"}),
        Some(r) => json!({
            "state": "present",
            "schema_version": r.get("schema_version").cloned().unwrap_or(Value::Null),
            "analyzer_version": r.get("analyzer_version").cloned().unwrap_or(Value::Null),
            "created_at": r.get("created_at").cloned().unwrap_or(Value::Null),
        }),
    }
}

/// `{state, reason}` of one declared capability, or an unavailable stand-in.
pub fn capability_state(capabilities: &Value, name: &str) -> Value {
    if let Some(entry) = capabilities.get(name).filter(|e| e.is_object()) {
        if let Some(state) = entry.get("state").and_then(Value::as_str) {
            let reason = entry.get("reason").and_then(Value::as_str).unwrap_or("");
            return json!({"state": state, "reason": reason});
        }
    }
    json!({"state": "unavailable", "reason": format!("scan declares no {name} capability")})
}

fn header_map(con: &Connection) -> Result<Map<String, Value>, StoreError> {
    let mut stmt = con.prepare("SELECT * FROM scan WHERE singleton=1")?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([])?;
    let row = rows
        .next()?
        .ok_or_else(|| scan_error("scan has no header row"))?;
    let mut out = Map::new();
    for (i, name) in names.iter().enumerate() {
        out.insert(name.clone(), cell_value(row.get_ref(i)?));
    }
    Ok(out)
}

fn text_of<'a>(map: &'a Map<String, Value>, key: &str) -> &'a str {
    map.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Open a scan artifact as an export source.
pub fn sqlite_source(path: &Path) -> Result<Source, StoreError> {
    let con = open_scan(path, false, None)?;
    let header = header_map(&con)?;
    let audit_row: Option<Map<String, Value>> = {
        let mut stmt = con.prepare(
            "SELECT schema_version, analyzer_version, created_at, document_json FROM audit WHERE singleton=1",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(r) => {
                let mut m = Map::new();
                for (i, k) in [
                    "schema_version",
                    "analyzer_version",
                    "created_at",
                    "document_json",
                ]
                .iter()
                .enumerate()
                {
                    m.insert(k.to_string(), cell_value(r.get_ref(i)?));
                }
                Some(m)
            }
            None => None,
        }
    };
    let mut document: Option<Value> = match &audit_row {
        Some(r) => Some(loads(text_of(r, "document_json"), "audit")?),
        None => None,
    };
    let mut audit_row = audit_row;
    let mut reader = None;
    if audit_v2_path(path).exists() {
        let r = AuditV2Reader::new(path, true, false, None)?;
        let mut doc = r.header.clone();
        // capability_rows needs only the finding-bearing check identities, never every payload.
        let registry = spiderhead_formats::check_registry::builtin().ok();
        let mut fired = std::collections::BTreeSet::new();
        for item in r.iter_collection("/issues")? {
            if let Some(check) = item.get("check").and_then(Value::as_str) {
                if registry.is_some_and(|reg| reg.get(check).is_some()) {
                    fired.insert(check.to_string());
                }
            }
        }
        doc["issues"] = Value::Array(fired.into_iter().map(|c| json!({"check": c})).collect());
        let mut row = Map::new();
        row.insert(
            "schema_version".into(),
            doc.get("schema_version").cloned().unwrap_or(Value::Null),
        );
        row.insert(
            "analyzer_version".into(),
            header.get("writer_version").cloned().unwrap_or(Value::Null),
        );
        row.insert(
            "created_at".into(),
            doc.get("run")
                .and_then(|r| r.get("generated_at"))
                .cloned()
                .unwrap_or(Value::Null),
        );
        audit_row = Some(row);
        document = Some(doc);
        reader = Some(Box::new(r));
    }
    let capabilities = loads(text_of(&header, "capabilities_json"), "capabilities")?;
    let limitations = loads(text_of(&header, "limitations_json"), "limitations")?;
    let count =
        |sql: &str| -> Result<i64, StoreError> { Ok(con.query_row(sql, [], |r| r.get(0))?) };
    let page_count = count("SELECT COUNT(*) FROM pages")?;
    let link_count = count("SELECT COUNT(*) FROM links")?;
    let issues: Vec<Value> = document
        .as_ref()
        .and_then(|d| d.get("issues"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter(|i| i.is_object()).cloned().collect())
        .unwrap_or_default();
    let partial = header.get("crawl_partial").is_some_and(truthy);
    let findings_state = if document.is_some() {
        json!({
            "state": if partial { "partial" } else { "complete" },
            "reason": if partial { "crawl was partial; findings cover only the saved scope" } else { "" },
        })
    } else {
        json!({"state": "unavailable", "reason": "scan retains no saved audit document"})
    };
    let mut record_states = IndexMap::new();
    record_states.insert(
        "pages".to_string(),
        capability_state(&capabilities, "pages"),
    );
    record_states.insert(
        "links".to_string(),
        capability_state(&capabilities, "links"),
    );
    record_states.insert("findings".to_string(), findings_state);
    let totals = document
        .as_ref()
        .and_then(|d| d.get("summary"))
        .and_then(|s| mapping(s, "totals"))
        .cloned();
    let finding_count = match (&reader, &document) {
        (Some(r), _) => Some(r.count("/issues")?),
        (None, Some(_)) => Some(issues.len() as i64),
        _ => None,
    };
    let mut provenance = Map::new();
    let get = |k: &str| header.get(k).cloned().unwrap_or(Value::Null);
    provenance.insert("input_kind".into(), get("format_version"));
    provenance.insert("input".into(), json!(path.to_string_lossy()));
    for k in [
        "scan_uuid",
        "format_version",
        "evidence_version",
        "source_kind",
        "parent_scan_uuid",
        "start_url",
        "lifecycle",
        "finish_reason",
    ] {
        provenance.insert(k.into(), get(k));
    }
    provenance.insert("crawl_partial".into(), json!(partial));
    provenance.insert(
        "corpus_partial".into(),
        json!(header.get("corpus_partial").is_some_and(truthy)),
    );
    for k in [
        "created_at",
        "finished_at",
        "writer_version",
        "writer_revision",
    ] {
        provenance.insert(k.into(), get(k));
    }
    provenance.insert("audit".into(), audit_presence(audit_row.as_ref()));
    let checks = match &document {
        Some(d) => check_coverage(d)?,
        None => json!({
            "state": "unavailable", "reason": "scan retains no saved audit document",
            "coverage": null, "totals": null, "ran": [], "skipped": [], "disabled": [], "unaccounted": [],
        }),
    };
    let run_statistics = match totals {
        Some(t) => json!({"state": "complete", "totals": t}),
        None => json!({"state": "unavailable", "reason": "saved audit declares no run totals"}),
    };
    Ok(Source {
        provenance,
        checks,
        capabilities,
        limitations,
        record_states,
        run_statistics,
        catalogs: catalogs(PAGE_FIELDS_SCAN),
        counts: [
            ("pages", Some(page_count)),
            ("links", Some(link_count)),
            ("findings", finding_count),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        backend: Backend::Scan {
            con,
            reader,
            issues,
        },
    })
}

fn catalogs(pages: &[&str]) -> IndexMap<String, Vec<String>> {
    let owned = |f: &[&str]| f.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    [
        ("pages", owned(pages)),
        ("links", owned(LINK_FIELDS)),
        ("findings", owned(FINDING_FIELDS)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// Open an Export Analyzer `audit.json` document as an export source.
pub fn document_source(document: &Value, label: &str) -> Result<Source, StoreError> {
    use spiderhead_formats::audit_kind::detect_kind;
    match detect_kind(document) {
        Ok(Some(kind)) if kind.name() == "sf-audit" => {}
        Ok(Some(_)) => {
            return Err(scan_error(
                "site-audit documents are rendered reports, not retained scan data; scan export requires an Export Analyzer audit.json or a scan.v1 artifact",
            ))
        }
        Ok(None) => {
            return Err(scan_error(
                "unrecognized input: expected an Export Analyzer audit.json (schema_version '2.0') or a scan.v1 artifact",
            ))
        }
        Err(message) => return Err(scan_error(message)),
    }
    let empty = Map::new();
    let run = mapping(document, "run").unwrap_or(&empty);
    let objects = |key: &str| -> Vec<Value> {
        document
            .get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter(|i| i.is_object()).cloned().collect())
            .unwrap_or_default()
    };
    let pages = objects("pages");
    let issues = objects("issues");
    let partial = run.get("crawl_partial").is_some_and(truthy);
    let partial_reason = if partial {
        "saved audit declares the crawl partial"
    } else {
        ""
    };
    let totals = document
        .get("summary")
        .and_then(|s| mapping(s, "totals"))
        .cloned();
    let limitations: Vec<Value> = match run.get("exports_missing") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(|n| json!({"kind": "missing_export", "name": n}))
            .collect(),
        _ => Vec::new(),
    };
    let schema_version = document
        .get("schema_version")
        .cloned()
        .unwrap_or(Value::Null);
    let run_block: Map<String, Value> = RUN_PROVENANCE_KEYS
        .iter()
        .map(|k| (k.to_string(), run.get(*k).cloned().unwrap_or(Value::Null)))
        .collect();
    let tool = document.get("tool").cloned().unwrap_or(Value::Null);
    let mut provenance = Map::new();
    provenance.insert("input_kind".into(), json!("audit_document"));
    provenance.insert("input".into(), json!(label));
    provenance.insert("schema_version".into(), schema_version.clone());
    provenance.insert("tool".into(), tool.clone());
    provenance.insert(
        "scan_uuid".into(),
        run.get("scan_uuid").cloned().unwrap_or(Value::Null),
    );
    provenance.insert("run".into(), Value::Object(run_block));
    provenance.insert(
        "audit".into(),
        json!({
            "state": "present", "schema_version": schema_version, "generated_at": run.get("generated_at").cloned().unwrap_or(Value::Null), "tool": tool,
        }),
    );
    let mut record_states = IndexMap::new();
    record_states.insert(
        "pages".to_string(),
        json!({"state": if partial {"partial"} else {"complete"}, "reason": partial_reason}),
    );
    record_states.insert(
        "links".to_string(),
        json!({"state": "unavailable", "reason": "audit documents retain no link records"}),
    );
    record_states.insert(
        "findings".to_string(),
        json!({"state": if partial {"partial"} else {"complete"}, "reason": partial_reason}),
    );
    Ok(Source {
        provenance,
        checks: check_coverage(document)?,
        capabilities: json!({"state": "unavailable", "reason": "audit documents retain no capability declarations"}),
        limitations: Value::Array(limitations),
        record_states,
        run_statistics: match totals {
            Some(t) => json!({"state": "complete", "totals": t}),
            None => json!({"state": "unavailable", "reason": "saved audit declares no run totals"}),
        },
        catalogs: catalogs(PAGE_FIELDS_DOCUMENT),
        counts: [
            ("pages", Some(pages.len() as i64)),
            ("links", None),
            ("findings", Some(issues.len() as i64)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        backend: Backend::Document { pages, issues },
    })
}

/// Open the export input: a document mapping, a scan artifact, or an audit JSON file.
pub fn open_source(input_value: &Value) -> Result<Source, StoreError> {
    if input_value.is_object() {
        return document_source(input_value, "<document>");
    }
    let text = match input_value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let path = PathBuf::from(&text);
    if !path.exists() {
        return Err(scan_error(format!("input not found: {}", path.display())));
    }
    if is_sqlite_input(&json!(text)) {
        return sqlite_source(&path);
    }
    let parsed = std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()));
    let document =
        parsed.map_err(|e| scan_error(format!("{}: unreadable input ({e})", path.display())))?;
    if !document.is_object() {
        return Err(scan_error(format!(
            "{}: expected a JSON audit object or a scan.v1 artifact",
            path.display()
        )));
    }
    document_source(&document, &path.to_string_lossy())
}

/// Project each record to exactly the selected fields, in catalogue order.
///
/// A field the row does not carry becomes an explicit `null` rather than a fabricated zero or a
/// silently dropped key.
pub fn record_stream<'a>(
    source: &'a Source,
    name: &'a str,
    fields: &'a [String],
) -> Result<RecordStream<'a>, StoreError> {
    Ok(Box::new(source.record_iter(name)?.map(move |record| {
        let record = record?;
        Ok(fields
            .iter()
            .map(|f| (f.clone(), record.get(f).cloned().unwrap_or(Value::Null)))
            .collect::<Record>())
    })))
}
