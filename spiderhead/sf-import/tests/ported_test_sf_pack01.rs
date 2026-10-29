//! Differential harness for `sf-pack-01-sf-import`: replays every case of the
//! reference-oracle fixture `tests/fixtures/sf_pack01.json` produced by
//! the reference suite against the Rust port. Covers:
//!
//! // from the reference suite (config_* cases)
//! // from the reference suite (config_default/example)
//! // from the reference suite (manifest_* cases)
//! // from the reference suite (snippet_* cases)

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{json, Value};

use spiderhead_sf_import::config;
use spiderhead_sf_import::export_manifest as em;
use spiderhead_sf_import::loader::LoadedExports;
use spiderhead_sf_import::snippet_edits as se;
use spiderhead_sf_import::table::Table;

fn cases() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/sf_pack01.json")).expect("fixture parses")
}

/// The on-disk fixture directory (`tests/fixtures/sf_pack01`), which is
/// `{root}` in recorded paths.
fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sf_pack01")
}

/// `{root}` in fixture strings becomes the real fixture directory.
fn subst_root(text: &str) -> String {
    text.replace("{root}", &fixture_root().to_string_lossy())
}

/// The inverse substitution: actual output text with the real path becomes
/// `{root}` again for comparison.
fn redact_root(text: &str) -> String {
    text.replace(&fixture_root().to_string_lossy().into_owned(), "{root}")
}

/// `capture(fn)` of the generator: `{ok: value}` or `{error: "Class: text"}`.
fn capture(result: Result<Value, String>) -> Value {
    match result {
        Ok(v) => json!({"ok": v}),
        Err(e) => json!({"error": redact_root(&e)}),
    }
}

/// `ExportRequest` from the `_req_json` shape the generator recorded.
fn request_from_json(v: &Value) -> em::ExportRequest {
    let strings = |key: &str| -> Vec<String> {
        v.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|x| x.as_str().unwrap_or_default().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    em::ExportRequest {
        group: v["group"].as_str().unwrap_or_default().to_owned(),
        name: v["name"].as_str().unwrap_or_default().to_owned(),
        keys: strings("keys"),
        raw_id: v.get("raw_id").and_then(Value::as_str).map(str::to_owned),
        file_tokens: strings("file_tokens"),
        derived_keys: strings("derived_keys"),
        derived_required_columns: strings("derived_required_columns"),
    }
}

/// `_req_json`: the request serialized the way the generator records it.
fn request_json(r: &em::ExportRequest) -> Value {
    json!({
        "group": r.group,
        "name": r.name,
        "keys": r.keys,
        "raw_id": r.raw_id,
        "file_tokens": r.file_tokens,
        "derived_keys": r.derived_keys,
        "derived_required_columns": r.derived_required_columns,
    })
}

fn requests_json(rs: &[em::ExportRequest]) -> Value {
    Value::Array(rs.iter().map(request_json).collect())
}

fn requests_of(case: &Value) -> Vec<em::ExportRequest> {
    case["requests"]
        .as_array()
        .expect("requests list")
        .iter()
        .map(request_from_json)
        .collect()
}

fn manifest_err(e: em::ManifestError) -> String {
    let class = match &e {
        em::ManifestError::Value(_) => "ValueError",
        em::ManifestError::Runtime(_) => "RuntimeError",
        em::ManifestError::NotDirectory(_) => "NotADirectoryError",
        em::ManifestError::Loader(_) => "OSError",
    };
    format!("{class}: {e}")
}

// --- font loader seam -------------------------------------------------------

struct FakeFont(f64);
impl se::TextMeasure for FakeFont {
    fn getlength(&self, text: &str) -> f64 {
        text.chars().count() as f64 * self.0
    }
}

/// The `_truetype(name)` patched loader: a factor per name, `fail` raises
/// `OSError("cannot open resource")` which the port turns into a ValueError.
fn loader_for(name: &str) -> impl Fn(&str, i64) -> Result<Rc<dyn se::TextMeasure>, String> + '_ {
    move |path, size| {
        let _ = (path, size);
        match name {
            "f4.2" => Ok(Rc::new(FakeFont(4.2)) as Rc<dyn se::TextMeasure>),
            "f3" => Ok(Rc::new(FakeFont(3.0)) as Rc<dyn se::TextMeasure>),
            "f1" => Ok(Rc::new(FakeFont(1.0)) as Rc<dyn se::TextMeasure>),
            _ => Err("cannot open resource".to_owned()),
        }
    }
}

// --- spellcheck seam --------------------------------------------------------

