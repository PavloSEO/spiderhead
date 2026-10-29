//! Ported tests for the Sheets transport (reference the reference suite)
//! using an API-shaped, stateful offline `SheetsDouble` — no network, no
//! authentication. Retry-After tests exercise the public `google_backoff` seam
//! with an explicit clock.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;
use spiderhead_reports::bi::google_client::{google_backoff, Jitter, Sleeper};
use spiderhead_reports::bi::project::{export_bi, ExportLimits};
use spiderhead_reports::bi::schema::{BI_SCHEMA_VERSION, DEFAULT_MAX_OUTPUT_BYTES};
use spiderhead_reports::bi_destinations::{
    apply_with_client, bi_err, destination_preview, filter_package, sheets_plan, uncertain,
    BeginArgs, DestinationClient, DestinationError, FetchError, Fetcher, GoogleRestClient,
    GoogleSheetsClient, SHEETS_MAX_CELLS,
};

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gsheets-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Serializes tests that read or patch the process-wide `SHEETS_MAX_CELLS`
/// capacity knob so the growth test's patch cannot race another apply.
static SHEETS_GATE: Mutex<()> = Mutex::new(());

/// `SheetsDouble` — models RAW values and trimmed ValueRange reads.
struct SheetsDouble {
    sheets: Mutex<Map<String, Value>>,
    values: Mutex<Map<String, Value>>,
    requests: Mutex<Vec<Value>>,
}

fn parse_range(range: &str) -> (String, usize, usize) {
    let re = regex::Regex::new(r"'([^']+)'!A(\d+):([A-Z]+)(\d+)").unwrap();
    let caps = re
        .captures(range)
        .unwrap_or_else(|| panic!("bad range {range}"));
    (
        caps[1].to_owned(),
        caps[2].parse().unwrap(),
        caps[4].parse().unwrap(),
    )
}

impl SheetsDouble {
    fn new(mapping: &Map<String, Value>) -> Arc<Self> {
        let mut sheets = Map::new();
        let mut values = Map::new();
        for item in mapping.values() {
            let id = item["worksheet_id"].as_i64().unwrap();
            sheets.insert(
                id.to_string(),
                json!({
                    "sheetId": id,
                    "title": item["worksheet_title"],
                    "gridProperties": {"rowCount": 100, "columnCount": 100},
                }),
            );
            values.insert(
                item["worksheet_title"].as_str().unwrap().to_owned(),
                json!([["previous value"]]),
            );
        }
        Arc::new(SheetsDouble {
            sheets: Mutex::new(sheets),
            values: Mutex::new(values),
            requests: Mutex::new(Vec::new()),
        })
    }
}

