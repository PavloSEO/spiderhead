//! Oracle parity for `verification` against the reference `spiderhead.core.verification`
//! module. Fixture `fixtures/verification.json` is emitted by the reference suite
//! (run with the oracle venv); every case carries the oracle's output or error text.
//!
//! audit.v2 reader cases rebuild the same companions from the shared raw scan, so the
//! retained binding and content hash are identical on both sides by construction.

use indexmap::IndexMap;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use spiderhead_sf_import::verification::{self, AuditSource, VerifyError};
use spiderhead_store::audit_v2::{write_audit_v2, AuditV2Reader};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

const FIXTURE: &str = include_str!("fixtures/verification.json");

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "sf-import-verification-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}

fn obj(v: &Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn objs(v: &Value) -> Vec<Map<String, Value>> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_object().unwrap().clone())
        .collect()
}

fn outcome<T>(
    expected: &Value,
    got: Result<T, VerifyError>,
    render: impl Fn(&T) -> Value,
    ctx: &str,
) {
    match (&expected["out"], got) {
        (Value::Object(o), Ok(value)) if o.contains_key("ok") => {
            assert_eq!(render(&value), o["ok"], "{ctx}");
        }
        (Value::Object(o), Err(e)) if o.contains_key("error") => {
            assert_eq!(e.0, o["error"].as_str().unwrap(), "{ctx}");
        }
        (expected_out, got) => panic!(
            "{ctx}: outcome kind mismatch: expected {expected_out}, got {}",
            match got {
                Ok(v) => format!("ok {}", render(&v)),
                Err(e) => format!("error {}", e.0),
            }
        ),
    }
}

/// One rebuilt audit.v2 reader per fixture file (shared raw scan, so binding is real).
fn build_readers(files: &Map<String, Value>) -> (Scratch, HashMap<String, AuditV2Reader>) {
    let dir = Scratch::new("readers");
    let mut readers = HashMap::new();
    for (name, file) in files {
        let scan = dir.0.join(format!("{name}.sqlite"));
        fs::copy(repo("oracle/raw-scans/basic/scan.sqlite"), &scan).unwrap();
        let binding = {
            let con =
                Connection::open_with_flags(&scan, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .unwrap();
            let (uuid, rev, ver, wrev): (String, i64, String, String) = con
                .query_row(
                    "SELECT scan_uuid,evidence_revision,writer_version,writer_revision FROM scan WHERE singleton=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            json!({"scan_uuid": uuid, "evidence_revision": rev,
                   "analyzer_version": ver, "analyzer_revision": wrev})
        };
        let mut colls = IndexMap::new();
        for pair in file["collections"].as_array().unwrap() {
            colls.insert(
                pair[0].as_str().unwrap().to_owned(),
                pair[1].as_array().unwrap().clone(),
            );
        }
        write_audit_v2(
            &scan,
            &obj(&file["header"]),
            &colls,
            binding.as_object().unwrap(),
        )
        .unwrap();
        readers.insert(
            name.clone(),
            AuditV2Reader::new(&scan, true, false, None).unwrap(),
        );
    }
    (dir, readers)
}

fn reader<'a>(readers: &'a HashMap<String, AuditV2Reader>, case: &Value) -> &'a AuditV2Reader {
    &readers[case["file"].as_str().unwrap()]
}

#[test]
fn digest_matches() {
    for case in fixture()["digest"].as_array().unwrap() {
        assert_eq!(
            verification::digest(&case["in"]),
            case["out"].as_str().unwrap()
        );
    }
}

#[test]
fn policy_fingerprint_matches() {
    for case in fixture()["policy_fingerprint"].as_array().unwrap() {
        let got = verification::results_policy_fingerprint(&obj(&case["in"]));
        assert_eq!(
            got.map_or(Value::Null, Value::from),
            case["out"],
            "{}",
            case["in"]
        );
    }
}

#[test]
fn scan_identity_matches() {
    for case in fixture()["scan_identity"].as_array().unwrap() {
        let got = verification::scan_identity(&obj(&case["in"]));
        assert_eq!(
            got.map_or(Value::Null, Value::from),
            case["out"],
            "{}",
            case["in"]
        );
    }
}

#[test]
fn offline_gap_matches() {
    for (i, case) in fixture()["offline_gap"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let got =
            verification::offline_observation_gap(&obj(&case["before"]), &obj(&case["after"]));
        assert_eq!(
            got.map_or(Value::Null, Value::from),
            case["out"],
            "offline_gap[{i}]"
        );
    }
}

