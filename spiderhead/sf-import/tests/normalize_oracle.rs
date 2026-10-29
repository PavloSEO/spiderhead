//! Differential test against the reference baseline for the reference suite.
//!
//! Fixture: `tests/fixtures/normalize.json`, produced by the reference suite.

use serde_json::{json, Map, Value};
use spiderhead_sf_import::normalize as n;
use spiderhead_sf_import::table::Table;
use std::collections::BTreeMap;

fn fixture() -> Value {
    let text = include_str!("fixtures/normalize.json");
    serde_json::from_str(text).expect("fixture parses")
}

/// JSON comparison where a fixture `int` equals a computed integral `float`
/// (pandas dtype details are invisible in JSON but the float/int repr differs —
/// the fixture records the exact reference repr, so we compare strictly).
fn assert_eq_strict(got: &Value, want: &Value, ctx: &str) {
    assert_eq!(got, want, "mismatch at {ctx}");
}

fn small_map(fx: &Value) -> Vec<(String, Vec<String>)> {
    fx["small_map"]
        .as_object()
        .expect("map")
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                v.as_array()
                    .expect("candidates")
                    .iter()
                    .map(|c| c.as_str().expect("str").to_owned())
                    .collect(),
            )
        })
        .collect()
}

fn internal_map(fx: &Value) -> Vec<(String, Vec<String>)> {
    fx["internal_field_map"]
        .as_object()
        .expect("map")
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                v.as_array()
                    .expect("candidates")
                    .iter()
                    .map(|c| c.as_str().expect("str").to_owned())
                    .collect(),
            )
        })
        .collect()
}

fn table(fx: &Value) -> Table {
    let columns: Vec<String> = fx["columns"]
        .as_array()
        .expect("cols")
        .iter()
        .map(|c| c.as_str().expect("str").to_owned())
        .collect();
    let rows: Vec<Vec<Value>> = fx["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|r| {
            columns
                .iter()
                .map(|c| r.get(c).cloned().unwrap_or(Value::Null))
                .collect()
        })
        .collect();
    Table::new(columns, rows)
}

#[test]
fn comma_to_dot() {
    let fx = fixture();
    for (input, want) in fx["comma_to_dot"].as_object().expect("map") {
        assert_eq!(
            n::comma_to_dot(input),
            want.as_str().expect("str"),
            "comma_to_dot({input})"
        );
    }
}

#[test]
fn normalize_value() {
    let fx = fixture();
    let values = fx["normalize_value"].as_array().expect("list");
    let inputs = json!([
        null, "", "   ", " x ", "2,500", "-0,000", "1,2,3", "1.2,3", 0, 1, 2.5, -7, true,
        false, "42", "4.5e2", "1e999", "-1e999", "inf", "-Infinity", "nan", "1_000",
        "1_0.5", "1_.5", "_5", "5_", "1__0", ".5", "5.", "5.e2", "0x10", "abc", "12 34",
        " 12 ", [1], {"a": 1}, []
    ]);
    for (i, (input, want)) in inputs
        .as_array()
        .expect("list")
        .iter()
        .zip(values)
        .enumerate()
    {
        assert_eq_strict(
            &n::normalize_value(input),
            want,
            &format!("normalize_value[{i}]"),
        );
    }
}

#[test]
fn to_float_and_to_int() {
    let fx = fixture();
    let inputs = json!([
        null, "", "   ", " x ", "2,500", "-0,000", "1,2,3", "1.2,3", 0, 1, 2.5, -7, true,
        false, "42", "4.5e2", "1e999", "-1e999", "inf", "-Infinity", "nan", "1_000",
        "1_0.5", "1_.5", "_5", "5_", "1__0", ".5", "5.", "5.e2", "0x10", "abc", "12 34",
        " 12 ", [1], {"a": 1}, []
    ]);
    let floats = fx["to_float"].as_array().expect("list");
    let ints = fx["to_int"].as_array().expect("list");
    for (i, input) in inputs.as_array().expect("list").iter().enumerate() {
        let want_f = &floats[i];
        match want_f {
            Value::Null => assert_eq!(n::to_float(input), None, "to_float[{i}]"),
            Value::Number(x) => {
                assert_eq!(n::to_float(input), x.as_f64(), "to_float[{i}] ({input})")
            }
            other => panic!("unexpected fixture {other}"),
        }
        let want_i = &ints[i];
        match want_i {
            Value::Null => assert_eq!(n::to_int(input), None, "to_int[{i}]"),
            Value::Number(x) => assert_eq!(n::to_int(input), x.as_i64(), "to_int[{i}] ({input})"),
            other => panic!("unexpected fixture {other}"),
        }
    }
}

