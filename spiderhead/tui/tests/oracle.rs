//! Frame, text and state parity of the terminal shell against fixtures
//! rendered by the reference implementation (the reference suite).
//!
//! The fixture carries the observer snapshots the reference rendered from,
//! so every case here is a pure function of recorded input: no project
//! directory, clock or terminal is read.

#![cfg(unix)]

use std::sync::{Mutex, MutexGuard};

use serde_json::{json, Value};
use spiderhead_tui::panels::{self, MIN_HEIGHT, MIN_WIDTH};
use spiderhead_tui::render::{render_lines, Options};
use spiderhead_tui::{app, labels, localization, pyjson, scan_now, theme, watch_views, ShellState};

/// The UI language is process-global; serialise the tests that touch it.
static LANGUAGE: Mutex<()> = Mutex::new(());

fn language_guard() -> MutexGuard<'static, ()> {
    let guard = LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
    localization::set_language("en");
    guard
}

fn fixture() -> Value {
    let text = include_str!("fixtures/tui_oracle.json");
    serde_json::from_str(text).expect("fixture parses")
}

fn plain_palette() -> theme::Palette {
    theme::resolve_palette(false)
}

fn strs(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|v| v.as_str().expect("string").to_string())
        .collect()
}

/// Build a state the way the reference constructor does: defaults, then the
/// keyword overrides, with `commands` merged with the group commands.
fn make_state(kwargs: &Value) -> ShellState {
    let commands = kwargs.get("commands").map(strs).unwrap_or_default();
    let base = ShellState::new(commands);
    let mut doc = serde_json::to_value(&base).expect("state serialises");
    if let Some(map) = kwargs.as_object() {
        for (key, value) in map {
            if key != "commands" {
                doc[key] = value.clone();
            }
        }
    }
    serde_json::from_value(doc).expect("state deserialises")
}

/// State as the reference's `vars()`: page identity is a nested tuple there.
fn state_doc(state: &ShellState) -> Value {
    let mut doc = serde_json::to_value(state).expect("state serialises");
    let identity = state
        .watch_page_identity
        .as_ref()
        .map(|p| json!([[p.section, p.offset, p.query], p.identities]));
    doc["watch_page_identity"] = identity.unwrap_or(Value::Null);
    doc
}

fn assert_state(actual: &ShellState, expected: &Value, name: &str) {
    let mut want = expected.clone();
    if let Some(map) = want.as_object_mut() {
        map.remove("_all");
    }
    assert_eq!(state_doc(actual), want, "state after {name}");
}

fn snapshot_for(fx: &Value, project: &str) -> Value {
    let key = match project {
        "bare" => "snapshot",
        "scan" => "scan_snapshot",
        _ => "sites_snapshot",
    };
    fx[key].clone()
}

fn plain_lines(lines: &[spiderhead_tui::text::Text]) -> Vec<String> {
    lines.iter().map(|t| t.plain.clone()).collect()
}

#[test]
fn watch_sections_match_reference_lines() {
    let _g = language_guard();
    let fx = fixture();
    let palette = plain_palette();
    for case in fx["watch_cases"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let project = case["project"].as_str().expect("project");
        let section = case["section"].as_str().expect("section");
        let index = case.get("init_index").and_then(Value::as_i64).unwrap_or(0);
        let mut state = make_state(&json!({
            "view": "watch", "watch_section": section, "watch_index": index
        }));
        let data = fx["pages"][project]
            .get(section)
            .cloned()
            .unwrap_or(json!({}));
        let snapshot = snapshot_for(&fx, project);
        let lines = watch_views::watch_lines(
            "unused",
            &mut state,
            &palette,
            None,
            Some(&snapshot),
            Some(&data),
        );
        assert_eq!(plain_lines(&lines), strs(&case["lines"]), "{name}: lines");
        assert_state(&state, &case["state"], name);
    }
}