#[test]
fn select_matches() {
    for (i, case) in fixture()["select"].as_array().unwrap().iter().enumerate() {
        let got = verification::select(
            &obj(&case["baseline"]),
            case.get("finding_ids"),
            case.get("urls"),
            case.get("view"),
        );
        outcome(
            case,
            got,
            |(chosen, targets)| json!({"chosen": chosen, "targets": targets}),
            &format!("select[{i}]"),
        );
    }
}

#[test]
fn select_source_document_matches() {
    for (i, case) in fixture()["select_source_doc"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let baseline = obj(&case["baseline"]);
        let got = verification::select_source(
            &AuditSource::Document(&baseline),
            case.get("finding_ids"),
            case.get("urls"),
            case.get("view"),
        );
        outcome(
            case,
            got,
            |(doc, chosen, targets, identity)| {
                json!({
                    "document": doc, "chosen": chosen, "targets": targets, "identity": identity
                })
            },
            &format!("select_source_doc[{i}]"),
        );
    }
}

#[test]
fn compact_after_document_matches() {
    for (i, case) in fixture()["compact_after_doc"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let document = obj(&case["in"]);
        let selected = objs(&case["selected"]);
        let got = verification::compact_after_source(&AuditSource::Document(&document), &selected);
        outcome(
            case,
            got,
            |(doc, identity)| json!({"document": doc, "identity": identity}),
            &format!("compact_after_doc[{i}]"),
        );
    }
}

#[test]
fn source_identity_document_matches() {
    for (i, case) in fixture()["source_identity_doc"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let document = obj(&case["in"]);
        let got = verification::source_identity(&AuditSource::Document(&document));
        outcome(
            case,
            got,
            |o| Value::Object(o.clone()),
            &format!("source_identity_doc[{i}]"),
        );
    }
}

#[test]
fn classify_matches() {
    for case in fixture()["classify"].as_array().unwrap() {
        let baseline = obj(&case["baseline"]);
        let selected = objs(&case["selected"]);
        let observations = obj(&case["observations"]);
        let got = verification::classify(
            &baseline,
            &selected,
            &observations,
            case["missing_reason"].as_str().unwrap(),
        );
        outcome(
            case,
            got,
            |rows| Value::Array(rows.iter().cloned().map(Value::Object).collect()),
            case["name"].as_str().unwrap(),
        );
    }
}

#[test]
fn markdown_matches() {
    for (i, case) in fixture()["markdown"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            verification::markdown(&obj(&case["in"])),
            case["out"].as_str().unwrap(),
            "markdown[{i}]"
        );
    }
}

#[test]
fn reader_source_identity_matches() {
    let fix = fixture();
    let (_dir, readers) = build_readers(fix["files"].as_object().unwrap());
    for case in fix["source_identity_reader"].as_array().unwrap() {
        let source = AuditSource::Reader(reader(&readers, case));
        let got = verification::source_identity(&source);
        outcome(
            case,
            got,
            |o| Value::Object(o.clone()),
            &format!("source_identity_reader {}", case["file"]),
        );
    }
}

#[test]
fn reader_select_source_matches() {
    let fix = fixture();
    let (_dir, readers) = build_readers(fix["files"].as_object().unwrap());
    for case in fix["select_source_reader"].as_array().unwrap() {
        let source = AuditSource::Reader(reader(&readers, case));
        let got = verification::select_source(
            &source,
            case.get("finding_ids"),
            case.get("urls"),
            case.get("view"),
        );
        outcome(
            case,
            got,
            |(doc, chosen, targets, identity)| {
                json!({
                    "document": doc, "chosen": chosen, "targets": targets, "identity": identity
                })
            },
            &format!("select_source_reader {}", case["file"]),
        );
    }
}

#[test]
fn reader_compact_after_matches() {
    let fix = fixture();
    let (_dir, readers) = build_readers(fix["files"].as_object().unwrap());
    for case in fix["compact_after_reader"].as_array().unwrap() {
        let source = AuditSource::Reader(reader(&readers, case));
        let selected = objs(&case["selected"]);
        let got = verification::compact_after_source(&source, &selected);
        outcome(
            case,
            got,
            |(doc, identity)| json!({"document": doc, "identity": identity}),
            &format!("compact_after_reader {}", case["file"]),
        );
    }
}