impl Fetcher for SheetsDouble {
    fn fetch(&self, request: &Value) -> Result<Value, FetchError> {
        self.requests.lock().unwrap().push(request.clone());
        let method = request["method"].as_str().unwrap_or("");
        let url = request["url"].as_str().unwrap_or("");
        let body = &request["body"];
        if method == "GET" && !url.contains("/values/") {
            let sheets: Vec<Value> = self
                .sheets
                .lock()
                .unwrap()
                .values()
                .map(|value| json!({"properties": value}))
                .collect();
            return Ok(json!({"sheets": sheets}));
        }
        if method == "GET" {
            let range = url
                .split("/values/")
                .nth(1)
                .and_then(|s| s.split('?').next())
                .unwrap_or("");
            let (title, start, end) = parse_range(range);
            let stored = self.values.lock().unwrap();
            let all = stored
                .get(&title)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut rows: Vec<Value> = all
                .iter()
                .skip(start.saturating_sub(1))
                .take(end.saturating_sub(start) + 1)
                .cloned()
                .collect();
            for row in &mut rows {
                let cells = row.as_array_mut().unwrap();
                while cells.last().is_some_and(|c| c == "") {
                    cells.pop();
                }
            }
            while rows
                .last()
                .is_some_and(|r| r.as_array().unwrap().is_empty())
            {
                rows.pop();
            }
            return Ok(json!({"values": rows}));
        }
        if url.ends_with("/values:batchUpdate") {
            assert_eq!(body["valueInputOption"], "RAW");
            let data = &body["data"][0];
            let (title, start, end) = parse_range(data["range"].as_str().unwrap());
            let mut stored = self.values.lock().unwrap();
            let entry = stored.entry(title).or_insert_with(|| json!([]));
            let rows = entry.as_array_mut().unwrap();
            while rows.len() < end {
                rows.push(json!([]));
            }
            let incoming = data["values"].as_array().unwrap();
            for (offset, row) in incoming.iter().enumerate() {
                rows[start - 1 + offset] = row.clone();
            }
            return Ok(json!({"totalUpdatedRows": incoming.len()}));
        }
        let mut replies = Vec::new();
        let mut sheets = self.sheets.lock().unwrap();
        let mut values = self.values.lock().unwrap();
        for item in body["requests"].as_array().cloned().unwrap_or_default() {
            if let Some(add) = item.get("addSheet") {
                let next_id = sheets
                    .keys()
                    .filter_map(|k| k.parse::<i64>().ok())
                    .max()
                    .unwrap_or(0)
                    + 1;
                let mut properties = add["properties"].clone();
                properties["sheetId"] = json!(next_id);
                sheets.insert(next_id.to_string(), properties.clone());
                replies.push(json!({"addSheet": {"properties": properties}}));
            } else if let Some(update) = item.get("updateCells") {
                let id = update["range"]["sheetId"].as_i64().unwrap().to_string();
                let title = sheets[&id]["title"].as_str().unwrap().to_owned();
                values.insert(title, json!([]));
            } else if let Some(copy) = item.get("copyPaste") {
                let source = sheets[&copy["source"]["sheetId"].as_i64().unwrap().to_string()]
                    ["title"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let target = sheets[&copy["destination"]["sheetId"].as_i64().unwrap().to_string()]
                    ["title"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let copied = values.get(&source).cloned().unwrap_or(json!([]));
                values.insert(target, copied);
            } else if let Some(delete) = item.get("deleteSheet") {
                let id = delete["sheetId"].as_i64().unwrap().to_string();
                if let Some(properties) = sheets.remove(&id) {
                    if let Some(title) = properties["title"].as_str() {
                        values.remove(title);
                    }
                }
            } else if let Some(update) = item.get("updateSheetProperties") {
                let properties = &update["properties"];
                let id = properties["sheetId"].as_i64().unwrap().to_string();
                let sheet = sheets.get_mut(&id).unwrap().as_object_mut().unwrap();
                for (k, v) in properties.as_object().unwrap() {
                    sheet.insert(k.clone(), v.clone());
                }
            } else {
                panic!("unexpected request item {item}");
            }
        }
        Ok(json!({"replies": replies}))
    }
}

fn audit() -> Value {
    json!({
        "schema": "spiderhead.site-audit/1",
        "url": "https://example.test/",
        "site": {},
        "pages": [{"url": "https://example.test/", "status_code": 200}],
        "findings": [],
        "summary": {"pages_checked": 1, "findings_total": 0, "tools_run": [], "tools_failed": []},
    })
}

/// `_selected(tmp_path, columns)` — filtered one-dataset package.
fn selected(tag: &str, columns: &[&str]) -> PathBuf {
    let dir = tmp(tag);
    let package = dir.join("package");
    export_bi(
        None,
        Some(&audit()),
        &[],
        Some(&package),
        ExportLimits::default(),
    )
    .expect("export_bi");
    let selected = dir.join("selected");
    filter_package(
        &package,
        "pages",
        &selected,
        None,
        Some(&columns.iter().map(|c| c.to_string()).collect::<Vec<_>>()),
        25_000,
        8 << 20,
        DEFAULT_MAX_OUTPUT_BYTES,
    )
    .expect("filter_package");
    selected
}

fn mapping(names: &[&str]) -> Map<String, Value> {
    names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            (
                name.to_string(),
                json!({"worksheet_id": index as i64, "worksheet_title": name}),
            )
        })
        .collect()
}