type SeamLog = Rc<RefCell<Vec<Value>>>;

/// The `_fake_spellcheck_run` the generator patched in: record the seam
/// inputs and answer the canned skipped verdict.
fn fake_spellcheck(calls: SeamLog) -> impl Fn(&se::SpellcheckArgs<'_>) -> Value {
    move |args| {
        calls.borrow_mut().push(json!({
            "html": args.html,
            "ignore_words": args.ignore_words,
            "content_area": args.content_area.cloned().unwrap_or(Value::Null),
            "visible_text_override": args.visible_text_override,
        }));
        json!({"state": "skipped", "reason": "no backend configured", "seam": true})
    }
}

// --- storage seam -----------------------------------------------------------

struct FakeCon {
    calls: SeamLog,
    mapping: HashMap<String, Value>,
    scan_error_for: Vec<String>,
}

impl se::ScanConnection for FakeCon {
    fn read_page_document_content(
        &self,
        url: &Value,
        representation: &str,
        variant_key: Option<&Value>,
        max_decoded_bytes: i64,
    ) -> Result<Value, se::ReadError> {
        self.calls.borrow_mut().push(json!({
            "op": "read",
            "url": url,
            "representation": representation,
            "variant_key": variant_key.cloned().unwrap_or(Value::Null),
            "max_decoded_bytes": max_decoded_bytes,
        }));
        if url
            .as_str()
            .is_some_and(|u| self.scan_error_for.iter().any(|s| s == u))
        {
            return Err(se::ReadError::Scan(format!(
                "boom for {}",
                spiderhead_parse::py_str::py_repr(url.as_str().unwrap_or_default())
            )));
        }
        Ok(url
            .as_str()
            .and_then(|u| self.mapping.get(u).cloned())
            .unwrap_or_else(|| {
                json!({
                    "state": "unavailable",
                    "reason": "document_not_in_corpus",
                    "url": url,
                })
            }))
    }
}

impl Drop for FakeCon {
    fn drop(&mut self) {
        self.calls.borrow_mut().push(json!({"op": "close"}));
    }
}

/// The `_fake_open_scan` + `_make_reader` pair.
fn fake_opener(
    calls: SeamLog,
    mapping: HashMap<String, Value>,
    scan_error_for: Vec<String>,
) -> impl Fn(&Path) -> Result<Box<dyn se::ScanConnection>, se::SnippetError> {
    move |path| {
        calls.borrow_mut().push(json!({
            "op": "open_scan",
            "path": path.to_string_lossy(),
            "require_audit": false,
        }));
        Ok(Box::new(FakeCon {
            calls: calls.clone(),
            mapping: mapping.clone(),
            scan_error_for: scan_error_for.clone(),
        }) as Box<dyn se::ScanConnection>)
    }
}

/// `_ATTACH_DOC` of the generator.
fn attach_doc() -> Value {
    json!({
        "state": "available",
        "url": "https://example.test/p",
        "text": "<html><head><title>Page title</title><meta name=\"description\" content=\"Meta summary\"></head><body><nav>Boilerplate query</nav><main>Authorization: Bearer synthetic-token. Page content for query. <span data-nosnippet>Excluded query text.</span></main></body></html>",
        "document_id": 7,
        "representation": "static",
        "raw_body_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "encoding": "utf-8",
        "decoder": {"source": "stored"},
        "validation": {"state": "light"},
        "total_chars": 220,
        "lines": 1,
    })
}

/// The generator's `(mapping, scan_error_for)` table, keyed by case name.
fn attach_stub(name: &str) -> (HashMap<String, Value>, Vec<String>) {
    let url = "https://example.test/p";
    match name {
        "attach-hit"
        | "attach-rendered"
        | "attach-two-hit-miss"
        | "attach-no-proposal"
        | "attach-proposal-nostr"
        | "attach-proposal-list" => (HashMap::from([(url.to_owned(), attach_doc())]), Vec::new()),
        "attach-scanerr" => (HashMap::new(), vec![url.to_owned()]),
        "attach-no-source-identity" => (
            HashMap::from([(
                url.to_owned(),
                json!({"state": "unavailable", "reason": "nope"}),
            )]),
            Vec::new(),
        ),
        _ => (HashMap::new(), Vec::new()),
    }
}

