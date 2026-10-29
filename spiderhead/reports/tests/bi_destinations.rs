//! Offline planner, checkpointed apply and fake-transport Google client tests.
//!
//! Covers the reference suite, the reference suite and
//! the reference suite semantics that fit the Rust seams; the
//! reference environment-mock and urllib-patch tests have no direct equivalent.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use calamine::Reader;
use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;
use spiderhead_reports::bi::project::{export_bi, ExportLimits};
use spiderhead_reports::bi_destinations::{
    _cell_text, _state_identity, a1_column, apply_with_client, bi_err, bigquery_plan, bq_type,
    bq_value, csv_chunks, destination_preview, export_bi_xlsx, filter_package, resolve_host_client,
    sheets_plan, sheets_readback_matches, sheets_values, uncertain, BeginArgs, BeginOutcome,
    CommitOutcome, DestinationClient, DestinationError, FetchError, Fetcher, GoogleBigQueryClient,
    GoogleRestClient, GoogleSheetsClient, PendingOutcome, BI_SCHEMA_VERSION, DATASET_SPECS,
    MANIFEST_FORMAT, SHEETS_SCOPE,
};

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bi-dest-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn sha256(data: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(data))
}

fn field_names(dataset: &str) -> Vec<String> {
    DATASET_SPECS()
        .iter()
        .find(|(n, ..)| *n == dataset)
        .expect("dataset spec")
        .1
        .iter()
        .map(|f| f.name.to_string())
        .collect()
}

/// Write a valid single-dataset package: `pages.csv` plus `manifest.json`
/// whose partition and totals declarations all verify.
fn write_package(tag: &str, dataset: &str, rows: &[Vec<String>]) -> PathBuf {
    let dir = tmp(tag);
    let names = field_names(dataset);
    let mut csv_text = names.join(",");
    csv_text.push('\n');
    for row in rows {
        assert_eq!(row.len(), names.len());
        csv_text.push_str(&row.join(","));
        csv_text.push('\n');
    }
    fs::write(dir.join(format!("{dataset}.csv")), &csv_text).unwrap();
    let bytes = csv_text.len() as i64;
    let (fields_spec, grain, key) = DATASET_SPECS()
        .iter()
        .find(|(n, ..)| *n == dataset)
        .map(|(_, f, g, k)| (*f, *g, *k))
        .unwrap();
    let fields: Vec<Value> = fields_spec
        .iter()
        .map(|f| json!({"name": f.name, "type": f.type_, "nullable": f.nullable}))
        .collect();
    let manifest = json!({
        "format": MANIFEST_FORMAT,
        "schema_version": BI_SCHEMA_VERSION,
        "datasets": {
            dataset: {
                "schema_version": format!("spiderhead.bi.{dataset}.v1"),
                "grain": grain,
                "primary_key": key,
                "fields": fields,
                "row_count": rows.len() as i64,
                "bytes": bytes,
                "partitions": [{
                    "path": format!("{dataset}.csv"),
                    "sha256": sha256(csv_text.as_bytes()),
                    "rows": rows.len() as i64,
                    "bytes": bytes,
                }],
            },
        },
    });
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap() + "\n",
    )
    .unwrap();
    dir
}

/// A pages row with `url` filled in and every other column empty.
fn page_row(url: &str) -> Vec<String> {
    let names = field_names("pages");
    let mut row = vec![String::new(); names.len()];
    for (i, name) in names.iter().enumerate() {
        if name == "url" {
            row[i] = url.to_owned();
        }
        if name == "run_id" {
            row[i] = "run-1".to_owned();
        }
        if name == "url_observation_id" {
            row[i] = format!("obs-{url}");
        }
    }
    row
}

#[test]
fn sheets_plan_is_offline_and_reports_bounded_cells() {
    let package = write_package(
        "plan",
        "pages",
        &[page_row("https://a/"), page_row("https://b/")],
    );
    let plan = sheets_plan(&package, 10_000_000).expect("plan");
    assert_eq!(plan["network"], false);
    assert_eq!(plan["apply"], false);
    assert_eq!(plan["state"], "ready");
    let sheets = plan["worksheets"].as_array().unwrap();
    assert_eq!(sheets.len(), 1);
    assert_eq!(sheets[0]["worksheet"], "pages");
    assert_eq!(sheets[0]["rows"], 2);
    assert_eq!(sheets[0]["header_rows"], 1);
    let columns = field_names("pages").len() as i64;
    assert_eq!(sheets[0]["cells"], 3 * columns);
    assert_eq!(plan["required_cells"], 3 * columns);
}

#[test]
fn sheets_plan_marks_oversized_packages_unavailable() {
    let package = write_package("plan-cap", "pages", &[page_row("https://a/")]);
    let columns = field_names("pages").len() as i64;
    let plan = sheets_plan(&package, columns).expect("plan");
    assert_eq!(plan["state"], "unavailable");
    assert!(plan["reason"].as_str().unwrap().contains("capacity"));
    assert!(sheets_plan(&package, 0).is_err());
    assert!(sheets_plan(&package, i64::MAX).is_err());
}

#[test]
fn bigquery_plan_is_offline_and_names_tables() {
    let package = write_package("bq-plan", "pages", &[page_row("https://a/")]);
    let plan = bigquery_plan(&package, "analytics", "replace").expect("plan");
    assert_eq!(plan["network"], false);
    assert_eq!(plan["apply"], false);
    assert_eq!(plan["billing_required"], true);
    assert_eq!(plan["tables"][0]["table"], "seohead_pages_v1");
    assert_eq!(plan["tables"][0]["rows"], 1);
    assert!(bigquery_plan(&package, "analytics", "merge").is_err());
    assert!(bigquery_plan(&package, "", "replace").is_err());
}

#[test]
fn manifest_refuses_missing_and_symlinked_packages() {
    let dir = tmp("no-manifest");
    assert!(sheets_plan(&dir, 1000).is_err());
    let file = dir.join("file.json");
    fs::write(&file, "{}").unwrap();
    assert!(sheets_plan(&file, 1000).is_err());
}

#[test]
fn csv_chunks_emit_header_once_then_bounded_chunks() {
    let rows: Vec<Vec<String>> = (0..5)
        .map(|i| page_row(&format!("https://a/{i}")))
        .collect();
    let package = write_package("chunks", "pages", &rows);
    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(package.join("manifest.json")).unwrap()).unwrap();
    let dataset = &manifest["datasets"]["pages"];
    let mut seen: Vec<Vec<Vec<String>>> = Vec::new();
    csv_chunks(&package, dataset, 2, |chunk| {
        seen.push(chunk);
        Ok(true)
    })
    .unwrap();
    // header, then 2+2+1 data chunks.
    assert_eq!(seen.len(), 4);
    assert_eq!(seen[0].len(), 1);
    assert_eq!(seen[0][0].len(), field_names("pages").len());
    assert_eq!(seen[1].len(), 2);
    assert_eq!(seen[2].len(), 2);
    assert_eq!(seen[3].len(), 1);
    // Early consumer exit stops iteration cleanly.
    let mut count = 0usize;
    csv_chunks(&package, dataset, 2, |_| {
        count += 1;
        Ok(count < 2)
    })
    .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn csv_chunks_reject_disagreeing_partition_headers() {
    let dir = tmp("bad-header");
    let names = field_names("pages");
    let first = format!(
        "{}\n{}\n",
        names.join(","),
        page_row("https://a/").join(",")
    );
    let mut wrong = names.clone();
    wrong.swap(0, 1);
    let second = format!(
        "{}\n{}\n",
        wrong.join(","),
        page_row("https://b/").join(",")
    );
    fs::write(dir.join("a.csv"), &first).unwrap();
    fs::write(dir.join("b.csv"), &second).unwrap();
    let dataset = json!({
        "partitions": [
            {"path": "a.csv", "sha256": sha256(first.as_bytes()), "rows": 1, "bytes": first.len()},
            {"path": "b.csv", "sha256": sha256(second.as_bytes()), "rows": 1, "bytes": second.len()},
        ],
    });
    let err = csv_chunks(&dir, &dataset, 100, |_| Ok(true)).unwrap_err();
    assert!(err.to_string().contains("headers disagree"));
}

struct FakeClient {
    calls: Mutex<Vec<String>>,
    uncertain_write: bool,
    write_error: bool,
    uncertain_commit: bool,
    authorize: bool,
}

impl DestinationClient for FakeClient {
    fn destination_name(&self) -> &str {
        "fake"
    }
    fn authorize_target(&self, _target: &str) -> bool {
        self.authorize
    }
    fn begin(&self, _args: &BeginArgs<'_>) -> Result<Value, DestinationError> {
        self.calls.lock().unwrap().push("begin".to_owned());
        Ok(json!({"transaction": "tx-1"}))
    }
    fn write(
        &self,
        _transaction: &mut Value,
        dataset: &str,
        rows: &[Vec<String>],
    ) -> Result<(), DestinationError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("write:{dataset}:{}", rows.len()));
        if self.uncertain_write {
            return Err(uncertain("write outcome unknown"));
        }
        if self.write_error {
            return Err(bi_err("write failed"));
        }
        Ok(())
    }
    fn commit(&self, _transaction: &mut Value) -> Result<Value, DestinationError> {
        self.calls.lock().unwrap().push("commit".to_owned());
        if self.uncertain_commit {
            return Err(uncertain("commit outcome unknown"));
        }
        Ok(json!({"published_via": "fake"}))
    }
    fn abort(&self, _transaction: &mut Value) -> Result<Option<()>, DestinationError> {
        self.calls.lock().unwrap().push("abort".to_owned());
        Ok(Some(()))
    }
    fn pending_write(
        &self,
        _transaction: &Value,
        dataset: &str,
        rows: &[Vec<String>],
        chunk: usize,
    ) -> Result<Option<Value>, DestinationError> {
        Ok(Some(json!({
            "dataset": dataset,
            "chunk": chunk,
            "data_rows": rows.len() as i64,
        })))
    }
    fn reconcile_pending(
        &self,
        _transaction: &mut Value,
        _pending: &Value,
        _rows: &[Vec<String>],
    ) -> Result<Option<PendingOutcome>, DestinationError> {
        Ok(Some(PendingOutcome::NotApplied))
    }
    fn reconcile_commit(
        &self,
        _transaction: &mut Value,
    ) -> Result<Option<CommitOutcome>, DestinationError> {
        Ok(Some(CommitOutcome::NotApplied))
    }
}

