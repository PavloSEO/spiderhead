//! Output layer: JSON identical to the reference `json.dump(..., ensure_ascii=False, indent=2)`.

mod common;

use serde_json::Value;
use spiderhead_cli::{render_csv, render_json, render_table, Format, Output};

#[test]
fn json_matches_reference_for_every_sample() {
    let cases = common::fixture("output_cases.json");
    let cases = cases.as_array().map_or(&[][..], Vec::as_slice);
    assert!(cases.len() >= 10);
    for case in cases {
        let data: Value = serde_json::from_str(case["data_json"].as_str().unwrap_or("null"))
            .unwrap_or(Value::Null);
        let want = case["stdout"].as_str().unwrap_or_default();
        let got = render_json(&data);
        assert_eq!(got, want, "{}", common::diff(want, &got));
        let mut out = Output::new(Format::Json);
        out.emit(&data);
        assert_eq!(out.stdout, want);
    }
}

#[test]
fn csv_and_table_render_rows() {
    let data: Value = serde_json::json!([{"url": "https://e.com/", "status": 200, "title": "a,b"}, {"url": "/x", "status": 404, "extra": null}]);
    assert_eq!(
        render_csv(&data),
        "url,status,title,extra\nhttps://e.com/,200,\"a,b\",\n/x,404,,\n"
    );
    assert_eq!(
        render_table(&data),
        "url             status  title  extra\n--------------  ------  -----  -----\nhttps://e.com/  200     a,b\n/x              404\n"
    );
    assert_eq!(
        render_csv(&serde_json::json!({"k": "v\"q"})),
        "key,value\nk,\"v\"\"q\"\n"
    );
    assert_eq!("table".parse::<Format>().ok(), Some(Format::Table));
    assert!("xml".parse::<Format>().is_err());
}