/// `_client(mapping, remote)`.
fn client(worksheets: Map<String, Value>, remote: Arc<dyn Fetcher>) -> GoogleSheetsClient {
    GoogleSheetsClient::new(
        "synthetic",
        "sheet-id",
        worksheets,
        Some(Arc::new(|_scope: &str| Some("test-token".to_owned()))),
        Some(remote),
    )
}

#[test]
fn selected_sheet_preserves_types_and_nulls_with_google_shaped_readback() {
    let _gate = SHEETS_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let package = selected("readback", &["url", "status_code", "meta_description"]);
    let mapping = mapping(&["pages"]);
    let remote = SheetsDouble::new(&mapping);
    let result = apply_with_client(
        &package,
        "synthetic",
        "replace",
        &client(mapping, remote.clone()),
        true,
        false,
    )
    .expect("apply");
    assert_eq!(result["state"], "committed");
    assert_eq!(
        remote.values.lock().unwrap()["pages"],
        json!([
            ["url", "status_code", "meta_description"],
            ["https://example.test/", 200, ""],
        ])
    );
    assert!(remote.values.lock().unwrap()["pages"][1][1].is_i64());
    assert_eq!(result["rows"], json!({"pages": 1}));
    assert_eq!(result["input_rows"], json!({"pages": 1}));
}

#[test]
fn sheets_raw_values_keep_zero_false_null_and_formula_text_distinct() {
    let _gate = SHEETS_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let mapping = mapping(&["pages"]);
    let remote = SheetsDouble::new(&mapping);
    let client = client(mapping, remote.clone());
    let fields = json!([
        {"name": "count", "type": "integer"},
        {"name": "measured", "type": "boolean"},
        {"name": "value", "type": "number"},
        {"name": "text", "type": "string"},
    ]);
    let datasets = Map::from_iter([(
        "pages".to_owned(),
        json!({"row_count": 3, "fields": fields}),
    )]);
    let mut tx = client
        .begin(&BeginArgs {
            target: "synthetic",
            operation: "replace",
            schema_version: BI_SCHEMA_VERSION,
            datasets: &datasets,
            package_sha256: &"f".repeat(64),
            selected_projection: false,
        })
        .expect("begin");
    client
        .write(
            &mut tx,
            "pages",
            &[vec![
                "count".into(),
                "measured".into(),
                "value".into(),
                "text".into(),
            ]],
        )
        .expect("header write");
    client
        .write(
            &mut tx,
            "pages",
            &[
                vec![
                    "0".into(),
                    "false".into(),
                    "0.0".into(),
                    "'=SUM(1,2)".into(),
                ],
                vec!["".into(), "".into(), "".into(), "".into()],
                vec!["".into(), "".into(), "".into(), "".into()],
            ],
        )
        .expect("data write");
    client.commit(&mut tx).expect("commit");
    let values = remote.values.lock().unwrap()["pages"].clone();
    let arr = values.as_array().unwrap();
    assert_eq!(
        arr[1..].to_vec(),
        json!([
            [0, false, 0.0, "'=SUM(1,2)"],
            ["", "", "", ""],
            ["", "", "", ""],
        ])
        .as_array()
        .unwrap()
        .clone()
    );
    assert!(values[1][0].is_i64());
    assert_eq!(values[1][1], false);
}

struct LostBegin {
    inner: GoogleSheetsClient,
    uncertain_error: bool,
}

impl DestinationClient for LostBegin {
    fn destination_name(&self) -> &str {
        self.inner.destination_name()
    }
    fn authorize_target(&self, target: &str) -> bool {
        self.inner.authorize_target(target)
    }
    fn checkpoint_identity(&self) -> Option<Value> {
        self.inner.checkpoint_identity()
    }
    fn begin(&self, args: &BeginArgs<'_>) -> Result<Value, DestinationError> {
        self.inner.begin(args)?;
        if self.uncertain_error {
            Err(uncertain("lost addSheet response"))
        } else {
            Err(bi_err("lost addSheet response"))
        }
    }
}