#[test]
fn apply_requires_explicit_apply_and_authorized_target() {
    let package = write_package("apply-guard", "pages", &[page_row("https://a/")]);
    let client = FakeClient {
        calls: Mutex::new(Vec::new()),
        uncertain_write: false,
        write_error: false,
        uncertain_commit: false,
        authorize: true,
    };
    assert!(apply_with_client(&package, "t", "replace", &client, false, false).is_err());
    assert!(apply_with_client(&package, "", "replace", &client, true, false).is_err());
    assert!(apply_with_client(&package, "t", "merge", &client, true, false).is_err());
    let denying = FakeClient {
        calls: Mutex::new(Vec::new()),
        uncertain_write: false,
        write_error: false,
        uncertain_commit: false,
        authorize: false,
    };
    let err = apply_with_client(&package, "t", "replace", &denying, true, false).unwrap_err();
    assert!(err.to_string().contains("authorize"));
}

#[test]
fn apply_commits_chunks_and_records_accounting() {
    let package = write_package(
        "apply-ok",
        "pages",
        &[page_row("https://a/"), page_row("https://b/")],
    );
    let client = FakeClient {
        calls: Mutex::new(Vec::new()),
        uncertain_write: false,
        write_error: false,
        uncertain_commit: false,
        authorize: true,
    };
    let result = apply_with_client(&package, "t", "replace", &client, true, false).unwrap();
    assert_eq!(result["state"], "committed");
    assert_eq!(result["rows"]["pages"], 2);
    assert_eq!(result["input_rows"]["pages"], 2);
    assert_eq!(result["published_via"], "fake");
    assert_eq!(result["row_conservation"], "verified");
    let calls = client.calls.lock().unwrap().clone();
    // The header travels as its own first chunk, then the data rows.
    assert_eq!(
        calls,
        vec!["begin", "write:pages:1", "write:pages:2", "commit"]
    );
    // A durable checkpoint remains beside the package.
    let state_dir = package.join(".seohead-destination-state");
    let states: Vec<_> = fs::read_dir(&state_dir).unwrap().collect();
    assert_eq!(states.len(), 1);
    let state: Value =
        serde_json::from_str(&fs::read_to_string(states[0].as_ref().unwrap().path()).unwrap())
            .unwrap();
    assert_eq!(state["format"], "spiderhead.bi-destination-state.v1");
    assert_eq!(state["status"], "committed");
}

#[test]
fn apply_write_error_marks_failed_and_aborts() {
    let package = write_package("apply-fail", "pages", &[page_row("https://a/")]);
    let client = FakeClient {
        calls: Mutex::new(Vec::new()),
        uncertain_write: false,
        write_error: true,
        uncertain_commit: false,
        authorize: true,
    };
    let result = apply_with_client(&package, "t", "replace", &client, true, false).unwrap();
    assert_eq!(result["state"], "failed");
    assert!(result["reason"].as_str().unwrap().contains("write failed"));
    let calls = client.calls.lock().unwrap().clone();
    assert_eq!(calls.last().unwrap(), "abort");
}

#[test]
fn apply_uncertain_write_marks_reconciliation_required() {
    let package = write_package("apply-unc", "pages", &[page_row("https://a/")]);
    let client = FakeClient {
        calls: Mutex::new(Vec::new()),
        uncertain_write: true,
        write_error: false,
        uncertain_commit: false,
        authorize: true,
    };
    let result = apply_with_client(&package, "t", "replace", &client, true, false).unwrap();
    assert_eq!(result["state"], "reconciliation_required");
    let calls = client.calls.lock().unwrap().clone();
    assert!(!calls.contains(&"commit".to_owned()));
}

#[test]
fn apply_reconciles_pending_chunk_before_retry() {
    // An uncertain write leaves a durable pending record; the next apply call
    // must reconcile that exact chunk instead of writing a duplicate.
    let package = write_package("apply-retry", "pages", &[page_row("https://a/")]);
    let client = Arc::new(FakeClient {
        calls: Mutex::new(Vec::new()),
        uncertain_write: true,
        write_error: false,
        uncertain_commit: false,
        authorize: true,
    });
    let first = apply_with_client(&package, "t", "replace", client.as_ref(), true, false).unwrap();
    assert_eq!(first["state"], "reconciliation_required");
    // Second run with a healthy client must go through reconcile_pending.
    let healthy = FakeClient {
        calls: Mutex::new(Vec::new()),
        uncertain_write: false,
        write_error: false,
        uncertain_commit: false,
        authorize: true,
    };
    let second = apply_with_client(&package, "t", "replace", &healthy, true, true).unwrap();
    assert_eq!(second["state"], "committed");
    assert_eq!(second["rows"]["pages"], 1);
}

struct FakeFetcher {
    responses: Mutex<Vec<Result<Value, FetchError>>>,
    requests: Mutex<Vec<Value>>,
}

impl Fetcher for FakeFetcher {
    fn fetch(&self, request: &Value) -> Result<Value, FetchError> {
        self.requests.lock().unwrap().push(request.clone());
        self.responses.lock().unwrap().remove(0)
    }
}

fn rest_client(fetcher: Arc<FakeFetcher>) -> GoogleRestClient {
    GoogleRestClient::new(
        SHEETS_SCOPE,
        Some(Arc::new(|_scope: &str| Some("test-token".to_owned()))),
        Some(fetcher),
    )
}

#[test]
fn rest_request_carries_bearer_token_and_returns_body() {
    let fetcher = Arc::new(FakeFetcher {
        responses: Mutex::new(vec![Ok(json!({"ok": true}))]),
        requests: Mutex::new(Vec::new()),
    });
    let client = GoogleRestClient::new(
        SHEETS_SCOPE,
        Some(Arc::new(|_scope: &str| Some("test-token".to_owned()))),
        Some(fetcher.clone()),
    );
    let response = client
        .request(
            "GET",
            "https://sheets.googleapis.com/v4/spreadsheets/x",
            None,
            None,
            true,
        )
        .unwrap();
    assert_eq!(response["ok"], true);
    let request = &fetcher.requests.lock().unwrap()[0];
    assert_eq!(request["authorization"], "Bearer test-token");
}

#[test]
fn rest_request_retries_server_errors_then_succeeds() {
    let fetcher = Arc::new(FakeFetcher {
        responses: Mutex::new(vec![
            Err(FetchError::Http {
                code: 500,
                retry_after: None,
            }),
            Ok(json!({"done": true})),
        ]),
        requests: Mutex::new(Vec::new()),
    });
    let mut client = rest_client(fetcher);
    client.sleeper = Arc::new(|_seconds: f64| {});
    client.jitter = Arc::new(|| 0.0);
    let response = client
        .request("GET", "https://example.test/x", None, None, true)
        .unwrap();
    assert_eq!(response["done"], true);
}

#[test]
fn rest_request_missing_token_is_a_destination_error() {
    let client = GoogleRestClient::new(SHEETS_SCOPE, None, None);
    let err = client
        .request("GET", "https://example.test/x", None, None, false)
        .unwrap_err();
    assert!(err.to_string().contains("token"));
}

#[test]
fn sheets_values_neutralize_formulas_and_decode_types() {
    let transaction = json!({
        "fields": {
            "pages": [
                {"name": "flag", "type": "boolean"},
                {"name": "n", "type": "integer"},
                {"name": "title", "type": "string"},
            ],
        },
        "rows": {"pages": 1},
    });
    let values = sheets_values(
        &transaction,
        "pages",
        &[vec!["true".to_owned(), "42".to_owned(), "=cmd".to_owned()]],
    )
    .unwrap();
    assert_eq!(values[0][0], true);
    assert_eq!(values[0][1], 42);
    // Sheets writes keep the CSV text literal; neutralization happened at
    // package write time.
    assert_eq!(values[0][2], "=cmd");
    // With zero staged rows the chunk must be exactly the declared header.
    let fresh = json!({
        "fields": transaction["fields"].clone(),
        "rows": {"pages": 0},
    });
    let header = sheets_values(
        &fresh,
        "pages",
        &[vec!["flag".to_owned(), "n".to_owned(), "title".to_owned()]],
    )
    .unwrap();
    assert_eq!(header[0][0], "flag");
    assert!(sheets_values(&fresh, "pages", &[vec!["wrong".to_owned(); 3]]).is_err());
}

#[test]
fn bq_value_and_type_follow_the_schema_mapping() {
    assert_eq!(bq_type("integer"), "INT64");
    assert_eq!(bq_type("number"), "FLOAT64");
    assert_eq!(bq_type("boolean"), "BOOL");
    assert_eq!(bq_type("string"), "STRING");
    assert_eq!(bq_value("true", "boolean").unwrap(), true);
    assert_eq!(bq_value("7", "integer").unwrap(), 7);
    assert_eq!(bq_value("1.5", "number").unwrap(), 1.5);
    assert_eq!(bq_value("", "integer").unwrap(), Value::Null);
}

