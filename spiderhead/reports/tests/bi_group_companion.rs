//! Ported tests for `reports/bi_index.rs` group companions
//! (reference the reference suite).
//!
//! `_population_scan` is realized on top of the retained raw-scan fixture plus
//! `write_audit_v2` — the same companion a native scan publishes — instead of
//! `NativeScan.create`/`save_audit_v2`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use spiderhead_reports::bi::project::{export_bi, ExportLimits};
use spiderhead_reports::bi::schema::Groups;
use spiderhead_reports::bi::writer::OutputBudget;
use spiderhead_reports::bi_index::{
    copy_group_members_companion, verify_group_member_references, verify_group_members_companion,
    write_group_members_companion, GroupIndex,
};
use spiderhead_store::audit_v2::{write_audit_v2, AuditV2Reader};

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bi-group-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn raw_scan() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../oracle/raw-scans/basic/scan.sqlite")
        .canonicalize()
        .expect("raw scan fixture")
}

/// The scan row's writer binding (uuid, revisions, version).
fn binding_for(scan: &Path) -> Map<String, Value> {
    let con = Connection::open_with_flags(scan, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("scan db");
    let (uuid, rev, ver, wrev): (String, i64, String, String) = con
        .query_row(
            "SELECT scan_uuid,evidence_revision,writer_version,writer_revision FROM scan WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("binding row");
    Map::from_iter([
        ("scan_uuid".into(), json!(uuid)),
        ("evidence_revision".into(), json!(rev)),
        ("analyzer_version".into(), json!(ver)),
        ("analyzer_revision".into(), json!(wrev)),
    ])
}

/// `_population_scan(path, count)`: a retained scan whose audit.v2 holds one
/// group of `count` member urls and one finding per member.
fn population_scan(dir: &Path, count: usize) -> (PathBuf, Vec<String>) {
    let scan = dir.join("scan.sqlite");
    std::fs::copy(raw_scan(), &scan).expect("copy scan");
    let _ = std::fs::remove_file(dir.join("scan.sqlite.audit-v2.sqlite"));
    let binding = binding_for(&scan);
    let urls: Vec<String> = (0..count)
        .map(|i| format!("https://example.test/{i:06}"))
        .collect();
    let issues: Vec<Value> = urls
        .iter()
        .enumerate()
        .map(|(i, url)| {
            json!({
                "id": format!("ISSUE-{i:06}"),
                "check": "TITLE_DUPLICATE",
                "severity": "warning",
                "source": "native",
                "message": "synthetic finding",
                "occurrences_count": 1,
                "target_url": url,
                "group_id": "GROUP",
            })
        })
        .collect();
    let pages: Vec<Value> = urls
        .iter()
        .map(|url| json!({"url": url, "status_code": 200, "metrics": {"representation": "static"}}))
        .collect();
    let groups = vec![json!({
        "group_id": "GROUP",
        "check": "TITLE_DUPLICATE",
        "count": count,
        "urls": urls,
    })];
    let header = json!({
        "schema_version": "2.0",
        "tool": {"name": "seohead", "version": binding["analyzer_version"]},
        "run": {
            "input_mode": "crawl",
            "source": "https://example.test/",
            "generated_at": "2026-10-08T00:00:00Z",
            "crawl_config": {},
            "crawl_partial": false,
            "scan_uuid": binding["scan_uuid"],
        },
        "summary": {
            "totals": {},
            "by_severity": {"critical": 0, "warning": 0, "notice": 0},
            "by_check": {},
            "health_score": 100,
        },
        "issues": [],
        "pages": [],
        "groups": [],
    });
    let mut collections: IndexMap<String, Vec<Value>> = IndexMap::new();
    collections.insert("/issues".into(), issues);
    collections.insert("/pages".into(), pages);
    collections.insert("/groups".into(), groups);
    write_audit_v2(&scan, header.as_object().unwrap(), &collections, &binding)
        .expect("write audit v2");
    (scan, urls)
}

/// `(headers, positional rows, dict rows)` for a BI partition file.
type DictRows = (Vec<String>, Vec<Vec<String>>, Vec<BTreeMap<String, String>>);

/// `csv.DictReader(stream)` rows from a BI partition file — every value stays
/// a string, as DictReader yields them.
fn dict_rows(path: &Path) -> DictRows {
    let mut reader = csv::Reader::from_path(path).expect("csv partition");
    let fields: Vec<String> = reader
        .headers()
        .expect("headers")
        .iter()
        .map(str::to_owned)
        .collect();
    let rows: Vec<Vec<String>> = reader
        .records()
        .map(|r| r.expect("csv row").iter().map(str::to_owned).collect())
        .collect();
    let dicts = rows
        .iter()
        .map(|row| fields.iter().cloned().zip(row.iter().cloned()).collect())
        .collect();
    (fields, rows, dicts)
}

fn write_partition(path: &Path, fields: &[String], rows: &[Vec<String>]) {
    let mut writer = csv::Writer::from_path(path).expect("csv writer");
    writer.write_record(fields).expect("header");
    for row in rows {
        writer.write_record(row).expect("row");
    }
    writer.flush().expect("flush");
}

#[test]
fn large_bi_groups_have_complete_companions_and_explicit_source_bound_references() {
    let dir = tmp("large");
    let (scan, urls) = population_scan(&dir, 200);
    let out = dir.join("bi");
    export_bi(
        Some(&scan),
        None,
        &[],
        Some(&out),
        ExportLimits {
            max_rows_per_file: 31,
            ..ExportLimits::default()
        },
    )
    .expect("export_bi");
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["datasets"].as_object().unwrap().len(), 6);
    let companion = &manifest["group_members"];
    assert_eq!(
        verify_group_members_companion(&out, companion)
            .expect("verify")
            .expect("descriptor")["members"],
        json!(urls.len() as i64)
    );
    assert_eq!(companion["groups"]["row_count"], json!(1));
    assert_eq!(companion["members"]["row_count"], json!(urls.len()));
    assert_eq!(
        verify_group_member_references(&out, &manifest)
            .expect("verify refs")
            .expect("descriptor")["members"],
        json!(urls.len() as i64)
    );

    let mut without_companion = manifest.clone();
    without_companion
        .as_object_mut()
        .unwrap()
        .remove("group_members");
    let message = verify_group_member_references(&out, &without_companion)
        .expect_err("missing companion must fail")
        .0;
    assert!(
        message.contains("lacks its complete companion"),
        "{message}"
    );

    let mut wrong_source = manifest.clone();
    wrong_source["run"]["audit_sha256"] = json!("b".repeat(64));
    let message = verify_group_member_references(&out, &wrong_source)
        .expect_err("foreign source must fail")
        .0;
    assert!(message.contains("bind"), "{message}");

    let reader = AuditV2Reader::new(&scan, true, false, None).expect("reader");
    assert_eq!(companion["source_audit_sha256"], json!(reader.sha256));

    for part in manifest["datasets"]["findings"]["partitions"]
        .as_array()
        .expect("partitions")
    {
        let (_fields, _rows, dicts) = dict_rows(&out.join(part["path"].as_str().unwrap()));
        for row in &dicts {
            let reference: Value =
                serde_json::from_str(&row["group_urls_json"]).expect("reference json");
            assert_eq!(reference["schema"], json!("bi-group-members.v1"));
            assert_eq!(reference["member_count"], json!(200));
            assert_eq!(reference["companion_sha256"], companion["sha256"]);
            assert_eq!(
                reference["source_audit_sha256"],
                companion["source_audit_sha256"]
            );
            assert_eq!(row["group_url_count"], "200");
        }
    }

    let copied = dir.join("copy");
    std::fs::create_dir(&copied).unwrap();
    copy_group_members_companion(&out, &copied, companion, &OutputBudget::new(10_000_000))
        .expect("copy companion");
    assert_eq!(
        verify_group_members_companion(&copied, companion).expect("verify copy"),
        verify_group_members_companion(&out, companion).expect("verify source")
    );
    let member_file = copied.join(
        companion["members"]["partitions"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["path"]
            .as_str()
            .unwrap(),
    );
    let bytes = std::fs::read(&member_file).unwrap();
    std::fs::write(&member_file, &bytes[..bytes.len() - 1]).unwrap();
    let message = verify_group_members_companion(&copied, companion)
        .expect_err("truncated member file must fail")
        .0;
    assert!(
        message.contains("digest") || message.contains("counts") || message.contains("checksum"),
        "{message}"
    );
}

#[test]
fn bi_index_retains_members_once_and_keeps_only_a_fixed_inline_population() {
    let dir = tmp("index");
    let mut sizes = Vec::new();
    for count in [100usize, 200] {
        let con = Connection::open_in_memory().expect("memory db");
        let urls: Vec<String> = (0..count)
            .map(|i| format!("https://example.test/{i:06}"))
            .collect();
        let mut groups = Groups::List(vec![json!({
            "group_id": "GROUP",
            "count": count,
            "urls": urls,
        })]);
        let mut index = GroupIndex::new(&con, &mut groups).expect("index");
        let directory = dir.join(count.to_string());
        std::fs::create_dir(&directory).unwrap();
        let companion = write_group_members_companion(
            &mut index,
            &directory,
            "test-run",
            Some(&"a".repeat(64)),
            50,
            1_000_000,
            &OutputBudget::new(10_000_000),
        )
        .expect("companion");
        for _ in 0..count {
            let group = index.get("GROUP").expect("get").expect("GROUP");
            assert!(group["urls"].is_object(), "urls must be a reference");
        }
        assert_eq!(companion["members"]["row_count"], json!(count));
        let size: i64 = con
            .query_row(
                "SELECT SUM(length(value_json)) FROM group_members",
                [],
                |r| r.get(0),
            )
            .expect("sum");
        sizes.push(size);
    }
    assert_eq!(sizes[1], sizes[0] * 2);
}

#[test]
fn finding_reference_must_agree_with_its_row_identity_and_count() {
    for (field, value) in [
        ("group_id", "OTHER"),
        ("run_id", "OTHER"),
        ("group_url_count", "29"),
    ] {
        let dir = tmp(&format!("ref-{field}"));
        let (scan, _urls) = population_scan(&dir, 30);
        let directory = dir.join("bi");
        export_bi(
            Some(&scan),
            None,
            &[],
            Some(&directory),
            ExportLimits::default(),
        )
        .expect("export_bi");
        let manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(directory.join("manifest.json")).unwrap(),
        )
        .unwrap();
        let part = directory.join(
            manifest["datasets"]["findings"]["partitions"][0]["path"]
                .as_str()
                .unwrap(),
        );
        let (fields, mut records, _dicts) = dict_rows(&part);
        records[0][fields.iter().position(|f| f == field).expect("field")] = value.to_owned();
        write_partition(&part, &fields, &records);
        let message = verify_group_member_references(&directory, &manifest)
            .expect_err("tampered row must fail")
            .0;
        assert!(message.contains("row differs"), "{field}: {message}");
    }
}

#[test]
fn small_group_with_large_atomic_members_uses_complete_companion_not_oversized_cell() {
    let dir = tmp("large-members");
    let con = Connection::open_in_memory().expect("memory db");
    let urls: Vec<String> = (0..6)
        .map(|i| format!("https://example.test/{i}/{}", "x".repeat(1_500_000)))
        .collect();
    let mut groups = Groups::List(vec![json!({
        "group_id": "GROUP",
        "count": 6,
        "urls": urls,
    })]);
    let mut index = GroupIndex::new(&con, &mut groups).expect("index");
    let descriptor = write_group_members_companion(
        &mut index,
        &dir,
        "test-run",
        Some(&"a".repeat(64)),
        25,
        8 * 1024 * 1024,
        &OutputBudget::new(32 * 1024 * 1024),
    )
    .expect("companion");
    assert_eq!(descriptor["inline_member_bytes"], json!(64 * 1024));
    assert_eq!(descriptor["members"]["row_count"], json!(6));
    assert!(
        descriptor["members"]["bytes"].as_u64().unwrap() > 8 * 1024 * 1024,
        "{:?}",
        descriptor["members"]
    );
    let group = index.get("GROUP").expect("get").expect("GROUP");
    assert_eq!(group["urls"]["schema"], json!("bi-group-members.v1"));
    assert_eq!(
        verify_group_members_companion(&dir, &descriptor)
            .expect("verify")
            .expect("descriptor")["members"],
        json!(6)
    );
}