#[test]
fn sheets_begin_recovers_after_plain_retry_and_error_status_change() {
    let _gate = SHEETS_GATE.lock().unwrap_or_else(|e| e.into_inner());
    for uncertain_error in [false, true] {
        let package = selected(
            if uncertain_error {
                "begin-unc"
            } else {
                "begin-plain"
            },
            &["url", "status_code"],
        );
        let mapping = mapping(&["pages"]);
        let remote = SheetsDouble::new(&mapping);
        let first = LostBegin {
            inner: client(mapping.clone(), remote.clone()),
            uncertain_error,
        };
        assert!(apply_with_client(&package, "synthetic", "replace", &first, true, false).is_err());
        let resumed = client(mapping.clone(), remote.clone());
        let before = remote.requests.lock().unwrap().len();
        let waiting = apply_with_client(&package, "synthetic", "replace", &resumed, true, false)
            .expect("waiting");
        assert_eq!(waiting["state"], "reconciliation_required");
        assert_eq!(remote.requests.lock().unwrap().len(), before);
        let result = apply_with_client(&package, "synthetic", "replace", &resumed, true, true)
            .expect("reconciled");
        assert_eq!(result["state"], "committed");
        let add_sheets = remote
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["body"].to_string().contains("addSheet"))
            .count();
        assert_eq!(add_sheets, 1);
    }
}

struct FakeHost {
    vars: Map<String, Value>,
}