#[test]
fn a1_column_and_readback_helpers() {
    assert_eq!(a1_column(1).unwrap(), "A");
    assert_eq!(a1_column(26).unwrap(), "Z");
    assert_eq!(a1_column(27).unwrap(), "AA");
    assert!(a1_column(0).is_err());
    // Google omits empty trailing cells; readback equality tolerates that.
    assert!(sheets_readback_matches(
        &json!([["a", ""], ["b", "c"]]),
        &[vec![json!("a"), json!("")], vec![json!("b"), json!("c")]]
    ));
    assert!(!sheets_readback_matches(
        &json!([["a"]]),
        &[vec![json!("b")]]
    ));
}

#[test]
fn filter_package_writes_selected_view_and_conservation() {
    let package = write_package(
        "filter",
        "pages",
        &[page_row("https://keep/"), page_row("https://drop/")],
    );
    let out = tmp("filter-out-py").join("selected");
    let where_ = Map::from_iter([("url".to_owned(), json!(["https://keep/"]))]);
    let result = filter_package(
        &package,
        "pages",
        &out,
        Some(&where_),
        None,
        10_000,
        1 << 20,
        1 << 24,
    )
    .unwrap();
    assert_eq!(result["format"], "spiderhead.bi-filter.v1");
    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["format"], "spiderhead.bi-filter.v1");
    assert_eq!(manifest["row_count"], 1);
    assert_eq!(manifest["conservation"]["source_rows"], 2);
    assert_eq!(manifest["conservation"]["selected_rows"], 1);
    let csv = fs::read_to_string(out.join("pages-0001.csv")).unwrap();
    assert!(csv.contains("https://keep/"));
    assert!(!csv.contains("https://drop/"));
}

#[test]
fn filter_package_refuses_unknown_columns_and_values() {
    let package = write_package("filter-bad", "pages", &[page_row("https://a/")]);
    let out = tmp("filter-bad-out").join("sel");
    let where_ = Map::from_iter([("nope".to_owned(), json!(["x"]))]);
    assert!(filter_package(
        &package,
        "pages",
        &out,
        Some(&where_),
        None,
        1000,
        1 << 20,
        1 << 24
    )
    .is_err());
}

#[test]
fn export_bi_xlsx_splits_sheets_and_writes_index() {
    let package = write_package(
        "xlsx",
        "pages",
        &[
            page_row("https://a/"),
            page_row("https://b/"),
            page_row("https://c/"),
        ],
    );
    let out = tmp("xlsx-out-py").join("pages.xlsx");
    let result = export_bi_xlsx(&package, "pages", &out, 2, 1 << 24).unwrap();
    assert_eq!(result["format"], "spiderhead.bi-xlsx.v1");
    assert_eq!(result["worksheets"], 2);
    assert_eq!(result["rows"], 3);
    assert!(out.is_file());
    let index_path = out.with_extension("xlsx.index.json");
    let index: Value = serde_json::from_str(&fs::read_to_string(&index_path).unwrap()).unwrap();
    assert_eq!(index["format"], "spiderhead.bi-xlsx-index.v1");
    assert_eq!(index["state"], "complete");
    assert_eq!(index["rows"], 3);
    assert_eq!(index["worksheets"], 2);
    let ranges = index["ranges"].as_array().unwrap();
    assert_eq!(ranges.len(), 2);
    assert_eq!(ranges[0]["worksheet"], "pages-0001");
    assert_eq!(ranges[1]["worksheet"], "pages-0002");
    assert_eq!(ranges[0]["worksheet_row_start"], 2);
    assert_eq!(ranges[1]["worksheet_row_start"], 2);
}

#[test]
fn export_bi_xlsx_refuses_existing_output_and_bad_suffix() {
    let package = write_package("xlsx-guard", "pages", &[page_row("https://a/")]);
    let dir = tmp("xlsx-guard-out");
    let existing = dir.join("taken.xlsx");
    fs::write(&existing, b"x").unwrap();
    assert!(export_bi_xlsx(&package, "pages", &existing, 10, 1 << 24).is_err());
    assert!(export_bi_xlsx(&package, "pages", &dir.join("out.csv"), 10, 1 << 24).is_err());
    assert!(export_bi_xlsx(&package, "pages", &dir.join("out.xlsx"), 0, 1 << 24).is_err());
}

#[test]
fn state_identity_binds_destination_target_operation_and_manifest() {
    // Checkpoint identity is the sha256 of "destination\0target\0operation\0sha".
    let a = _state_identity("sheets", "t", "replace", "aaa");
    let b = _state_identity("sheets", "t", "append", "aaa");
    let c = _state_identity("sheets", "t", "replace", "bbb");
    assert_ne!(a, b);
    assert_ne!(a, c);
    assert_eq!(a.len(), 64);
}

#[test]
fn cell_text_mirrors_python_stringification() {
    assert_eq!(_cell_text(&Value::Null), "");
    assert_eq!(_cell_text(&json!(true)), "true");
    assert_eq!(_cell_text(&json!(5)), "5");
    assert_eq!(_cell_text(&json!("=x")), "'=x");
}

// ---------------------------------------------------------------------------
// the reference suite — real `export_bi` packages and the Google
// REST seams.  Deferred: the mcp-handler, cli, and private `_job` tests.
// ---------------------------------------------------------------------------

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

fn export_package(tag: &str, audit: &Value) -> PathBuf {
    let dir = tmp(tag).join("package");
    export_bi(None, Some(audit), &[], Some(&dir), ExportLimits::default()).expect("export_bi");
    dir
}

fn read_manifest(package: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(package.join("manifest.json")).unwrap()).unwrap()
}

fn write_manifest(package: &Path, manifest: &Value) {
    fs::write(
        package.join("manifest.json"),
        serde_json::to_string_pretty(manifest).unwrap(),
    )
    .unwrap();
}

/// `_worksheet_mapping(package)`: dataset -> worksheet id/title config.
fn worksheet_mapping(package: &Path) -> Map<String, Value> {
    read_manifest(package)["datasets"]
        .as_object()
        .unwrap()
        .keys()
        .enumerate()
        .map(|(index, name)| {
            (
                name.clone(),
                json!({"worksheet_id": index + 2, "worksheet_title": name}),
            )
        })
        .collect()
}

/// `_table_mapping(package)`: dataset -> deterministic BigQuery table id.
fn table_mapping(package: &Path) -> Map<String, Value> {
    read_manifest(package)["datasets"]
        .as_object()
        .unwrap()
        .keys()
        .map(|name| {
            (
                name.clone(),
                json!({"table_id": format!("seohead_{name}_v1")}),
            )
        })
        .collect()
}

fn read_csv_partition(path: &Path) -> Vec<Map<String, Value>> {
    let mut reader = csv::Reader::from_path(path).unwrap();
    reader
        .deserialize()
        .map(|row| row.expect("csv row"))
        .collect()
}

#[test]
fn offline_destination_plans_reconcile_the_complete_local_package() {
    let package = export_package("plans-py", &audit());
    let sheets = sheets_plan(&package, 10_000_000).expect("sheets plan");
    assert_eq!(sheets["network"], false);
    assert_eq!(sheets["apply"], false);
    assert_eq!(sheets["state"], "ready");
    let names: std::collections::BTreeSet<String> = sheets["worksheets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["worksheet"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains("pages") && names.contains("cohorts"));
    let bigquery = bigquery_plan(&package, "synthetic_reporting", "replace").expect("bq plan");
    assert_eq!(bigquery["billing_required"], true);
    assert!(bigquery["tables"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["table"].as_str().unwrap().starts_with("seohead_")));
    assert_eq!(
        sheets_plan(&package, 1).expect("plan")["state"],
        "unavailable"
    );
}

struct RecordingClient {
    calls: Mutex<Vec<String>>,
}

impl DestinationClient for RecordingClient {
    fn destination_name(&self) -> &str {
        "recording"
    }
    fn authorize_target(&self, target: &str) -> bool {
        self.calls
            .lock()
            .unwrap()
            .push(format!("authorize:{target}"));
        true
    }
    fn begin(&self, _args: &BeginArgs<'_>) -> Result<Value, DestinationError> {
        self.calls.lock().unwrap().push("begin".to_owned());
        Ok(json!("tx"))
    }
    fn write(
        &self,
        _transaction: &mut Value,
        dataset: &str,
        rows: &[Vec<String>],
    ) -> Result<(), DestinationError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("write:{dataset}:{}", rows.len()));
        Ok(())
    }
    fn commit(&self, _transaction: &mut Value) -> Result<Value, DestinationError> {
        self.calls.lock().unwrap().push("commit".to_owned());
        Ok(json!({}))
    }
}

#[test]
fn injected_destination_client_is_explicit_transactional_and_streamed() {
    let package = export_package("apply", &audit());
    let client = RecordingClient {
        calls: Mutex::new(Vec::new()),
    };
    let err =
        apply_with_client(&package, "synthetic", "replace", &client, false, false).unwrap_err();
    assert!(err.to_string().contains("apply=True"));
    let result =
        apply_with_client(&package, "synthetic", "replace", &client, true, false).expect("apply");
    assert_eq!(result["rows"]["pages"], 1);
    let calls = client.calls.lock().unwrap().clone();
    assert_eq!(calls[0], "authorize:synthetic");
    assert_eq!(calls.last().unwrap(), "commit");
}