#[test]
fn is_true() {
    let fx = fixture();
    let inputs = json!([
        null, "", "   ", " x ", "2,500", "-0,000", "1,2,3", "1.2,3", 0, 1, 2.5, -7, true,
        false, "42", "4.5e2", "1e999", "-1e999", "inf", "-Infinity", "nan", "1_000",
        "1_0.5", "1_.5", "_5", "5_", "1__0", ".5", "5.", "5.e2", "0x10", "abc", "12 34",
        " 12 ", [1], {"a": 1}, []
    ]);
    for (i, (input, want)) in inputs
        .as_array()
        .expect("list")
        .iter()
        .zip(fx["is_true"].as_array().expect("list"))
        .enumerate()
    {
        assert_eq!(
            n::is_true(input),
            want.as_bool().expect("bool"),
            "is_true[{i}] ({input})"
        );
    }
}

#[test]
fn norm_url() {
    let fx = fixture();
    for (i, case) in fx["norm_url"].as_array().expect("list").iter().enumerate() {
        let input = case["in"].as_str();
        let got = n::norm_url(input).expect("norm_url");
        assert_eq!(got, case["out"].as_str().expect("str"), "norm_url case {i}");
    }
}

#[test]
fn find_column() {
    let fx = fixture();
    let columns: Vec<String> = [
        "address",
        "Status Code",
        "TITLE 1",
        "status code",
        "X-Robots-Tag 1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for (i, case) in fx["find_column"]
        .as_array()
        .expect("list")
        .iter()
        .enumerate()
    {
        let cands: Vec<String> = case["candidates"]
            .as_array()
            .expect("list")
            .iter()
            .map(|c| c.as_str().expect("str").to_owned())
            .collect();
        let got = n::find_column(&columns, &cands);
        match &case["hit"] {
            Value::Null => assert_eq!(got, None, "find_column case {i}"),
            Value::String(s) => assert_eq!(got, Some(s.as_str()), "find_column case {i}"),
            other => panic!("unexpected {other}"),
        }
    }
}

#[test]
fn resolve_columns() {
    let fx = fixture();
    let columns: Vec<String> = [
        "address",
        "Status Code",
        "TITLE 1",
        "status code",
        "X-Robots-Tag 1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let map = small_map(&fx);
    let resolved = n::resolve_columns(&columns, &map);
    let want = fx["resolve_columns"].as_object().expect("map");
    let got: BTreeMap<String, Option<String>> = resolved
        .iter()
        .map(|(f, c)| (f.clone(), c.map(str::to_owned)))
        .collect();
    let want: BTreeMap<String, Option<String>> = want
        .iter()
        .map(|(f, c)| (f.clone(), c.as_str().map(str::to_owned)))
        .collect();
    assert_eq!(got, want);
}

#[test]
fn records_and_iter_and_mapping() {
    let fx = fixture();
    let t = table(&fx);
    let internal = internal_map(&fx);
    let small = small_map(&fx);

    let got_records = n::records_from_df(&t, &internal);
    let want_records = fx["records_from_df"].as_array().expect("list");
    assert_eq!(got_records.len(), want_records.len());
    for (i, (got, want)) in got_records.iter().zip(want_records).enumerate() {
        assert_eq!(&Value::Object(got.clone()), want, "records_from_df row {i}");
    }

    let got_iter: Vec<Map<String, Value>> = n::iter_records_from_df(&t, &internal).collect();
    let want_iter = fx["iter_records_from_df"].as_array().expect("list");
    assert_eq!(got_iter.len(), want_iter.len());
    for (i, (got, want)) in got_iter.iter().zip(want_iter).enumerate() {
        assert_eq!(
            &Value::Object(got.clone()),
            want,
            "iter_records_from_df row {i}"
        );
    }

    let rows = fx["rows"].as_array().expect("rows");
    let want_map = fx["record_from_mapping"].as_array().expect("list");
    let want_row = fx["row_to_record"].as_array().expect("list");
    for (i, (row, want)) in rows.iter().zip(want_map).enumerate() {
        let obj = row.as_object().expect("row object");
        let got = n::record_from_mapping(obj, &small);
        assert_eq!(&Value::Object(got), want, "record_from_mapping row {i}");
        let got2 = n::row_to_record(obj, &small);
        assert_eq!(&Value::Object(got2), &want_row[i], "row_to_record row {i}");
    }
}