impl Host for FakeHost {
    fn var(&self, name: &str) -> Option<String> {
        self.vars
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
    fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<std::net::SocketAddr>, String> {
        Err("offline test host".to_owned())
    }
    fn zstd_available(&self) -> bool {
        false
    }
}

#[test]
fn sheets_rejects_duplicate_targets_in_preview_and_apply() {
    let _gate = SHEETS_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reporting-pack-worksheets");
    for duplicate in ["worksheet_id", "worksheet_title"] {
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(fixture.join("manifest.json")).unwrap())
                .unwrap();
        let dataset_names: Vec<String> = manifest["datasets"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let mut mapping = mapping(&dataset_names.iter().map(String::as_str).collect::<Vec<_>>());
        // `enumerate` order in reference follows manifest order; duplicate the
        // cohorts identity onto pages.
        let dup = mapping["cohorts"][duplicate].clone();
        mapping.get_mut("pages").unwrap()[duplicate] = dup;
        let config = tmp(&format!("dup-config-{duplicate}")).join("destinations.json");
        std::fs::write(
            &config,
            json!({
                "sheets": {"targets": {"synthetic": {
                    "enabled": true,
                    "kind": "google_sheets_service_account",
                    "spreadsheet_id": "sheet-id",
                    "worksheets": mapping,
                }}}
            })
            .to_string(),
        )
        .unwrap();
        let host = FakeHost {
            vars: Map::from_iter([(
                "SEOHEAD_BI_DESTINATIONS_FILE".to_owned(),
                json!(config.to_string_lossy()),
            )]),
        };
        let err =
            destination_preview(&host, &fixture, "synthetic", "sheets", "replace").unwrap_err();
        assert!(err.to_string().contains("distinct"), "{duplicate}: {err}");
        let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
        struct Recording(Arc<Mutex<Vec<Value>>>);
        impl Fetcher for Recording {
            fn fetch(&self, request: &Value) -> Result<Value, FetchError> {
                self.0.lock().unwrap().push(request.clone());
                Ok(json!({}))
            }
        }
        let client = client(mapping, Arc::new(Recording(calls.clone())));
        let datasets = manifest["datasets"].as_object().unwrap();
        let err = client
            .begin(&BeginArgs {
                target: "synthetic",
                operation: "replace",
                schema_version: manifest["schema_version"].as_str().unwrap(),
                datasets,
                package_sha256: &"a".repeat(64),
                selected_projection: false,
            })
            .unwrap_err();
        assert!(err.to_string().contains("distinct"), "{duplicate}: {err}");
        assert!(calls.lock().unwrap().is_empty());
    }
}

#[test]
fn checkpoint_rejects_changed_target_before_claiming_commit() {
    let _gate = SHEETS_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let package = selected("checkpoint", &["url", "status_code"]);
    let mapping = mapping(&["pages"]);
    let remote = SheetsDouble::new(&mapping);
    let client = client(mapping.clone(), remote.clone());
    let result =
        apply_with_client(&package, "synthetic", "replace", &client, true, false).expect("apply");
    assert_eq!(result["state"], "committed");
    let calls = remote.requests.lock().unwrap().len();
    // Changed spreadsheet identity.
    let different = GoogleSheetsClient::new(
        "synthetic",
        "a-different-spreadsheet",
        mapping.clone(),
        Some(Arc::new(|_scope: &str| Some("test-token".to_owned()))),
        Some(remote.clone()),
    );
    let err =
        apply_with_client(&package, "synthetic", "replace", &different, true, false).unwrap_err();
    assert!(
        err.to_string().contains("another package or target"),
        "{err}"
    );
    assert_eq!(remote.requests.lock().unwrap().len(), calls);
    // Changed worksheet identity.
    let mut altered = mapping.clone();
    altered.get_mut("pages").unwrap()["worksheet_id"] = json!(123);
    let different = GoogleSheetsClient::new(
        "synthetic",
        "sheet-id",
        altered,
        Some(Arc::new(|_scope: &str| Some("test-token".to_owned()))),
        Some(remote.clone()),
    );
    let err =
        apply_with_client(&package, "synthetic", "replace", &different, true, false).unwrap_err();
    assert!(
        err.to_string().contains("another package or target"),
        "{err}"
    );
    assert_eq!(remote.requests.lock().unwrap().len(), calls);
}

struct QuotaFetch {
    remote: Arc<SheetsDouble>,
    throttled: Arc<AtomicBool>,
}

impl Fetcher for QuotaFetch {
    fn fetch(&self, request: &Value) -> Result<Value, FetchError> {
        if self.throttled.load(Ordering::Relaxed)
            && request["url"]
                .as_str()
                .unwrap_or("")
                .ends_with("/values:batchUpdate")
        {
            return Ok(json!({"error": {"code": 429}}));
        }
        self.remote.fetch(request)
    }
}

#[test]
fn quota_exhaustion_keeps_stage_and_reconciles_after_quota_recovers() {
    let _gate = SHEETS_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let package = selected("quota", &["url", "status_code"]);
    let mapping = mapping(&["pages"]);
    let remote = SheetsDouble::new(&mapping);
    let throttled = Arc::new(AtomicBool::new(true));
    let fetcher = Arc::new(QuotaFetch {
        remote: remote.clone(),
        throttled: throttled.clone(),
    });
    let sleeps = Arc::new(Mutex::new(Vec::<f64>::new()));
    let record = sleeps.clone();
    let mut client = GoogleSheetsClient::new(
        "synthetic",
        "sheet-id",
        mapping,
        Some(Arc::new(|_scope: &str| Some("test-token".to_owned()))),
        Some(fetcher),
    );
    client.rest_mut().sleeper = Arc::new(move |s| record.lock().unwrap().push(s));
    client.rest_mut().jitter = Arc::new(|| 0.5);
    let first =
        apply_with_client(&package, "synthetic", "replace", &client, true, false).expect("first");
    assert_eq!(first["state"], "reconciliation_required");
    assert_eq!(*sleeps.lock().unwrap(), vec![1.5, 2.5, 4.5]);
    assert_eq!(
        remote.values.lock().unwrap()["pages"],
        json!([["previous value"]])
    );
    assert_eq!(first["pending_rows"], json!({"pages": 1}));
    assert_eq!(first["input_rows"], json!({"pages": 1}));
    let calls = remote.requests.lock().unwrap().len();
    let waiting =
        apply_with_client(&package, "synthetic", "replace", &client, true, false).expect("waiting");
    assert_eq!(waiting["state"], "reconciliation_required");
    assert_eq!(remote.requests.lock().unwrap().len(), calls);
    throttled.store(false, Ordering::Relaxed);
    let result = apply_with_client(&package, "synthetic", "replace", &client, true, true)
        .expect("reconciled");
    assert_eq!(result["state"], "committed");
    assert_eq!(result["pending_rows"], json!({"pages": 0}));
    let add_sheets = remote
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["body"].to_string().contains("addSheet"))
        .count();
    assert_eq!(add_sheets, 1);
}