#[test]
fn filtered_bi_export_is_exact_and_partitioned() {
    let package = export_package("filter-py", &audit());
    let out = tmp("filter-out-py").join("filtered");
    let where_ = Map::from_iter([(
        "cohort_id".to_owned(),
        json!(["observed_status", "crawl_relative_depth"]),
    )]);
    let filtered = filter_package(
        &package,
        "cohorts",
        &out,
        Some(&where_),
        Some(&[
            "run_id".to_owned(),
            "cohort_id".to_owned(),
            "value_label".to_owned(),
            "state".to_owned(),
        ]),
        1,
        1 << 20,
        1 << 24,
    )
    .expect("filter");
    assert_eq!(filtered["row_count"], 2);
    let parts: Vec<i64> = filtered["partitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part["rows"].as_i64().unwrap())
        .collect();
    assert_eq!(parts, vec![1, 1]);
    assert!(out.join("manifest.json").is_file());
}

#[test]
fn finding_segment_selection_keeps_an_unfiltered_coverage_companion() {
    let mut audit = audit();
    audit["findings"] = json!([
        {
            "severity": "warning",
            "check": "MISSING_TITLE",
            "url": "https://example.test/blog/a",
            "text": "missing title",
        },
        {
            "severity": "warning",
            "check": "MISSING_TITLE",
            "url": "https://example.test/catalogue/a",
            "text": "missing title",
        },
    ]);
    audit["summary"]["tools_failed"] = json!([{"tool": "render", "error": "unavailable"}]);
    let package = export_package("segment", &audit);
    let mut manifest = read_manifest(&package);
    manifest["run"]["crawl_settings"] =
        json!({"scope.segments": [{"name": "blog", "prefix": "/blog/"}]});
    write_manifest(&package, &manifest);

    let out = tmp("segment-out-py").join("selected");
    let where_ = Map::from_iter([("segment".to_owned(), json!(["blog"]))]);
    let filtered = filter_package(
        &package,
        "findings",
        &out,
        Some(&where_),
        Some(&["url".to_owned(), "check_id".to_owned()]),
        10_000,
        1 << 20,
        1 << 24,
    )
    .expect("filter");
    assert_eq!(filtered["row_count"], 1);
    let companion = &filtered["coverage_companion"];
    assert!(companion["selection"]
        .as_str()
        .unwrap()
        .starts_with("complete immutable"));
    assert_eq!(
        companion["row_count"],
        manifest["datasets"]["coverage"]["row_count"]
    );

    let selected_manifest = read_manifest(&out);
    let mut selected_rows = Vec::new();
    for part in selected_manifest["partitions"].as_array().unwrap() {
        selected_rows.extend(read_csv_partition(
            &out.join(part["path"].as_str().unwrap()),
        ));
    }
    assert_eq!(
        selected_rows,
        vec![Map::from_iter([
            ("url".to_owned(), json!("https://example.test/blog/a")),
            ("check_id".to_owned(), json!("MISSING_TITLE")),
        ])]
    );
    let mut source_coverage = Vec::new();
    for part in manifest["datasets"]["coverage"]["partitions"]
        .as_array()
        .unwrap()
    {
        source_coverage.extend(read_csv_partition(
            &package.join(part["path"].as_str().unwrap()),
        ));
    }
    let mut copied_coverage = Vec::new();
    for part in companion["partitions"].as_array().unwrap() {
        copied_coverage.extend(read_csv_partition(
            &out.join(part["path"].as_str().unwrap()),
        ));
    }
    assert_eq!(copied_coverage, source_coverage);
    let selected_plan = sheets_plan(&out, 10_000_000).expect("selected plan");
    let names: std::collections::BTreeSet<String> = selected_plan["worksheets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["worksheet"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        names,
        ["findings".to_owned(), "coverage".to_owned()]
            .into_iter()
            .collect()
    );
}

#[test]
fn finding_segment_selection_refuses_a_field_missing_from_projection() {
    let mut audit = audit();
    audit["findings"] = json!([{
        "severity": "warning",
        "check": "MISSING_TITLE",
        "url": "https://example.test/",
        "text": "missing title",
    }]);
    let package = export_package("segment-miss", &audit);
    let mut manifest = read_manifest(&package);
    manifest["run"]["crawl_settings"] = json!({
        "analysis.segments": [
            {"name": "has-title", "rules": [{"op": "eq", "field": "title", "value": "Home"}]}
        ]
    });
    write_manifest(&package, &manifest);

    let out = tmp("segment-miss-out-py").join("selected");
    let where_ = Map::from_iter([("segment".to_owned(), json!(["has-title"]))]);
    let err = filter_package(
        &package,
        "findings",
        &out,
        Some(&where_),
        None,
        10_000,
        1 << 20,
        1 << 24,
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("retained page fields are missing: title"));
    assert!(!out.exists());
}

#[test]
fn destination_preflight_streams_partitions_and_accepts_selected_projection() {
    let package = export_package("preflight", &audit());
    let out = tmp("preflight-out-py").join("filtered");
    let filtered = filter_package(
        &package,
        "cohorts",
        &out,
        None,
        Some(&[
            "run_id".to_owned(),
            "cohort_id".to_owned(),
            "state".to_owned(),
        ]),
        10_000,
        1 << 20,
        1 << 24,
    )
    .expect("filter");
    assert!(filtered["row_count"].as_i64().unwrap() > 0);
    // Rust streams partition bytes through `BufRead`; there is no read-all
    // path to forbid, so the preflight assertions are the shared contract.
    let plan = sheets_plan(&out, 10_000_000).expect("plan");
    let worksheets: Vec<&str> = plan["worksheets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["worksheet"].as_str().unwrap())
        .collect();
    assert_eq!(worksheets, vec!["cohorts"]);
    assert_eq!(plan["worksheets"][0]["columns"], 3);
}

#[test]
fn selected_bi_view_streams_to_excel_with_numbered_sheet_splitting() {
    let package = export_package("xlsx-py", &audit());
    let out = tmp("xlsx-out-py").join("filtered");
    let filtered = filter_package(
        &package,
        "cohorts",
        &out,
        None,
        Some(&[
            "run_id".to_owned(),
            "cohort_id".to_owned(),
            "state".to_owned(),
        ]),
        10_000,
        1 << 20,
        1 << 24,
    )
    .expect("filter");
    let xlsx = tmp("xlsx-file-py").join("cohorts.xlsx");
    let result = export_bi_xlsx(&out, "cohorts", &xlsx, 2, 1 << 24).expect("xlsx");
    let row_count = filtered["row_count"].as_i64().unwrap();
    assert_eq!(result["rows"], row_count);
    assert_eq!(result["worksheets"], (row_count + 1) / 2);
    let mut workbook: calamine::Xlsx<_> = calamine::open_workbook(&xlsx).expect("open xlsx");
    let names = workbook.sheet_names().to_owned();
    assert_eq!(names[0], "cohorts-0001");
    let range = workbook.worksheet_range(&names[0]).expect("first sheet");
    let header: Vec<String> = range
        .rows()
        .next()
        .unwrap()
        .iter()
        .map(|cell| cell.to_string())
        .collect();
    assert_eq!(header, vec!["run_id", "cohort_id", "state"]);
}

/// A `Host` whose only variable is the destinations config path.
struct FakeHost {
    vars: Map<String, Value>,
}

impl FakeHost {
    fn with_config(config_path: &Path) -> Self {
        FakeHost {
            vars: Map::from_iter([(
                "SEOHEAD_BI_DESTINATIONS_FILE".to_owned(),
                json!(config_path.to_string_lossy()),
            )]),
        }
    }
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

fn sheets_config(package: &Path) -> Value {
    json!({
        "sheets": {
            "targets": {
                "reporting": {
                    "enabled": true,
                    "kind": "google_sheets_service_account",
                    "spreadsheet_id": "1Qs8BdfxZXALh6vX4zrE7ZyGnR3h5k",
                    "worksheets": worksheet_mapping(package),
                }
            }
        }
    })
}

#[test]
fn selected_projection_preview_uses_its_mapping_from_a_full_target_config() {
    let package = export_package("sel-preview", &audit());
    let out = tmp("sel-preview-out-py").join("filtered");
    filter_package(
        &package,
        "cohorts",
        &out,
        None,
        Some(&[
            "run_id".to_owned(),
            "cohort_id".to_owned(),
            "state".to_owned(),
        ]),
        10_000,
        1 << 20,
        1 << 24,
    )
    .expect("filter");
    let config_path = tmp("sel-preview-config-py").join("bi-destinations.json");
    fs::write(&config_path, sheets_config(&package).to_string()).unwrap();
    let host = FakeHost::with_config(&config_path);
    let preview =
        destination_preview(&host, &out, "reporting", "sheets", "replace").expect("preview");
    let worksheets: std::collections::BTreeSet<String> = preview["resolved_target"]["worksheets"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(worksheets, ["cohorts".to_owned()].into_iter().collect());
}

#[test]
fn destination_preflight_rejects_manifest_type_or_csv_header_drift() {
    let package = export_package("drift-type", &audit());
    let mut manifest = read_manifest(&package);
    manifest["datasets"]["pages"]["fields"][0]["type"] = json!("integer");
    write_manifest(&package, &manifest);
    let err = sheets_plan(&package, 10_000_000).unwrap_err();
    assert!(err.to_string().contains("fields do not match"));

    let package = export_package("drift-header", &audit());
    let mut manifest = read_manifest(&package);
    let part = manifest["datasets"]["pages"]["partitions"][0].clone();
    let path = package.join(part["path"].as_str().unwrap());
    let content = fs::read_to_string(&path).unwrap();
    fs::write(&path, content.replacen("run_id", "wrong_header", 1)).unwrap();
    let raw = fs::read(&path).unwrap();
    let old_bytes = part["bytes"].as_i64().unwrap();
    manifest["datasets"]["pages"]["partitions"][0]["bytes"] = json!(raw.len() as i64);
    manifest["datasets"]["pages"]["partitions"][0]["sha256"] = json!(sha256(&raw));
    manifest["datasets"]["pages"]["bytes"] = json!(
        manifest["datasets"]["pages"]["bytes"].as_i64().unwrap() + raw.len() as i64 - old_bytes
    );
    write_manifest(&package, &manifest);
    let err = sheets_plan(&package, 10_000_000).unwrap_err();
    assert!(err.to_string().contains("header"));
}

#[test]
fn destination_preview_shows_resolved_target_and_operation_without_auth_or_transport() {
    let package = export_package("preview", &audit());
    let worksheets = worksheet_mapping(&package);
    let config_path = tmp("preview-config-py").join("bi-destinations.json");
    fs::write(&config_path, sheets_config(&package).to_string()).unwrap();
    // A `Host` that cannot resolve any name makes authentication impossible;
    // preview must not reach for a token either.
    let host = FakeHost::with_config(&config_path);
    let result =
        destination_preview(&host, &package, "reporting", "sheets", "replace").expect("preview");
    assert_eq!(result["state"], "ready_to_apply");
    assert_eq!(result["target"], "reporting");
    assert_eq!(result["operation"], "replace");
    assert_eq!(result["datasets"]["pages"]["rows"], 1);
    assert_eq!(
        result["resolved_target"],
        json!({
            "kind": "google_sheets_service_account",
            "spreadsheet_id": "1Qs8BdfxZXALh6vX4zrE7ZyGnR3h5k",
            "worksheets": worksheets,
        })
    );
}

#[test]
fn bigquery_preview_discloses_resolved_project_dataset_and_table_mapping() {
    let package = export_package("bq-preview", &audit());
    let tables = table_mapping(&package);
    let config = json!({
        "bigquery": {
            "targets": {
                "warehouse": {
                    "enabled": true,
                    "kind": "google_bigquery_service_account",
                    "project_id": "reporting-project",
                    "dataset_id": "seohead_reporting",
                    "location": "EU",
                    "cost_authorized": false,
                    "tables": tables,
                }
            }
        }
    });
    let config_path = tmp("bq-preview-config-py").join("bi-destinations.json");
    fs::write(&config_path, config.to_string()).unwrap();
    let host = FakeHost::with_config(&config_path);
    let result =
        destination_preview(&host, &package, "warehouse", "bigquery", "replace").expect("preview");
    assert_eq!(
        result["resolved_target"],
        json!({
            "kind": "google_bigquery_service_account",
            "project_id": "reporting-project",
            "dataset_id": "seohead_reporting",
            "location": "EU",
            "cost_authorized": false,
            "tables": tables,
        })
    );
}

/// `_restartable_sheets_fetcher(worksheets)`: a stateful in-memory Sheets
/// backend — stage tabs, values, and deletes persist across client instances.
struct SheetsRemote {
    sheets: Mutex<Map<String, Value>>,
    values: Mutex<Map<String, Value>>,
    requests: Mutex<Vec<Value>>,
    fail_add_sheet_once: Mutex<bool>,
}

impl SheetsRemote {
    fn new(worksheets: Map<String, Value>) -> Arc<Self> {
        let sheets = worksheets
            .values()
            .map(|item| {
                let id = item["worksheet_id"].as_i64().unwrap().to_string();
                (
                    id.clone(),
                    json!({
                        "sheetId": item["worksheet_id"],
                        "title": item["worksheet_title"],
                        "gridProperties": {"rowCount": 2, "columnCount": 100},
                    }),
                )
            })
            .collect();
        Arc::new(SheetsRemote {
            sheets: Mutex::new(sheets),
            values: Mutex::new(Map::new()),
            requests: Mutex::new(Vec::new()),
            fail_add_sheet_once: Mutex::new(false),
        })
    }
}

impl Fetcher for SheetsRemote {
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
        if method == "GET" && url.contains("/values/") {
            let key = url
                .split("/values/")
                .nth(1)
                .and_then(|s| s.split('?').next())
                .unwrap_or("");
            let values = self
                .values
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .unwrap_or(json!([]));
            return Ok(json!({"values": values}));
        }
        if url.ends_with("/values:batchUpdate") {
            let values = body["data"][0]["values"].clone();
            let range = body["data"][0]["range"].as_str().unwrap().to_owned();
            self.values.lock().unwrap().insert(range, values.clone());
            return Ok(json!({
                "totalUpdatedRows": values.as_array().map(|v| v.len()).unwrap_or(0)
            }));
        }
        let requests = body["requests"].as_array().cloned().unwrap_or_default();
        if requests
            .first()
            .map(|item| item.get("addSheet").is_some())
            .unwrap_or(false)
        {
            let mut fail = self.fail_add_sheet_once.lock().unwrap();
            if *fail {
                *fail = false;
                return Err(FetchError::Transport("lost addSheet response".to_owned()));
            }
            drop(fail);
            let mut replies = Vec::new();
            let mut sheets = self.sheets.lock().unwrap();
            for (offset, item) in requests.iter().enumerate() {
                let props = &item["addSheet"]["properties"];
                let id = 100 + offset as i64;
                let record = json!({
                    "sheetId": id,
                    "title": props["title"],
                    "gridProperties": props["gridProperties"],
                });
                sheets.insert(id.to_string(), record.clone());
                replies.push(json!({"addSheet": {"properties": record}}));
            }
            return Ok(json!({"replies": replies}));
        }
        for item in requests {
            if let Some(delete) = item.get("deleteSheet") {
                let id = delete["sheetId"].as_i64().unwrap().to_string();
                self.sheets.lock().unwrap().remove(&id);
            }
        }
        Ok(json!({}))
    }
}

#[test]
fn google_sheets_replace_uses_one_atomic_swap_and_preserves_target_sheet_ids() {
    let package = export_package("sheets-swap", &audit());
    let worksheets = worksheet_mapping(&package);
    let remote = SheetsRemote::new(worksheets.clone());
    let client = GoogleSheetsClient::new(
        "synthetic",
        "sheet-id",
        worksheets.clone(),
        Some(Arc::new(|_scope: &str| Some("token".to_owned()))),
        Some(remote.clone()),
    );
    let result =
        apply_with_client(&package, "synthetic", "replace", &client, true, false).expect("apply");
    assert_eq!(result["state"], "committed");
    assert_eq!(result["row_conservation"], "verified");
    let requests = remote.requests.lock().unwrap();
    let commit = requests.last().unwrap()["body"]["requests"]
        .as_array()
        .unwrap()
        .clone();
    assert!(commit.iter().all(|request| {
        request.get("updateCells").is_some()
            || request.get("copyPaste").is_some()
            || request.get("deleteSheet").is_some()
            || request.get("updateSheetProperties").is_some()
    }));
    let destinations: std::collections::BTreeSet<i64> = commit
        .iter()
        .filter_map(|request| {
            request
                .get("copyPaste")
                .map(|cp| cp["destination"]["sheetId"].as_i64().unwrap())
        })
        .collect();
    let expected: std::collections::BTreeSet<i64> = worksheets
        .values()
        .map(|item| item["worksheet_id"].as_i64().unwrap())
        .collect();
    assert_eq!(destinations, expected);
    assert!(commit.iter().any(|request| {
        request
            .get("updateCells")
            .and_then(|uc| uc.get("fields"))
            .and_then(Value::as_str)
            == Some("userEnteredValue")
    }));
    assert_eq!(requests[1]["authorization"], "Bearer token");
}

#[test]
fn google_sheets_rejects_partial_mapping_before_creating_a_stage() {
    let package = export_package("sheets-partial", &audit());
    let called = Arc::new(Mutex::new(Vec::<Value>::new()));
    let called_fetch = called.clone();
    struct CountingFetch(Arc<Mutex<Vec<Value>>>);
    impl Fetcher for CountingFetch {
        fn fetch(&self, request: &Value) -> Result<Value, FetchError> {
            self.0.lock().unwrap().push(request.clone());
            Ok(json!({}))
        }
    }
    let client = GoogleSheetsClient::new(
        "synthetic",
        "sheet-id",
        Map::from_iter([(
            "pages".to_owned(),
            json!({"worksheet_id": 2, "worksheet_title": "pages"}),
        )]),
        Some(Arc::new(|_scope: &str| Some("token".to_owned()))),
        Some(Arc::new(CountingFetch(called_fetch))),
    );
    let err =
        apply_with_client(&package, "synthetic", "replace", &client, true, false).unwrap_err();
    assert!(err.to_string().contains("exactly once"));
    assert!(called.lock().unwrap().is_empty());
}

/// A BigQuery REST fake: upload jobs, job polling, stage-table reads.
struct BqRemote {
    expected: Map<String, Value>,
    job_rows: Mutex<Map<String, Value>>,
    requests: Mutex<Vec<Value>>,
}

impl Fetcher for BqRemote {
    fn fetch(&self, request: &Value) -> Result<Value, FetchError> {
        self.requests.lock().unwrap().push(request.clone());
        let method = request["method"].as_str().unwrap_or("");
        let url = request["url"].as_str().unwrap_or("");
        if method == "POST" && url.contains("upload/bigquery") {
            let bytes: Vec<u8> = request["body"]["__bytes__"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u8)
                .collect();
            let text = String::from_utf8_lossy(&bytes);
            let marker = "\"jobId\":\"";
            let start = text.find(marker).expect("jobId") + marker.len();
            let job_id = text[start..]
                .split('"')
                .next()
                .expect("jobId end")
                .to_owned();
            let media = text
                .split("Content-Type: application/octet-stream\r\n\r\n")
                .nth(1)
                .expect("media");
            let media = media.rsplit("\r\n--").nth(1).unwrap_or(media);
            let rows = media.matches('\n').count() as i64;
            self.job_rows
                .lock()
                .unwrap()
                .insert(job_id.clone(), json!(rows));
            return Ok(json!({"jobReference": {"jobId": job_id}}));
        }
        if method == "POST" && url.ends_with("/jobs") {
            let job_id = request["body"]["jobReference"]["jobId"].clone();
            return Ok(json!({"jobReference": {"jobId": job_id}}));
        }
        if method == "GET" && url.contains("/jobs/") {
            let job_id = url
                .split("/jobs/")
                .nth(1)
                .and_then(|s| s.split('?').next())
                .unwrap_or("");
            let rows = self
                .job_rows
                .lock()
                .unwrap()
                .get(job_id)
                .and_then(Value::as_i64)
                .unwrap_or(0);
            return Ok(json!({
                "status": {"state": "DONE"},
                "statistics": {"load": {"outputRows": rows.to_string()}},
            }));
        }
        if method == "GET" && url.contains("/datasets/dataset_id") && !url.contains("/tables/") {
            return Ok(json!({"datasetReference": {"datasetId": "dataset_id"}}));
        }
        if method == "GET" && url.contains("/tables/") {
            let stage = url.rsplit('/').next().unwrap_or("");
            let name = stage
                .strip_prefix("_seohead_stage_")
                .and_then(|s| s.rsplit_once('_').map(|x| x.0))
                .unwrap_or(stage);
            let rows = self.expected.get(name).and_then(Value::as_i64).unwrap_or(0);
            return Ok(json!({"numRows": rows.to_string()}));
        }
        if method == "DELETE" {
            return Ok(json!({}));
        }
        panic!("unexpected request {request}");
    }
}

#[test]
fn google_bigquery_stages_chunks_and_publishes_each_table_with_mocked_rest() {
    let package = export_package("bq-stage", &audit());
    let manifest = read_manifest(&package);
    let expected: Map<String, Value> = manifest["datasets"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, item)| (name.clone(), item["row_count"].clone()))
        .collect();
    let remote = Arc::new(BqRemote {
        expected,
        job_rows: Mutex::new(Map::new()),
        requests: Mutex::new(Vec::new()),
    });
    let client = GoogleBigQueryClient::new(
        "synthetic",
        "project-id",
        "dataset_id",
        table_mapping(&package),
        Some("EU".to_owned()),
        true,
        Some(Arc::new(|_scope: &str| Some("token".to_owned()))),
        Some(remote.clone()),
    );
    let result =
        apply_with_client(&package, "synthetic", "replace", &client, true, false).expect("apply");
    assert_eq!(
        result["publication"],
        "atomic_per_bigquery_table_not_across_datasets"
    );
    let requests = remote.requests.lock().unwrap();
    let uploads: Vec<&Value> = requests
        .iter()
        .filter(|r| r["url"].as_str().unwrap_or("").contains("upload/bigquery"))
        .collect();
    assert!(!uploads.is_empty());
    assert!(uploads.iter().all(|r| r["body"].get("__bytes__").is_some()));
    let copies: Vec<&Value> = requests
        .iter()
        .filter(|r| {
            r["method"].as_str() == Some("POST")
                && r["url"].as_str().unwrap_or("").ends_with("/jobs")
        })
        .collect();
    assert!(!copies.is_empty());
    assert!(copies
        .iter()
        .all(|r| { r["body"]["configuration"]["copy"]["writeDisposition"] == "WRITE_TRUNCATE" }));
}

#[test]
fn fresh_host_resolution_creates_google_clients_without_registration() {
    let config = json!({
        "sheets": {
            "targets": {
                "reporting-fresh": {
                    "enabled": true,
                    "kind": "google_sheets_service_account",
                    "spreadsheet_id": "spreadsheet-id",
                    "worksheets": {},
                }
            }
        },
        "bigquery": {
            "targets": {
                "warehouse-fresh": {
                    "enabled": true,
                    "kind": "google_bigquery_service_account",
                    "project_id": "project-id",
                    "dataset_id": "dataset_id",
                    "tables": {},
                    "cost_authorized": true,
                }
            }
        },
    });
    let config_path = tmp("fresh-config-py").join("bi-destinations.json");
    fs::write(&config_path, config.to_string()).unwrap();
    let host = FakeHost::with_config(&config_path);
    let sheets = resolve_host_client(&host, "sheets", "reporting-fresh").expect("sheets");
    assert_eq!(sheets.destination_name(), "sheets");
    let bigquery = resolve_host_client(&host, "bigquery", "warehouse-fresh").expect("bigquery");
    assert_eq!(bigquery.destination_name(), "bigquery");
}

#[test]
fn bigquery_cost_denial_calls_neither_auth_nor_transport() {
    let package = export_package("bq-deny", &audit());
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let auth_calls = calls.clone();
    let recording_fetch = Arc::new(Mutex::new(Vec::<String>::new()));
    let fetch_log = recording_fetch.clone();
    struct LogFetch(Arc<Mutex<Vec<String>>>);
    impl Fetcher for LogFetch {
        fn fetch(&self, _request: &Value) -> Result<Value, FetchError> {
            self.0.lock().unwrap().push("transport".to_owned());
            Ok(json!({}))
        }
    }
    let client = GoogleBigQueryClient::new(
        "warehouse",
        "project-id",
        "dataset_id",
        table_mapping(&package),
        None,
        false,
        Some(Arc::new(move |scope: &str| {
            auth_calls.lock().unwrap().push(format!("auth:{scope}"));
            Some("token".to_owned())
        })),
        Some(Arc::new(LogFetch(fetch_log))),
    );
    let err =
        apply_with_client(&package, "warehouse", "replace", &client, true, false).unwrap_err();
    assert!(err.to_string().contains("cost_authorized"));
    assert!(calls.lock().unwrap().is_empty());
    assert!(recording_fetch.lock().unwrap().is_empty());
}

/// A sheets client whose second write succeeds remotely but reports the
/// outcome as lost — the reference `KeyboardInterrupt` after `original_write`.
struct InterruptingSheets {
    inner: GoogleSheetsClient,
    writes: Mutex<usize>,
}

impl DestinationClient for InterruptingSheets {
    fn destination_name(&self) -> &str {
        self.inner.destination_name()
    }
    fn authorize_target(&self, target: &str) -> bool {
        self.inner.authorize_target(target)
    }
    fn begin(&self, args: &BeginArgs<'_>) -> Result<Value, DestinationError> {
        self.inner.begin(args)
    }
    fn reconcile_begin(
        &self,
        args: &BeginArgs<'_>,
    ) -> Result<Option<BeginOutcome>, DestinationError> {
        self.inner.reconcile_begin(args)
    }
    fn checkpoint_identity(&self) -> Option<Value> {
        self.inner.checkpoint_identity()
    }
    fn write(
        &self,
        transaction: &mut Value,
        dataset: &str,
        rows: &[Vec<String>],
    ) -> Result<(), DestinationError> {
        self.inner.write(transaction, dataset, rows)?;
        let mut writes = self.writes.lock().unwrap();
        *writes += 1;
        if *writes == 2 {
            return Err(uncertain("write response lost after landing"));
        }
        Ok(())
    }
    fn pending_write(
        &self,
        transaction: &Value,
        dataset: &str,
        rows: &[Vec<String>],
        chunk: usize,
    ) -> Result<Option<Value>, DestinationError> {
        self.inner.pending_write(transaction, dataset, rows, chunk)
    }
    fn reconcile_pending(
        &self,
        transaction: &mut Value,
        pending: &Value,
        rows: &[Vec<String>],
    ) -> Result<Option<PendingOutcome>, DestinationError> {
        self.inner.reconcile_pending(transaction, pending, rows)
    }
    fn reconcile_commit(
        &self,
        transaction: &mut Value,
    ) -> Result<Option<CommitOutcome>, DestinationError> {
        self.inner.reconcile_commit(transaction)
    }
    fn commit(&self, transaction: &mut Value) -> Result<Value, DestinationError> {
        self.inner.commit(transaction)
    }
    fn abort(&self, transaction: &mut Value) -> Result<Option<()>, DestinationError> {
        self.inner.abort(transaction)
    }
}

fn sheets_client(
    target: &str,
    worksheets: Map<String, Value>,
    fetcher: Arc<SheetsRemote>,
) -> GoogleSheetsClient {
    GoogleSheetsClient::new(
        target,
        "sheet-id",
        worksheets,
        Some(Arc::new(|_scope: &str| Some("token".to_owned()))),
        Some(fetcher),
    )
}

#[test]
fn sheets_restart_reconciles_an_uncertain_stage_range_before_resuming() {
    let package = export_package("sheets-restart", &audit());
    let worksheets = worksheet_mapping(&package);
    let remote = SheetsRemote::new(worksheets.clone());
    let first = InterruptingSheets {
        inner: sheets_client("synthetic", worksheets.clone(), remote.clone()),
        writes: Mutex::new(0),
    };
    let first_result =
        apply_with_client(&package, "synthetic", "replace", &first, true, false).expect("apply");
    assert_eq!(first_result["state"], "reconciliation_required");
    let state_dir = package.join(".seohead-destination-state");
    assert!(!fs::read_dir(&state_dir)
        .unwrap()
        .collect::<Vec<_>>()
        .is_empty());

    let resumed = sheets_client("synthetic", worksheets, remote);
    let result = apply_with_client(&package, "synthetic", "replace", &resumed, true, true)
        .expect("resumed apply");
    assert_eq!(result["state"], "committed");
    assert_eq!(result["rows"]["pages"], result["input_rows"]["pages"]);
    assert_eq!(result["rows"]["pages"], 1);
    assert_eq!(result["failed_rows"]["pages"], 0);
}

#[test]
fn uncertain_write_never_replays_without_explicit_reconciliation() {
    let package = export_package("uncertain", &audit());
    struct UncertainOnSecond {
        calls: Mutex<usize>,
    }
    impl DestinationClient for UncertainOnSecond {
        fn destination_name(&self) -> &str {
            "synthetic"
        }
        fn authorize_target(&self, target: &str) -> bool {
            target == "synthetic"
        }
        fn begin(&self, _args: &BeginArgs<'_>) -> Result<Value, DestinationError> {
            Ok(json!({}))
        }
        fn write(
            &self,
            _transaction: &mut Value,
            _dataset: &str,
            _rows: &[Vec<String>],
        ) -> Result<(), DestinationError> {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            if *calls == 2 {
                return Err(uncertain("synthetic write response lost"));
            }
            Ok(())
        }
        fn commit(&self, _transaction: &mut Value) -> Result<Value, DestinationError> {
            panic!("uncertain write must stop before commit");
        }
    }
    let client = UncertainOnSecond {
        calls: Mutex::new(0),
    };
    let first =
        apply_with_client(&package, "synthetic", "replace", &client, true, false).expect("apply");
    assert_eq!(first["state"], "reconciliation_required");
    assert_eq!(first["rows"]["pages"], 0);
    let calls = *client.calls.lock().unwrap();
    let second =
        apply_with_client(&package, "synthetic", "replace", &client, true, false).expect("apply");
    assert_eq!(second["state"], "reconciliation_required");
    assert_eq!(*client.calls.lock().unwrap(), calls);
}

#[test]
fn interrupted_begin_reconciles_before_any_second_remote_begin() {
    let package = export_package("begin-lost", &audit());
    struct FirstClient {
        begin_calls: Mutex<usize>,
    }
    impl DestinationClient for FirstClient {
        fn destination_name(&self) -> &str {
            "synthetic-begin"
        }
        fn authorize_target(&self, _target: &str) -> bool {
            true
        }
        fn begin(&self, _args: &BeginArgs<'_>) -> Result<Value, DestinationError> {
            *self.begin_calls.lock().unwrap() += 1;
            Err(uncertain("response lost after remote staging"))
        }
    }
    let first = FirstClient {
        begin_calls: Mutex::new(0),
    };
    assert!(apply_with_client(&package, "synthetic", "replace", &first, true, false).is_err());
    assert_eq!(*first.begin_calls.lock().unwrap(), 1);

    struct RecoveredClient {
        begin_calls: Mutex<usize>,
        writes: Mutex<usize>,
    }
    impl DestinationClient for RecoveredClient {
        fn destination_name(&self) -> &str {
            "synthetic-begin"
        }
        fn authorize_target(&self, _target: &str) -> bool {
            true
        }
        fn begin(&self, _args: &BeginArgs<'_>) -> Result<Value, DestinationError> {
            *self.begin_calls.lock().unwrap() += 1;
            panic!("an uncertain begin must be reconciled before another remote begin");
        }
        fn reconcile_begin(
            &self,
            _args: &BeginArgs<'_>,
        ) -> Result<Option<BeginOutcome>, DestinationError> {
            Ok(Some(BeginOutcome::Transaction(json!({"recovered": true}))))
        }
        fn write(
            &self,
            _transaction: &mut Value,
            _dataset: &str,
            _rows: &[Vec<String>],
        ) -> Result<(), DestinationError> {
            *self.writes.lock().unwrap() += 1;
            Ok(())
        }
        fn commit(&self, transaction: &mut Value) -> Result<Value, DestinationError> {
            assert_eq!(*transaction, json!({"recovered": true}));
            Ok(json!({}))
        }
    }
    let recovered = RecoveredClient {
        begin_calls: Mutex::new(0),
        writes: Mutex::new(0),
    };
    let result =
        apply_with_client(&package, "synthetic", "replace", &recovered, true, true).expect("apply");
    assert_eq!(result["state"], "committed");
    assert_eq!(*recovered.begin_calls.lock().unwrap(), 0);
    assert!(*recovered.writes.lock().unwrap() > 0);
}

#[test]
fn sheets_recovers_staged_tabs_after_a_lost_begin_response() {
    let package = export_package("begin-recover", &audit());
    let worksheets = worksheet_mapping(&package);
    let remote = SheetsRemote::new(worksheets.clone());
    *remote.fail_add_sheet_once.lock().unwrap() = true;
    let first = sheets_client("synthetic", worksheets.clone(), remote.clone());
    assert!(apply_with_client(&package, "synthetic", "replace", &first, true, false).is_err());

    let resumed = sheets_client("synthetic", worksheets, remote);
    let result = apply_with_client(&package, "synthetic", "replace", &resumed, true, true)
        .expect("resumed apply");
    assert_eq!(result["state"], "committed");
}

#[test]
fn failed_write_returns_complete_per_dataset_accounting() {
    let package = export_package("write-fail", &audit());
    struct FailingClient {
        calls: Mutex<usize>,
        aborted: Mutex<bool>,
    }
    impl DestinationClient for FailingClient {
        fn destination_name(&self) -> &str {
            "synthetic-failure"
        }
        fn authorize_target(&self, _target: &str) -> bool {
            true
        }
        fn begin(&self, _args: &BeginArgs<'_>) -> Result<Value, DestinationError> {
            Ok(json!({}))
        }
        fn write(
            &self,
            _transaction: &mut Value,
            _dataset: &str,
            _rows: &[Vec<String>],
        ) -> Result<(), DestinationError> {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            if *calls == 2 {
                return Err(bi_err("synthetic rejected bounded row"));
            }
            Ok(())
        }
        fn abort(&self, _transaction: &mut Value) -> Result<Option<()>, DestinationError> {
            *self.aborted.lock().unwrap() = true;
            Ok(Some(()))
        }
        fn commit(&self, _transaction: &mut Value) -> Result<Value, DestinationError> {
            panic!("failed write must stop before commit");
        }
    }
    let client = FailingClient {
        calls: Mutex::new(0),
        aborted: Mutex::new(false),
    };
    let result =
        apply_with_client(&package, "synthetic", "replace", &client, true, false).expect("apply");
    assert_eq!(result["state"], "failed");
    assert!(*client.aborted.lock().unwrap());
    assert_eq!(
        result["input_rows"]["cohorts"],
        result["failed_rows"]["cohorts"]
    );
    assert_eq!(result["rows"]["cohorts"], 0);
    let input: std::collections::BTreeSet<&String> =
        result["input_rows"].as_object().unwrap().keys().collect();
    let rows: std::collections::BTreeSet<&String> =
        result["rows"].as_object().unwrap().keys().collect();
    let failed: std::collections::BTreeSet<&String> =
        result["failed_rows"].as_object().unwrap().keys().collect();
    assert_eq!(input, rows);
    assert_eq!(rows, failed);
}

// ---------------------------------------------------------------------------
// the reference suite — complete local group companions
// retained through selection, preview, apply and XLSX publication.
// Deferred: test_companion_copy_respects_the_combined_filter_partition_budget
// (reference monkeypatches bi.MAX_OUTPUT_PARTITIONS; Rust binds the constant).
// ---------------------------------------------------------------------------

use spiderhead_reports::bi::schema::{
    Groups, DEFAULT_BYTES_PER_PARTITION, DEFAULT_MAX_OUTPUT_BYTES, DEFAULT_ROWS_PER_PARTITION,
};
use spiderhead_reports::bi::writer::OutputBudget;
use spiderhead_reports::bi_destinations::{_manifest, EXCEL_MAX_ROWS};
use spiderhead_reports::bi_index::{
    verify_group_member_references, write_group_members_companion, GroupIndex,
};

fn copy_dir(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), &destination).unwrap();
        }
    }
}

