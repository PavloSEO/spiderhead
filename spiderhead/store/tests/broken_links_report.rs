//! The broken links and resources report over the retained `errors` scan.

use std::path::{Path, PathBuf};

use spiderhead_store::broken_links::{collect, write_report};
use spiderhead_store::scan_artifact::open_scan;

fn scan_copy(dir: &Path) -> PathBuf {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle/raw-scans/errors/scan.sqlite");
    let target = dir.join("scan.sqlite");
    std::fs::copy(source, &target).unwrap();
    target
}

#[test]
fn error_pages_become_rows_with_status_type_and_anchor() {
    let dir = tempfile::tempdir().unwrap();
    let scan = scan_copy(dir.path());
    let con = open_scan(&scan, false, None).unwrap();
    let rows = collect(&con).unwrap();
    let nope: Vec<_> = rows
        .iter()
        .filter(|r| r.target.ends_with("/nope.html"))
        .collect();
    assert_eq!(nope.len(), 2, "one row per referencing page: {rows:#?}");
    assert!(nope.iter().all(|r| r.status == 404 && r.kind == "page"));
    assert!(nope
        .iter()
        .any(|r| r.anchor_or_alt == "Broken link inside page"));
    let statuses: std::collections::BTreeSet<i64> = rows.iter().map(|r| r.status).collect();
    assert_eq!(statuses, [403, 404, 410, 500, 503].into_iter().collect());
    assert!(rows.iter().all(|r| !r.target.ends_with("/ok.html")));
}

#[test]
fn csv_and_xlsx_files_are_written_once_and_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let scan = scan_copy(dir.path());
    let csv = dir.path().join("broken.csv");
    let result = write_report(&scan, &csv, "csv").unwrap();
    assert_eq!(result["ok"], true);
    // The shared CSV writer emits a BOM and semicolons, as the other exports do.
    let text = std::fs::read_to_string(&csv)
        .unwrap()
        .replace('\u{feff}', "");
    assert_eq!(
        text.lines().next().unwrap(),
        "source_page;target;status;type;anchor_or_alt"
    );
    assert_eq!(
        text.lines().count() as i64,
        1 + result["rows"].as_i64().unwrap()
    );
    assert!(text.contains("/nope.html;404;page;Broken link inside page"));

    let xlsx = dir.path().join("broken.xlsx");
    write_report(&scan, &xlsx, "xlsx").unwrap();
    assert!(std::fs::metadata(&xlsx).unwrap().len() > 0);

    assert!(write_report(&scan, &csv, "csv").is_err());
    assert!(write_report(&scan, &dir.path().join("x.bin"), "ods").is_err());
}

#[test]
fn failed_external_checks_and_resources_are_reported_with_their_type() {
    let dir = tempfile::tempdir().unwrap();
    let scan = scan_copy(dir.path());
    // The extension tables are not part of the strict v1 schema, so the rows are read
    // through a plain connection, as a v2 writer would leave them.
    let con = rusqlite::Connection::open(&scan).unwrap();
    {
        spiderhead_store::external_checks::ensure_schema(&con).unwrap();
        let page: i64 = con
            .query_row(
                "SELECT url_id FROM urls WHERE url LIKE '%/ok.html'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let add = |url: &str| -> i64 {
            con.execute("INSERT INTO urls(url) VALUES (?1)", [url])
                .unwrap();
            con.last_insert_rowid()
        };
        let external = add("https://elsewhere.test/gone");
        let script = add("http://127.0.0.1:18702/app.js");
        con.execute(
            "INSERT INTO external_checks VALUES (0,'fetched',?1)",
            [r#"{"url":"https://elsewhere.test/gone","status_code":404}"#],
        )
        .unwrap();
        con.execute(
            "INSERT INTO links (source_url_id,destination_url_id,evidence_representation,ordinal,anchor,anchor_source,nofollow,position,rel_json,target,raw_href) \
             VALUES (?1,?2,'static',99,'Partner','',0,'','[]','','')",
            [page, external],
        )
        .unwrap();
        con.execute(
            "INSERT INTO responses (request_url_id,request_ordinal,redirect_chain_json,method,purpose,requested_at,\
             request_headers_redacted_json,credentials_used,variant_key,status_code,effective_status_code,\
             response_headers_redacted_json,effective_headers_redacted_json,content_type,charset,content_encoding,\
             transport_source,cache_status,body_fidelity,body_state,body_reason,error,error_kind) \
             VALUES (?1,900,'[]','GET','script','2026-01-01T00:00:00Z','[]',0,'',404,404,'[]','[]','','','',\
             'network','','unavailable','unavailable','fetch_failed','','')",
            [script],
        )
        .unwrap();
        let response = con.last_insert_rowid();
        con.execute(
            "INSERT INTO resource_refs (page_url_id,ordinal,resource_url_id,kind,representation,raw_url,response_id,capture_state,reason) \
             VALUES (?1,0,?2,'script','static','/app.js',?3,'fetch_failed','')",
            [page, script, response],
        )
        .unwrap();
    }
    let rows = collect(&con).unwrap();
    let external = rows
        .iter()
        .find(|r| r.target.contains("elsewhere"))
        .unwrap();
    assert_eq!((external.status, external.kind.as_str()), (404, "external"));
    assert_eq!(external.anchor_or_alt, "Partner");
    let script = rows.iter().find(|r| r.target.ends_with("/app.js")).unwrap();
    assert_eq!((script.status, script.kind.as_str()), (404, "js"));
}