#[test]
fn google_retry_after_is_respected_within_the_wait_budget() {
    // Numeric Retry-After, end to end through the retryable request path.
    let calls = Arc::new(AtomicU64::new(0));
    let counted = calls.clone();
    struct Flaky(Arc<AtomicU64>);
    impl Fetcher for Flaky {
        fn fetch(&self, _request: &Value) -> Result<Value, FetchError> {
            if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
                return Err(FetchError::Http {
                    code: 429,
                    retry_after: Some("10".to_owned()),
                });
            }
            Ok(json!({"recovered": true}))
        }
    }
    let mut rest = GoogleRestClient::new(
        "scope",
        Some(Arc::new(|_scope: &str| Some("test-token".to_owned()))),
        Some(Arc::new(Flaky(counted))),
    );
    let sleeps = Arc::new(Mutex::new(Vec::<f64>::new()));
    let record = sleeps.clone();
    rest.sleeper = Arc::new(move |s| record.lock().unwrap().push(s));
    rest.jitter = Arc::new(|| 0.5);
    let result = rest
        .request(
            "GET",
            "https://sheets.googleapis.com/synthetic",
            None,
            None,
            true,
        )
        .expect("request");
    assert_eq!(result, json!({"recovered": true}));
    assert_eq!(*sleeps.lock().unwrap(), vec![10.0]);
    // HTTP-date Retry-After: reference pins time.time to 1445412470; the public
    // backoff seam takes the clock explicitly.
    let sleeps = Arc::new(Mutex::new(Vec::<f64>::new()));
    let record = sleeps.clone();
    let sleeper: Sleeper = Arc::new(move |s| record.lock().unwrap().push(s));
    let jitter: Jitter = Arc::new(|| 0.5);
    google_backoff(
        0,
        Some("Wed, 21 Oct 2015 07:28:00 GMT"),
        &sleeper,
        &jitter,
        1_445_412_470.0,
    )
    .expect("backoff");
    assert_eq!(*sleeps.lock().unwrap(), vec![10.0]);
}

#[test]
fn long_retry_after_requires_later_reconciliation_without_sleeping() {
    let calls = Arc::new(AtomicU64::new(0));
    let counted = calls.clone();
    struct Always429(Arc<AtomicU64>);
    impl Fetcher for Always429 {
        fn fetch(&self, request: &Value) -> Result<Value, FetchError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            let _ = request;
            Err(FetchError::Http {
                code: 429,
                retry_after: Some("120".to_owned()),
            })
        }
    }
    let mut rest = GoogleRestClient::new(
        "scope",
        Some(Arc::new(|_scope: &str| Some("test-token".to_owned()))),
        Some(Arc::new(Always429(counted))),
    );
    rest.sleeper = Arc::new(|_s| panic!("wait must stay bounded"));
    let err = rest
        .request(
            "GET",
            "https://sheets.googleapis.com/synthetic",
            None,
            None,
            true,
        )
        .unwrap_err();
    assert!(err.to_string().contains("60-second wait budget"), "{err}");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn destination_rejects_dataset_contract_drift_before_transport() {
    let _gate = SHEETS_GATE.lock().unwrap_or_else(|e| e.into_inner());
    for (index, (field, value)) in [
        ("schema_version", json!("spiderhead.bi.pages.v999")),
        ("grain", json!("wrong grain")),
        ("primary_key", json!(["wrong_key"])),
    ]
    .iter()
    .enumerate()
    {
        let package = selected(&format!("drift-{index}"), &["url"]);
        let manifest_path = package
            .parent()
            .unwrap()
            .join("package")
            .join("manifest.json");
        let mut manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest["datasets"]["pages"][field] = value.clone();
        std::fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();
        let err = sheets_plan(&package.parent().unwrap().join("package"), 10_000_000).unwrap_err();
        assert!(
            err.to_string().contains("version, grain or key"),
            "{field}: {err}"
        );
    }
}