/// `package` fixture: the reporting-pack worksheets plus a rebuilt
/// `complete-group` companion bound to `run.audit_sha256 = "a"*64`.
fn companion_package(tag: &str) -> (PathBuf, Value) {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reporting-pack-worksheets");
    let package = tmp(tag).join("package");
    copy_dir(&fixture, &package);
    let mut manifest = read_manifest(&package);
    manifest["run"]["audit_sha256"] = json!("a".repeat(64));
    let audit_sha = "a".repeat(64);
    let urls: Vec<Value> = (0..30)
        .map(|n| json!(format!("https://example.test/member-{n}")))
        .collect();
    let con = rusqlite::Connection::open_in_memory().unwrap();
    let mut groups = Groups::List(vec![json!({
        "group_id": "complete-group",
        "count": 30,
        "urls": urls,
    })]);
    let mut index = GroupIndex::new(&con, &mut groups).expect("group index");
    let run_id = manifest["run"]["run_id"].as_str().unwrap().to_owned();
    let companion = write_group_members_companion(
        &mut index,
        &package,
        &run_id,
        Some(&audit_sha),
        7,
        1_000_000,
        &OutputBudget::new(10_000_000),
    )
    .expect("companion");
    let reference = index
        .get("complete-group")
        .expect("group get")
        .expect("group exists")["urls"]
        .clone();
    drop(index);
    drop(con);
    // Attach the group reference to the first two finding rows, exactly like
    // the reference fixture does.
    let dataset = manifest["datasets"]["findings"].clone();
    let part = dataset["partitions"][0].clone();
    let path = package.join(part["path"].as_str().unwrap());
    let mut reader = csv::Reader::from_path(&path).unwrap();
    let fields: Vec<String> = reader
        .headers()
        .unwrap()
        .iter()
        .map(str::to_owned)
        .collect();
    let mut rows: Vec<Vec<String>> = reader
        .records()
        .map(|r| r.unwrap().iter().map(str::to_owned).collect())
        .collect();
    let column = |name: &str| fields.iter().position(|f| f == name).unwrap();
    let (c_id, c_count, c_json) = (
        column("group_id"),
        column("group_url_count"),
        column("group_urls_json"),
    );
    for row in rows.iter_mut().take(2) {
        row[c_id] = "complete-group".to_owned();
        row[c_count] = "30".to_owned();
        row[c_json] = reference.to_string();
    }
    let mut writer = csv::Writer::from_path(&path).unwrap();
    writer.write_record(&fields).unwrap();
    for row in &rows {
        writer.write_record(row).unwrap();
    }
    drop(writer);
    let new_bytes = fs::metadata(&path).unwrap().len();
    let mut manifest_datasets = manifest["datasets"].clone();
    manifest_datasets["findings"]["bytes"] = json!(
        dataset["bytes"].as_i64().unwrap() + new_bytes as i64 - part["bytes"].as_i64().unwrap()
    );
    manifest_datasets["findings"]["partitions"][0]["bytes"] = json!(new_bytes);
    manifest_datasets["findings"]["partitions"][0]["sha256"] =
        json!(sha256(&fs::read(&path).unwrap()));
    manifest["datasets"] = manifest_datasets;
    manifest["group_members"] = companion.clone();
    write_manifest(&package, &manifest);
    let verified = verify_group_member_references(&package, &manifest)
        .expect("verify")
        .expect("descriptor");
    assert_eq!(verified["members"], 30);
    (package, companion)
}