#[test]
fn watch_details_match_reference_lines() {
    let _g = language_guard();
    let fx = fixture();
    let palette = plain_palette();
    for case in fx["detail_cases"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let project = case["project"].as_str().expect("project");
        let kind = case["kind"].as_str().expect("kind");
        let snapshot = snapshot_for(&fx, project);
        let data = case["data"].clone();
        let mut state = if kind == "site" {
            let index = case["init_index"].as_i64().expect("index");
            let mut state = make_state(&json!({
                "view": "watch", "watch_section": "sites", "watch_index": index
            }));
            watch_views::watch_lines(
                "unused",
                &mut state,
                &palette,
                None,
                Some(&snapshot),
                Some(&json!({})),
            );
            state.handle_key("enter");
            state
        } else {
            let mut kwargs = json!({"view": "watch_detail"});
            let source = if case.get("kwargs").is_some() {
                &case["kwargs"]
            } else {
                &case["state"]
            };
            for key in [
                "watch_section",
                "watch_item_id",
                "watch_inbox_entry_id",
                "watch_selected_scan_uuid",
                "watch_detail_ordinal",
            ] {
                if let Some(v) = source.get(key) {
                    kwargs[key] = v.clone();
                }
            }
            if name == "detail:scan" {
                kwargs["watch_section"] = json!("scans");
                let first = snapshot["scans"]["items"][0]["uuid"].clone();
                kwargs["watch_selected_scan_uuid"] = first;
            }
            kwargs["watch_detail_kind"] = json!(kind);
            make_state(&kwargs)
        };
        let lines = watch_views::watch_detail_lines(
            "unused",
            &mut state,
            &palette,
            Some(&snapshot),
            Some(&data),
        );
        assert_eq!(plain_lines(&lines), strs(&case["lines"]), "{name}: lines");
        assert_state(&state, &case["state"], name);
    }
}

fn frame_text(fx: &Value, case: &Value) -> String {
    let palette = plain_palette();
    let name = case["name"].as_str().expect("name");
    let width = case["width"].as_u64().expect("width") as usize;
    let height = case["height"].as_u64().expect("height") as usize;
    let project = case["project"].as_str().expect("project");
    let mut state = if let Some(view) = case.get("view").and_then(Value::as_str) {
        let mut kwargs = case["kwargs"].clone();
        kwargs["view"] = json!(view);
        make_state(&kwargs)
    } else if name == "failure:watch" || name == "bare:overview:80x24" {
        make_state(&json!({"view": "watch"}))
    } else {
        make_state(&json!({
            "view": "watch", "watch_section": case["section"]
        }))
    };
    let snapshot = if name == "failure:watch" {
        None
    } else {
        Some(snapshot_for(fx, project))
    };
    let data = match case.get("data_key").and_then(Value::as_str) {
        Some(section) => fx["pages"][project]
            .get(section)
            .cloned()
            .unwrap_or(json!({})),
        None => json!({}),
    };
    let frame = panels::build_frame(
        &mut state,
        width,
        height,
        &palette,
        Some("unused-project-dir"),
        None,
        Some(&(snapshot, Vec::new())),
        Some(&data),
        false,
        None,
    );
    let lines = render_lines(
        &frame,
        &Options {
            max_width: width,
            height: Some(height),
            ..Default::default()
        },
    );
    // The reference exports the printed frame without right padding.
    let mut out = String::new();
    for line in lines {
        let text: String = line.iter().map(|s| s.text.as_str()).collect();
        out.push_str(text.trim_end());
        out.push('\n');
    }
    out
}

fn trim_rows(text: &str) -> String {
    let mut rows: Vec<&str> = text.lines().map(str::trim_end).collect();
    while rows.last() == Some(&"") {
        rows.pop();
    }
    rows.iter().map(|l| format!("{l}\n")).collect()
}

#[test]
fn frames_match_reference_text() {
    let _g = language_guard();
    let fx = fixture();
    for case in fx["frame_cases"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let want = case["text"].as_str().expect("text");
        let mut got = trim_rows(&frame_text(&fx, case));
        let mut want = trim_rows(want);
        if name == "view:detail" {
            // Below row 7 the reference prints the CLI's own argparse help,
            // which the CLI crate supplies through `CommandDocs`.
            let head = |t: &str| {
                t.lines()
                    .take(7)
                    .map(|l| format!("{l}\n"))
                    .collect::<String>()
            };
            (got, want) = (head(&got), head(&want));
        }
        if let Some((row, (g, w))) = got
            .lines()
            .zip(want.lines())
            .enumerate()
            .find(|(_, (g, w))| g != w)
        {
            panic!("{name}: row {row} differs\n got: {g}\nwant: {w}");
        }
        assert_eq!(got, want, "{name}: frame text");
    }
}