#[test]
fn sheet_target_growth_is_budgeted_before_staging_and_applied_atomically() {
    let _gate = SHEETS_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let package = selected("growth", &["url", "status_code"]);
    let mapping = mapping(&["pages"]);
    let remote = SheetsDouble::new(&mapping);
    remote.sheets.lock().unwrap().get_mut("0").unwrap()["gridProperties"] =
        json!({"rowCount": 1, "columnCount": 1});
    let client = client(mapping, remote.clone());
    SHEETS_MAX_CELLS.store(7, Ordering::Relaxed);
    let result = apply_with_client(&package, "synthetic", "replace", &client, true, false);
    SHEETS_MAX_CELLS.store(10_000_000, Ordering::Relaxed);
    let err = result.unwrap_err();
    assert!(err.to_string().contains("staging cells"), "{err}");
    assert!(remote
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|r| r["method"] == "GET"));
    assert_eq!(
        remote.values.lock().unwrap()["pages"],
        json!([["previous value"]])
    );
    SHEETS_MAX_CELLS.store(8, Ordering::Relaxed);
    let result = apply_with_client(&package, "synthetic", "replace", &client, true, true);
    SHEETS_MAX_CELLS.store(10_000_000, Ordering::Relaxed);
    let result = result.expect("apply");
    assert_eq!(result["state"], "committed");
    assert_eq!(
        remote.sheets.lock().unwrap()["0"]["gridProperties"],
        json!({"rowCount": 2, "columnCount": 2})
    );
    let requests = remote.requests.lock().unwrap();
    let publication = requests.last().unwrap()["body"]["requests"]
        .as_array()
        .unwrap();
    assert!(publication[0].get("updateSheetProperties").is_some());
    assert!(publication[1].get("updateCells").is_some());
    assert!(publication[2].get("copyPaste").is_some());
}

#[test]
fn empty_header_failure_does_not_overwrite_the_existing_target() {
    let _gate = SHEETS_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let package = selected("bad-header", &["url", "status_code"]);
    let mapping = mapping(&["pages"]);
    let remote = SheetsDouble::new(&mapping);
    struct BadReadback(Arc<SheetsDouble>);
    impl Fetcher for BadReadback {
        fn fetch(&self, request: &Value) -> Result<Value, FetchError> {
            if request["method"] == "GET"
                && request["url"].as_str().unwrap_or("").contains("/values/")
            {
                return Ok(json!({"values": [["wrong", "header"]]}));
            }
            self.0.fetch(request)
        }
    }
    let client = client(mapping, Arc::new(BadReadback(remote.clone())));
    let result =
        apply_with_client(&package, "synthetic", "replace", &client, true, false).expect("apply");
    assert_eq!(result["state"], "failed");
    assert_eq!(
        remote.values.lock().unwrap()["pages"],
        json!([["previous value"]])
    );
    let total: i64 = ["rows", "skipped_rows", "failed_rows", "pending_rows"]
        .iter()
        .map(|key| result[key]["pages"].as_i64().unwrap_or(0))
        .sum();
    assert_eq!(result["input_rows"]["pages"], total);
}

struct UncertainCommit {
    remote: Arc<SheetsDouble>,
    lost: AtomicBool,
    apply_remote: bool,
}

impl Fetcher for UncertainCommit {
    fn fetch(&self, request: &Value) -> Result<Value, FetchError> {
        if request["body"].to_string().contains("copyPaste")
            && self.lost.swap(false, Ordering::Relaxed)
        {
            if self.apply_remote {
                self.remote.fetch(request)?;
            }
            return Err(FetchError::Timeout);
        }
        self.remote.fetch(request)
    }
}