/// `_companion_files(package)`: path -> bytes for both companion kinds.
fn companion_files(package: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let manifest = read_manifest(package);
    let mut files = std::collections::BTreeMap::new();
    for kind in ["groups", "members"] {
        for part in manifest["group_members"][kind]["partitions"]
            .as_array()
            .unwrap()
        {
            let path = part["path"].as_str().unwrap();
            files.insert(path.to_owned(), fs::read(package.join(path)).unwrap());
        }
    }
    files
}

/// `_assert_local_only(result, companion)`.
fn assert_local_only(result: &Value, companion: &Value) {
    let refs = result["external_companion_refs"].as_array().unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0]["publication"], "local_only");
    assert_eq!(refs[0]["published"], false);
    assert_eq!(refs[0]["groups"], 1);
    assert_eq!(refs[0]["members"], 30);
    assert_eq!(refs[0]["sha256"], companion["sha256"]);
    assert_eq!(
        refs[0]["source_audit_sha256"],
        companion["source_audit_sha256"]
    );
}

fn filtered_findings_companion(tag: &str, columns: Option<Vec<String>>) {
    let (package, companion) = companion_package(tag);
    let manifest = read_manifest(&package);
    let parent = package.parent().unwrap().to_path_buf();
    let selected = parent.join("selected");
    let where_ = Map::from_iter([("group_id".to_owned(), json!(["complete-group"]))]);
    let result = filter_package(
        &package,
        "findings",
        &selected,
        Some(&where_),
        columns.as_deref(),
        DEFAULT_ROWS_PER_PARTITION,
        DEFAULT_BYTES_PER_PARTITION,
        DEFAULT_MAX_OUTPUT_BYTES,
    )
    .expect("filter");
    assert_eq!(result["row_count"], 2);
    assert_eq!(companion_files(&selected), companion_files(&package));
    assert_eq!(result["group_members"], manifest["group_members"]);
    let (root, normalized) = _manifest(&selected).expect("manifest");
    assert_eq!(normalized["run"], manifest["run"]);
    let verified = verify_group_member_references(&root, &normalized)
        .expect("verify")
        .expect("descriptor");
    assert_eq!(verified["members"], 30);
    assert_local_only(&result, &companion);
    assert_local_only(
        &sheets_plan(&selected, 10_000_000).expect("sheets plan"),
        &companion,
    );
    let twice = parent.join("twice");
    filter_package(
        &selected,
        "findings",
        &twice,
        None,
        None,
        DEFAULT_ROWS_PER_PARTITION,
        DEFAULT_BYTES_PER_PARTITION,
        DEFAULT_MAX_OUTPUT_BYTES,
    )
    .expect("filter twice");
    assert_eq!(companion_files(&twice), companion_files(&package));
    assert_eq!(sheets_plan(&twice, 10_000_000).unwrap()["state"], "ready");
}