#[test]
fn too_small_terminal_shows_notice() {
    let _g = language_guard();
    let fx = fixture();
    let case = fx["frame_cases"]
        .as_array()
        .expect("cases")
        .iter()
        .find(|c| c["width"].as_u64() == Some(MIN_WIDTH as u64 - 1))
        .expect("small case");
    assert!(case["height"].as_u64().expect("h") < MIN_HEIGHT as u64);
    assert!(frame_text(&fx, case).contains("terminal too small"));
}

#[test]
fn state_replays_match_reference() {
    let _g = language_guard();
    let fx = fixture();
    for case in fx["state_cases"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let mut kwargs = case["kwargs"].clone();
        if let Some(pre) = case.get("pre_state") {
            for (k, v) in pre.as_object().expect("pre") {
                kwargs[k] = v.clone();
            }
        }
        let mut state = make_state(&kwargs);
        for key in strs(&case["keys"]) {
            state.handle_key(&key);
        }
        assert_state(&state, &case["expect"], name);
    }
}

fn pipe_pair() -> (i32, i32) {
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    (fds[0], fds[1])
}

fn from_hex(hex: &str) -> Vec<u8> {
    (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

#[test]
fn read_key_streams_match_reference() {
    let fx = fixture();
    for case in fx["key_cases"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let (reader, writer) = pipe_pair();
        let bytes = from_hex(case["bytes"].as_str().expect("bytes"));
        if !bytes.is_empty() {
            assert_eq!(
                unsafe { libc::write(writer, bytes.as_ptr().cast(), bytes.len()) },
                bytes.len() as isize
            );
        }
        let mut got = Vec::new();
        for timeout in case["reads"].as_array().expect("reads") {
            let timeout = timeout.as_f64();
            got.push(spiderhead_tui::keys::read_key(reader, timeout).expect("read"));
        }
        unsafe {
            libc::close(reader);
            libc::close(writer);
        }
        assert_eq!(got, strs(&case["expect"]), "{name}: keys");
    }
}

#[test]
fn value_helpers_match_reference() {
    let _g = language_guard();
    let fx = fixture();
    let values = &fx["values"];
    let palette = plain_palette();

    let meters = [
        ("Scenarios", Some(5), Some(10)),
        ("Sitemap", None, None),
        ("Discovered URL coverage", Some(100), Some(1097)),
        ("Zero", Some(0), Some(0)),
        ("Partial", Some(3), Some(7)),
    ];
    let want = strs(&values["meter"]);
    for (i, (label, done, total)) in meters.iter().enumerate() {
        assert_eq!(
            panels::meter(label, *done, *total, &palette).plain,
            want[i],
            "meter {i}"
        );
    }

    let telemetry = [
        json!({"current_rate_per_second": 20.0, "unit": "urls_including_resources"}),
        json!({"current_rate_per_second": null, "unit": "urls_including_resources"}),
        json!({"current_rate_per_second": 4.0}),
        json!({}),
    ];
    let want = strs(&values["rate_text"]);
    for (i, t) in telemetry.iter().enumerate() {
        assert_eq!(watch_views::rate_text(t), want[i], "rate_text {i}");
    }

    let runs = [
        json!({"kind": "screaming_frog", "state": "running", "telemetry": {"state": "fresh", "current_rate_per_second": 2.5}}),
        json!({"kind": "native", "state": "failed", "collector": {"mode": "spider"}, "telemetry": {"state": "stale", "current_rate_per_second": null}}),
        json!({"kind": "native", "state": "running", "site_label": "Own", "scenario": {"id": "scenario:x"}, "telemetry": {}, "next_action": "wait"}),
    ];
    let want = strs(&values["run_summary"]);
    for (i, item) in runs.iter().enumerate() {
        assert_eq!(watch_views::run_summary(item), want[i], "run_summary {i}");
    }

    let separate = json!({"runs": {"items": [{"id": "0143aa", "state": "running",
        "collector": {"mode": "list"},
        "counters": {"fetched": 1330, "queued": 3670, "inflight": 0},
        "telemetry": {"queue_semantics": "separate", "state": "fresh"}}]},
        "scans": {"items": []}});
    let unknown = json!({"runs": {"items": [{"id": "one", "state": "running",
        "counters": {"fetched": 4, "queued": 5, "inflight": 1},
        "telemetry": {"queue_semantics": "unknown", "state": "fresh"}}]},
        "scans": {"items": []}});
    let summaries = [
        None,
        Some(fx["snapshot"].clone()),
        Some(fx["scan_snapshot"].clone()),
        Some(separate.clone()),
        Some(unknown),
    ];
    let want = strs(&values["summary"]);
    for (i, snap) in summaries.iter().enumerate() {
        assert_eq!(scan_now::summary(snap.as_ref()), want[i], "summary {i}");
    }

    for case in values["select"].as_array().expect("select") {
        let picked = scan_now::select(&case["in"], case["scan"].as_str()).expect("select");
        let kept = pyjson::list_of(&picked["scans"], "items").len()
            + pyjson::list_of(&picked["runs"], "items").len();
        assert_eq!(kept, 1, "select keeps the matching entry only");
        assert_eq!(picked["selected_scan"], case["scan"], "select");
    }

    let raw = [
        "running",
        "stale",
        "screaming_frog",
        "sf_exports",
        "custom_state",
    ];
    let want = strs(&values["labels"]["label"]);
    for (i, v) in raw.iter().enumerate() {
        assert_eq!(labels::label(&json!(v)), want[i], "label {v}");
    }
    assert_eq!(labels::label(&Value::Null), want[5]);
    assert_eq!(labels::label(&json!("")), want[6]);
    for (i, v) in ["critical", "proposed_goal", "primary"].iter().enumerate() {
        assert_eq!(labels::label(&json!(v)), want[7 + i], "label {v}");
    }
    let titles = [
        "structured-data",
        "custom_late",
        "Already spaced",
        "custom:late",
    ];
    let want = strs(&values["labels"]["title"]);
    for (i, v) in titles.iter().enumerate() {
        assert_eq!(labels::title(v), want[i], "title {v}");
    }

    for (kind, want) in values["badge"].as_object().expect("badge") {
        let got = theme::badge(kind, "label", &palette).expect("badge");
        assert_eq!(&got, want.as_str().expect("str"), "badge {kind}");
    }

    // Without a command table the leaf help falls back to a pointer; the
    // argparse text itself comes from the CLI crate when the shell is mounted.
    let reference = panels::command_reference("sf", None);
    assert_eq!(reference[..5], strs(&values["command_reference"])[..5]);
    assert_eq!(reference[5], "seohead sf --help");

    assert_eq!(
        localization::number(1330),
        values["number"]["en"].as_str().expect("en")
    );
    localization::set_language("ru");
    assert_eq!(
        localization::number(1330),
        values["number"]["ru"].as_str().expect("ru")
    );
    assert_eq!(
        scan_now::summary(Some(&separate)),
        values["summary_ru"].as_str().expect("ru summary")
    );
    localization::set_language("en");
}

#[test]
fn language_resolution_follows_locale_names() {
    let _g = language_guard();
    let fx = fixture();
    assert!(localization::resolve_language(Some("de")).is_err());
    assert_eq!(
        localization::resolve_language(Some("ru")).expect("ru"),
        "ru"
    );
    let saved: Vec<_> = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .map(|k| (*k, std::env::var(k).ok()))
        .collect();
    for (locale, want) in fx["values"]["resolve_language"].as_object().expect("map") {
        std::env::set_var("LC_ALL", locale);
        std::env::set_var("LC_MESSAGES", "");
        std::env::set_var("LANG", "");
        let got = localization::resolve_language(None).expect("resolve");
        assert_eq!(&got, want.as_str().expect("str"), "{locale}");
    }
    for (key, value) in saved {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}

#[test]
fn saved_notes_carry_reference_receipts() {
    let _g = language_guard();
    let fx = fixture();
    for case in fx["note_cases"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("project");
        spiderhead_projects::workspace::create_project(
            &project,
            "https://example.test/",
            None,
            None,
            None,
            None,
        )
        .expect("create project");
        let mut state = make_state(&case["kwargs"]);
        let message = app::save_note(project.to_str().expect("utf8"), &mut state);
        let (prefix, rest) = message.split_once(" · ").expect("receipt shape");
        assert_eq!(prefix, case["message_prefix"], "{name}");
        assert!(
            message.ends_with(case["message_suffix"].as_str().expect("suffix")),
            "{name}"
        );
        assert!(!rest.is_empty());
        let listed = spiderhead_projects::inbox::list_entries(&project, "oracle", 0, 50, true)
            .expect("list");
        let entries = pyjson::list_of(&listed, "entries");
        let last = entries.last().expect("entry");
        assert_eq!(last["references"], case["references"], "{name}: references");
        assert_eq!(last["kind"], case["kind"], "{name}: kind");
    }
}