#[test]
fn atomic_commit_timeout_requires_reconciliation_without_claiming_success() {
    let _gate = SHEETS_GATE.lock().unwrap_or_else(|e| e.into_inner());
    for applied in [true, false] {
        let package = selected(
            if applied { "commit-yes" } else { "commit-no" },
            &["url", "status_code"],
        );
        let mapping = mapping(&["pages"]);
        let remote = SheetsDouble::new(&mapping);
        let fetcher = Arc::new(UncertainCommit {
            remote: remote.clone(),
            lost: AtomicBool::new(true),
            apply_remote: applied,
        });
        let client = client(mapping.clone(), fetcher);
        let result = apply_with_client(&package, "synthetic", "replace", &client, true, false)
            .expect("apply");
        assert_eq!(result["state"], "reconciliation_required");
        assert!(result.get("row_conservation").is_none());
        let expected = if applied {
            json!([["url", "status_code"], ["https://example.test/", 200]])
        } else {
            json!([["previous value"]])
        };
        assert_eq!(remote.values.lock().unwrap()["pages"], expected);
        let result = apply_with_client(&package, "synthetic", "replace", &client, true, true)
            .expect("reconcile");
        eprintln!("applied={applied} result={result}");
        assert_eq!(
            result["state"],
            if applied {
                "reconciliation_required"
            } else {
                "committed"
            }
        );
        let copy_pastes = remote
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["body"].to_string().contains("copyPaste"))
            .count();
        assert_eq!(copy_pastes, 1);
    }
}

struct MutatingClient {
    inner: GoogleSheetsClient,
    package: PathBuf,
    change_partition: bool,
}

impl DestinationClient for MutatingClient {
    fn destination_name(&self) -> &str {
        self.inner.destination_name()
    }
    fn authorize_target(&self, target: &str) -> bool {
        self.inner.authorize_target(target)
    }
    fn checkpoint_identity(&self) -> Option<Value> {
        self.inner.checkpoint_identity()
    }
    fn begin(&self, args: &BeginArgs<'_>) -> Result<Value, DestinationError> {
        let transaction = self.inner.begin(args)?;
        if self.change_partition {
            let manifest: Value = serde_json::from_str(
                &std::fs::read_to_string(self.package.join("manifest.json")).unwrap(),
            )
            .unwrap();
            let part = manifest["partitions"][0]["path"]
                .as_str()
                .unwrap()
                .to_owned();
            let path = self.package.join(part);
            let mut rows: Vec<Vec<String>> = csv::ReaderBuilder::new()
                .has_headers(false)
                .from_path(&path)
                .unwrap()
                .records()
                .map(|r| r.unwrap().iter().map(str::to_owned).collect())
                .collect();
            rows[1][1] = "201".to_owned();
            let mut writer = csv::Writer::from_path(&path).unwrap();
            for row in &rows {
                writer.write_record(row).unwrap();
            }
            drop(writer);
        } else {
            let path = self.package.join("manifest.json");
            let content = std::fs::read_to_string(&path).unwrap();
            std::fs::write(&path, format!("{content}\n")).unwrap();
        }
        Ok(transaction)
    }
    fn write(
        &self,
        transaction: &mut Value,
        dataset: &str,
        rows: &[Vec<String>],
    ) -> Result<(), DestinationError> {
        self.inner.write(transaction, dataset, rows)
    }
    fn commit(&self, transaction: &mut Value) -> Result<Value, DestinationError> {
        self.inner.commit(transaction)
    }
}

#[test]
fn source_mutation_during_staging_never_publishes_against_the_old_manifest() {
    let _gate = SHEETS_GATE.lock().unwrap_or_else(|e| e.into_inner());
    for change_partition in [true, false] {
        let package = selected(
            if change_partition {
                "mutate-part"
            } else {
                "mutate-manifest"
            },
            &["url", "status_code"],
        );
        let mapping = mapping(&["pages"]);
        let remote = SheetsDouble::new(&mapping);
        let mutating = MutatingClient {
            inner: client(mapping, remote.clone()),
            package: package.clone(),
            change_partition,
        };
        let result = apply_with_client(&package, "synthetic", "replace", &mutating, true, false)
            .expect("apply");
        assert_eq!(result["state"], "failed");
        assert!(result.get("row_conservation").is_none());
        assert_eq!(
            remote.values.lock().unwrap()["pages"],
            json!([["previous value"]])
        );
        assert!(!remote
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["body"].to_string().contains("copyPaste")));
    }
}