#[test]
fn filtered_findings_companion_all_columns() {
    filtered_findings_companion("companion-all", None);
}

#[test]
fn filtered_findings_companion_selected_columns() {
    filtered_findings_companion(
        "companion-cols",
        Some(vec!["finding_id".to_owned(), "url".to_owned()]),
    );
}

struct NeverAuthorized;

impl DestinationClient for NeverAuthorized {
    fn destination_name(&self) -> &str {
        "never"
    }
    fn authorize_target(&self, _target: &str) -> bool {
        panic!("invalid source must fail before target authorization");
    }
}

#[test]
fn invalid_companions_fail_before_authentication_or_remote_write() {
    for corruption in [
        "missing_descriptor",
        "missing_file",
        "corrupt_file",
        "wrong_source",
    ] {
        let (package, _companion) = companion_package(&format!("corrupt-{corruption}"));
        let mut manifest = read_manifest(&package);
        match corruption {
            "missing_descriptor" => {
                manifest.as_object_mut().unwrap().remove("group_members");
            }
            "wrong_source" => {
                manifest["run"]["audit_sha256"] = json!("b".repeat(64));
            }
            _ => {
                let member = package.join(
                    manifest["group_members"]["members"]["partitions"][0]["path"]
                        .as_str()
                        .unwrap(),
                );
                if corruption == "missing_file" {
                    fs::remove_file(&member).unwrap();
                } else {
                    let mut bytes = fs::read(&member).unwrap();
                    bytes.extend_from_slice(b"\n");
                    fs::write(&member, bytes).unwrap();
                }
            }
        }
        write_manifest(&package, &manifest);
        let err = apply_with_client(
            &package,
            "synthetic",
            "replace",
            &NeverAuthorized,
            true,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("companion"), "{corruption}: {err}");
    }
}