/// The generator records `tasks` after the in-place call, so a successful or
/// part-applied attach is replayed from the post-state. Rebuild the pre-state:
/// drop `retained_document` and cut the appended ` Retained source: …` tail.
fn pre_attach_tasks(tasks: &Value) -> Value {
    let mut tasks = tasks.clone();
    if let Some(arr) = tasks.get_mut("tasks").and_then(Value::as_array_mut) {
        for task in arr.iter_mut() {
            let Some(obj) = task.as_object_mut() else {
                continue;
            };
            obj.remove("retained_document");
            let summary = obj
                .get("goal_proposal")
                .and_then(|g| g.get("summary"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(summary) = summary {
                if let Some(idx) = summary.find(" Retained source:") {
                    if let Some(g) = obj.get_mut("goal_proposal").and_then(Value::as_object_mut) {
                        g.insert("summary".to_owned(), json!(&summary[..idx]));
                    }
                }
            }
        }
    }
    tasks
}

/// `attach_retained_measurements` writes `pixel_widths`/`previews` into the
/// retained document's `metadata` and `query_excerpt`; the recorded tasks are
/// again the post-state. Strip the injected keys for the replay.
fn pre_measure_tasks(tasks: &Value) -> Value {
    let mut tasks = tasks.clone();
    if let Some(arr) = tasks.get_mut("tasks").and_then(Value::as_array_mut) {
        for task in arr.iter_mut() {
            let Some(obj) = task.as_object_mut() else {
                continue;
            };
            for key in ["metadata", "query_excerpt"] {
                if let Some(target) = obj
                    .get_mut("retained_document")
                    .and_then(|rd| rd.get_mut(key))
                    .and_then(Value::as_object_mut)
                {
                    target.remove("pixel_widths");
                    target.remove("previews");
                }
            }
        }
    }
    tasks
}

/// Substitute `{root}` inside every `font_path` string of a metrics dict.
fn subst_font_paths(metrics: &Value) -> Value {
    let mut metrics = metrics.clone();
    if let Some(obj) = metrics.as_object_mut() {
        for spec in obj.values_mut() {
            if let Some(spec_obj) = spec.as_object_mut() {
                if let Some(fp) = spec_obj.get_mut("font_path") {
                    if let Some(s) = fp.as_str() {
                        *fp = json!(subst_root(s));
                    }
                }
            }
        }
    }
    metrics
}

// --- per-kind runner --------------------------------------------------------

fn run_case(case: &Value) -> Value {
    let kind = case["kind"].as_str().expect("kind");
    match kind {
        "config_default" | "config_example" => config::default_config(),
        "config_lite_exports" => config::lite_exports(),
        "config_deep_merge" => capture(
            config::deep_merge(&config::default_config(), &case["user"]).map_err(|e| e.to_string()),
        ),
        "config_load_file" => capture(
            config::load_config(Some(
                &fixture_root()
                    .join(case["file"].as_str().expect("file"))
                    .to_string_lossy(),
            ))
            .map_err(|e| e.to_string()),
        ),
        "config_load_missing" => capture(
            config::load_config(Some(&subst_root(case["path"].as_str().expect("path"))))
                .map_err(|e| e.to_string()),
        ),
        "config_apply_profile" => {
            capture(config::apply_profile(&case["cfg"]).map_err(|e| e.to_string()))
        }
        "config_validate" => capture(
            config::validate_config(&case["cfg"])
                .map(|()| Value::Null)
                .map_err(|e| e.to_string()),
        ),
        "manifest_profile_exports" => em::default_profile_exports(),
        "manifest_requests" => capture(
            em::requests_from_config(&case["exports"])
                .map(|rs| requests_json(&rs))
                .map_err(manifest_err),
        ),
        "manifest_is_full" => capture(
            em::is_full_manifest(&case["exports"])
                .map(Value::Bool)
                .map_err(manifest_err),
        ),
        "manifest_resolved_profile" => capture(
            em::resolved_profile(case["profile"].as_str().expect("profile"), &case["exports"])
                .map(Value::String)
                .map_err(manifest_err),
        ),
        "manifest_help_names" => capture(
            em::help_names(
                case["text"].as_str().expect("text"),
                case["group"].as_str().expect("group"),
            )
            .map(|names| Value::Array(names.into_iter().map(Value::String).collect()))
            .map_err(manifest_err),
        ),
        "manifest_unsupported" => {
            let caps: HashMap<String, BTreeSet<String>> = HashMap::from([
                (
                    "tabs".to_owned(),
                    BTreeSet::from(["Internal:All".to_owned(), "H1:All".to_owned()]),
                ),
                ("bulk".to_owned(), BTreeSet::new()),
                ("reports".to_owned(), BTreeSet::new()),
            ]);
            let manifest = em::full_export_manifest();
            let refs: Vec<em::ExportRequest> = manifest[..6].to_vec();
            Value::Array(
                em::unsupported_requests(&refs, &caps)
                    .into_iter()
                    .map(request_json)
                    .collect(),
            )
        }
        "manifest_fingerprint" => {
            let caps: HashMap<String, BTreeSet<String>> = case["capabilities"]
                .as_object()
                .expect("caps")
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_array()
                            .expect("names")
                            .iter()
                            .filter_map(|n| n.as_str().map(str::to_owned))
                            .collect(),
                    )
                })
                .collect();
            Value::String(em::help_fingerprint(&caps))
        }
        "manifest_output_tokens" => {
            let req = match case["name"].as_str().expect("name") {
                "tokens-named" => em::ExportRequest {
                    group: "tabs".to_owned(),
                    name: "Structured Data:Validation Errors".to_owned(),
                    keys: Vec::new(),
                    raw_id: Some("x".to_owned()),
                    file_tokens: vec!["a".to_owned(), "b".to_owned()],
                    derived_keys: Vec::new(),
                    derived_required_columns: Vec::new(),
                },
                "tokens-derived" => em::ExportRequest {
                    group: "tabs".to_owned(),
                    name: "Images:Over X KB Stuff".to_owned(),
                    keys: Vec::new(),
                    raw_id: None,
                    file_tokens: Vec::new(),
                    derived_keys: Vec::new(),
                    derived_required_columns: Vec::new(),
                },
                _ => em::ExportRequest {
                    group: "tabs".to_owned(),
                    name: "Custom:X One Two X".to_owned(),
                    keys: Vec::new(),
                    raw_id: None,
                    file_tokens: Vec::new(),
                    derived_keys: Vec::new(),
                    derived_required_columns: Vec::new(),
                },
            };
            Value::Array(
                em::output_tokens(&req)
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            )
        }
        "manifest_validate_files" => {
            let dir = fixture_root().join(case["dir"].as_str().unwrap_or("absent-dir"));
            let requests = requests_of(case);
            let derive = case["derive"].as_bool().unwrap_or(false);
            capture(em::validate_export_files(&dir, &requests, derive).map_err(manifest_err))
        }
        "manifest_validate_loaded" => {
            let frames: HashMap<String, Table> = case["frames"]
                .as_array()
                .expect("frames")
                .iter()
                .filter_map(|k| {
                    k.as_str()
                        .map(|s| (s.to_owned(), Table::new(Vec::new(), Vec::new())))
                })
                .collect();
            let missing: Vec<String> = case["missing"]
                .as_array()
                .expect("missing")
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            let derivation_errors: HashMap<String, String> = case["derivation_errors"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                        .collect()
                })
                .unwrap_or_default();
            let exports = LoadedExports {
                frames,
                missing,
                derivation_errors,
                ..LoadedExports::default()
            };
            let requests = requests_of(case);
            let derive = case["derive"].as_bool().unwrap_or(false);
            capture(
                em::validate_loaded_requests(&exports, &requests, derive)
                    .map(|()| Value::Null)
                    .map_err(manifest_err),
            )
        }
        "snippet_parse_csv" => capture(
            se::parse_snippet_csv(case["text"].as_str().expect("text")).map_err(|e| e.to_string()),
        ),
        "snippet_parse_csv_oversize" => {
            let text = "x".repeat(se::MAX_CHARS + 1);
            capture(se::parse_snippet_csv(&text).map_err(|e| e.to_string()))
        }
        "snippet_measure_pixels" => {
            let loader = loader_for(case["loader"].as_str().expect("loader"));
            capture(
                se::measure_text_pixels(
                    case["text"].as_str().expect("text"),
                    &subst_root(case["font_path"].as_str().expect("font_path")),
                    case["font_size"].as_i64().expect("font_size"),
                    &loader,
                )
                .map(|w| json!(w))
                .map_err(|e| e.to_string()),
            )
        }
        "snippet_measure" => {
            let loader = loader_for(case["loader"].as_str().expect("loader"));
            let edits = case["edits"].as_array().cloned().unwrap_or_default();
            let metrics = subst_font_paths(&case["metrics"]);
            capture(se::measure_snippet_edits(&edits, &metrics, &loader).map_err(|e| e.to_string()))
        }
        "snippet_excerpt" => Value::Object(se::bounded_query_excerpt(
            case["text"].as_str().expect("text"),
            case["start"].as_u64().expect("start") as usize,
            case["end"].as_u64().expect("end") as usize,
        )),
        "snippet_inspect" => {
            let calls: SeamLog = Rc::new(RefCell::new(Vec::new()));
            let spell = fake_spellcheck(calls.clone());
            let kwargs = &case["kwargs"];
            let outcome = se::inspect_retained_document(
                &case["document"],
                kwargs.get("query"),
                kwargs.get("content_area"),
                kwargs.get("ignore_words"),
                &spell,
            )
            .map_err(|e| e.to_string());
            json!({
                "expect": capture(outcome),
                "seam": Value::Array(calls.borrow().clone()),
            })
        }
        "snippet_build_tasks" => {
            capture(se::build_snippet_tasks(&case["parsed"]).map_err(|e| e.to_string()))
        }
        "snippet_write_tasks" => {
            let name = case["name"].as_str().unwrap_or("x");
            let tmp = std::env::temp_dir().join(format!("sf-pack01-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            let dir = tmp.join("wt-out");
            let target = dir.join("tasks.json");
            if name == "write-exists" {
                // The generator leaves the `write-ok` payload on disk, so the
                // pre-existing file holds exactly the recorded `written` text.
                std::fs::create_dir_all(&dir).expect("mkdir");
                std::fs::write(&target, case["written"].as_str().expect("written")).expect("seed");
            }
            let tmp_str = tmp.to_string_lossy().into_owned();
            let outcome = se::write_snippet_tasks(&case["tasks"], &target)
                .map(|p| json!(redact_root(&p).replace(&tmp_str, "{root}")))
                .map_err(|e| redact_root(&e.to_string()).replace(&tmp_str, "{root}"));
            if let Some(want) = case.get("written") {
                let written = std::fs::read_to_string(&target).unwrap_or_default();
                assert_eq!(
                    &written,
                    want.as_str().expect("written"),
                    "written payload for {name}"
                );
            }
            let _ = std::fs::remove_dir_all(&tmp);
            capture(outcome)
        }
        "snippet_attach_context" => {
            let calls: SeamLog = Rc::new(RefCell::new(Vec::new()));
            let spell = fake_spellcheck(calls.clone());
            let scan_calls: SeamLog = Rc::new(RefCell::new(Vec::new()));
            // The reader mapping lives in the generator's table, not in the
            // recorded case — mirror it by case name.
            let (mapping, scan_error_for) = attach_stub(case["name"].as_str().unwrap_or(""));
            let opener = fake_opener(scan_calls.clone(), mapping, scan_error_for);
            let mut tasks = pre_attach_tasks(&case["tasks"]);
            let kwargs = case["kwargs"].as_object().cloned().unwrap_or_default();
            let outcome = se::attach_retained_context_in(
                &mut tasks,
                Path::new("{root}/scan.sqlite"),
                &opener,
                &kwargs,
                &spell,
            )
            .map(|()| tasks.clone())
            .map_err(|e| e.to_string());
            json!({
                "expect": capture(outcome),
                "seam": Value::Array(calls.borrow().clone()),
                "scan": Value::Array(scan_calls.borrow().clone()),
            })
        }
        "snippet_attach_measurements" => {
            let loader = loader_for(case["loader"].as_str().expect("loader"));
            let mut tasks = pre_measure_tasks(&case["tasks"]);
            let metrics = subst_font_paths(&case["metrics"]);
            let outcome = se::attach_retained_measurements(&mut tasks, &metrics, &loader)
                .map(|()| tasks.clone())
                .map_err(|e| e.to_string());
            capture(outcome)
        }
        other => panic!("unhandled fixture kind {other}"),
    }
}

fn expected(case: &Value) -> Value {
    match case["kind"].as_str().expect("kind") {
        "snippet_inspect" => json!({
            "expect": case["expect"],
            "seam": case["seam_calls"],
        }),
        "snippet_attach_context" => json!({
            "expect": case["expect"],
            "seam": case["seam_calls"],
            "scan": case["scan_calls"],
        }),
        _ => case["expect"].clone(),
    }
}

#[test]
fn sf_pack01_oracle_parity() {
    let mut failed: Vec<String> = Vec::new();
    for case in cases() {
        let name = case["name"].as_str().unwrap_or("?").to_owned();
        let actual = run_case(&case);
        let want = expected(&case);
        if actual != want {
            failed.push(format!(
                "{name}:\n  got  {}\n  want {}",
                serde_json::to_string(&actual).unwrap_or_default(),
                serde_json::to_string(&want).unwrap_or_default()
            ));
        }
    }
    assert!(
        failed.is_empty(),
        "{} oracle diffs:\n{}",
        failed.len(),
        failed.join("\n")
    );
}