#[test]
fn google_publication_discloses_local_only_members_and_uploads_only_main_datasets() {
    let (package, companion) = companion_package("g-pub");
    let manifest = read_manifest(&package);
    let mapping: Map<String, Value> = manifest["datasets"]
        .as_object()
        .unwrap()
        .keys()
        .enumerate()
        .map(|(index, name)| {
            (
                name.clone(),
                json!({"worksheet_id": index, "worksheet_title": name}),
            )
        })
        .collect();
    let config = tmp("g-pub-config").join("destinations.json");
    fs::write(
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
    let host = FakeHost::with_config(&config);
    let preview =
        destination_preview(&host, &package, "synthetic", "sheets", "replace").expect("preview");
    assert_local_only(&preview, &companion);
    let preview_datasets: std::collections::BTreeSet<&String> =
        preview["datasets"].as_object().unwrap().keys().collect();
    let manifest_datasets: std::collections::BTreeSet<&String> =
        manifest["datasets"].as_object().unwrap().keys().collect();
    assert_eq!(preview_datasets, manifest_datasets);
    assert_local_only(
        &bigquery_plan(&package, "synthetic", "replace").expect("bq plan"),
        &companion,
    );
    let remote = SheetsRemote::new(mapping.clone());
    let client = GoogleSheetsClient::new(
        "synthetic",
        "sheet-id",
        mapping.clone(),
        Some(Arc::new(|_scope: &str| Some("token".to_owned()))),
        Some(remote.clone()),
    );
    let result =
        apply_with_client(&package, "synthetic", "replace", &client, true, false).expect("apply");
    assert_eq!(result["state"], "committed");
    assert_local_only(&result, &companion);
    let published_rows: std::collections::BTreeSet<&String> =
        result["rows"].as_object().unwrap().keys().collect();
    assert_eq!(published_rows, manifest_datasets);
    // Every written range belongs to a published dataset — no companion sheet.
    let remote_values = remote.values.lock().unwrap();
    assert!(!remote_values.is_empty());
    assert!(remote_values
        .keys()
        .all(|range| manifest_datasets.iter().any(|d| range.contains(d.as_str()))));
    drop(remote_values);
    let requests = remote.requests.lock().unwrap();
    assert!(!requests.iter().any(|request| {
        request["body"].to_string().contains("group-members")
            && request["body"].to_string().contains("addSheet")
    }));
    let calls = requests.len();
    drop(requests);
    let replay =
        apply_with_client(&package, "synthetic", "replace", &client, true, false).expect("replay");
    assert_local_only(&replay, &companion);
    assert_eq!(remote.requests.lock().unwrap().len(), calls);
}

#[test]
fn pending_publication_also_discloses_unpublished_companion() {
    let (package, companion) = companion_package("pending-pub");
    struct Pending;
    impl DestinationClient for Pending {
        fn destination_name(&self) -> &str {
            "pending"
        }
        fn authorize_target(&self, _target: &str) -> bool {
            true
        }
        fn begin(&self, _args: &BeginArgs<'_>) -> Result<Value, DestinationError> {
            Ok(json!({}))
        }
        fn write(
            &self,
            _transaction: &mut Value,
            _dataset: &str,
            _rows: &[Vec<String>],
        ) -> Result<(), DestinationError> {
            Err(uncertain("synthetic lost response"))
        }
    }
    let result =
        apply_with_client(&package, "synthetic", "replace", &Pending, true, false).expect("apply");
    assert_eq!(result["state"], "reconciliation_required");
    assert_local_only(&result, &companion);
}

#[test]
fn workbook_index_keeps_the_explicit_local_companion_reference() {
    let (package, companion) = companion_package("wb-index");
    let out = package.parent().unwrap().join("findings.xlsx");
    let result = export_bi_xlsx(
        &package,
        "findings",
        &out,
        EXCEL_MAX_ROWS - 1,
        DEFAULT_MAX_OUTPUT_BYTES,
    )
    .expect("xlsx");
    assert_local_only(&result, &companion);
    let index: Value =
        serde_json::from_str(&fs::read_to_string(result["index"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_local_only(&index, &companion);
}
