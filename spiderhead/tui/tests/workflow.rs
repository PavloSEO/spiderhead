//! Terminal workflows: key handling, editing, saving notes, scanned-project
//! frames and the compact scan line.

#![cfg(unix)]

use std::os::unix::io::RawFd;
use std::sync::{Mutex, MutexGuard};

use serde_json::{json, Value};
use spiderhead_tui::keys::{read_key, RawMode};
use spiderhead_tui::panels::{self, MIN_HEIGHT, MIN_WIDTH};
use spiderhead_tui::render::{render_lines, Options};
use spiderhead_tui::{app, localization, pyjson, scan_now, theme, watch_views, ShellState};

static LANGUAGE: Mutex<()> = Mutex::new(());

fn guard() -> MutexGuard<'static, ()> {
    let g = LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
    localization::set_language("en");
    g
}

fn state(view: &str) -> ShellState {
    let mut s = ShellState::new(Vec::new());
    s.view = view.into();
    s
}

fn palette() -> theme::Palette {
    theme::resolve_palette(false)
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/tui_oracle.json")).expect("fixture parses")
}

fn pipe() -> (RawFd, RawFd) {
    let mut fds = [0 as RawFd; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    (fds[0], fds[1])
}

fn put(fd: RawFd, bytes: &[u8]) {
    assert_eq!(
        unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) },
        bytes.len() as isize
    );
}

fn close(fds: &[RawFd]) {
    for fd in fds {
        unsafe { libc::close(*fd) };
    }
}

fn new_project(dir: &std::path::Path, target: &str) -> std::path::PathBuf {
    let root = dir.join("project");
    spiderhead_projects::workspace::create_project(&root, target, None, None, None, None)
        .expect("create project");
    root
}

fn frame_rows(
    state: &mut ShellState,
    width: usize,
    height: usize,
    project: Option<&str>,
    snapshot: Option<&(Option<Value>, Vec<spiderhead_tui::text::Text>)>,
    data: Option<&Value>,
    message: Option<&str>,
) -> Vec<String> {
    let frame = panels::build_frame(
        state,
        width,
        height,
        &palette(),
        project,
        message,
        snapshot,
        data,
        false,
        None,
    );
    render_lines(
        &frame,
        &Options {
            max_width: width,
            height: Some(height),
            ..Default::default()
        },
    )
    .iter()
    .map(|line| line.iter().map(|s| s.text.as_str()).collect::<String>())
    .collect()
}

fn keys(state: &mut ShellState, keys: &[&str]) {
    for key in keys {
        state.handle_key(key);
    }
}

#[test]
fn palette_navigation_filters_without_running_a_command() {
    let mut s = ShellState::new(vec!["project-status".into(), "crawl-site".into()]);
    keys(&mut s, &["char:p", "char:r"]);
    assert_eq!(s.selected(), Some("project-status"));
    s.handle_key("enter");
    assert_eq!(s.view, "detail");
    s.handle_key("escape");
    assert!(s.view == "palette" && !s.quit_requested);
}

#[test]
fn small_terminal_has_ascii_status_and_resize_message() {
    let _g = guard();
    let mut s = ShellState::new(vec!["project-status".into()]);
    let rows = frame_rows(
        &mut s,
        MIN_WIDTH - 1,
        MIN_HEIGHT - 1,
        None,
        None,
        None,
        None,
    )
    .join("\n");
    assert!(rows.contains("terminal too small"));
    assert!(rows.contains("[-] NO COLOR"));
}

#[test]
fn raw_key_reader_keeps_one_utf8_character_intact() {
    let (r, w) = pipe();
    put(w, "П".as_bytes());
    assert_eq!(read_key(r, None).expect("key"), "char:П");
    close(&[r, w]);
}

#[test]
fn bracketed_paste_preserves_multiline_text_until_explicit_save() {
    let (r, w) = pipe();
    put(w, b"\x1b[200~first\nsecond\x1b[201~\r");
    let mut s = state("note");
    loop {
        let key = read_key(r, None).expect("key");
        s.handle_key(&key);
        if key == "paste_end" {
            break;
        }
    }
    assert_eq!(s.note_text, "first\nsecond");
    assert!(s.view == "note" && !s.note_ready);
    let key = read_key(r, None).expect("key");
    s.handle_key(&key);
    assert!(s.note_ready);
    close(&[r, w]);
}

#[test]
fn high_descriptor_keeps_timeout_unicode_and_bracketed_paste() {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    if limit.rlim_cur <= 1100 {
        return; // host descriptor limit cannot exercise FD_SETSIZE
    }
    let (r, w) = pipe();
    let high = unsafe { libc::fcntl(r, libc::F_DUPFD, 1024) };
    assert!(high >= 1024);
    assert_eq!(read_key(high, Some(0.0)).expect("key"), "timeout");
    put(w, "\x1b[200~П\x1b[201~".as_bytes());
    let got: Vec<String> = (0..3).map(|_| read_key(high, None).expect("key")).collect();
    assert_eq!(got, ["paste_start", "char:П", "paste_end"]);
    close(&[high, r, w]);
}

#[test]
fn delete_and_modified_arrows_do_not_cancel_or_pollute_note() {
    let (r, w) = pipe();
    put(w, b"\x1b[3~\x1b[1;5D\r");
    let mut s = state("note");
    s.note_text = "retain this draft".into();
    for _ in 0..2 {
        let key = read_key(r, None).expect("key");
        s.handle_key(&key);
    }
    assert!(s.view == "note" && s.note_text == "retain this draft");
    assert_eq!(read_key(r, None).expect("key"), "enter");
    close(&[r, w]);
}

#[test]
fn overlong_note_and_save_failure_preserve_the_draft() {
    let mut s = state("note");
    s.note_text = "x".repeat(8001);
    s.handle_key("enter");
    assert!(s.view == "note" && !s.note_ready);
    assert!(s.note_error.contains("shorten"));
    s.note_text = "Do not lose this question".into();
    s.note_ready = true;
    // A directory that is not a project refuses the write.
    let dir = tempfile::tempdir().expect("tempdir");
    let message = app::save_note(dir.path().join("absent").to_str().expect("utf8"), &mut s);
    assert!(message.starts_with("Could not save"), "{message}");
    assert_eq!(s.note_text, "Do not lose this question");
    assert!(s.view == "note" && !s.note_ready);
}

#[test]
fn saved_note_is_linked_to_selected_finding() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = new_project(dir.path(), "https://example.test/");
    let mut s = state("watch");
    s.note_text = "Please recheck this finding".into();
    s.watch_section = "findings".into();
    s.watch_selected_scan_uuid = Some("scan-001".into());
    s.watch_detail_ordinal = Some(7);
    s.note_ready = true;
    app::save_note(root.to_str().expect("utf8"), &mut s);
    let listed =
        spiderhead_projects::inbox::list_entries(&root, "proof", 0, 50, true).expect("list");
    let entry = &pyjson::list_of(&listed, "entries")[0];
    assert_eq!(
        entry["references"],
        json!(["section:findings", "scan:scan-001", "finding:scan-001/7"])
    );
    assert!(s.note_text.is_empty() && !s.note_ready);
}

#[test]
fn note_from_task_context_saves_the_selected_task_reference() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = new_project(dir.path(), "https://example.test/");
    let mut s = state("watch");
    s.watch_section = "tasks".into();
    s.watch_item_id = Some("custom:late".into());
    s.note_text = "Please verify this task's retained evidence".into();
    let message = app::save_note(root.to_str().expect("utf8"), &mut s);
    assert!(message.contains("saved") && s.note_error.is_empty());
    let listed =
        spiderhead_projects::inbox::list_entries(&root, "test", 0, 50, true).expect("list");
    let refs = &pyjson::list_of(&listed, "entries")[0]["references"];
    assert!(refs
        .as_array()
        .expect("refs")
        .contains(&json!("task:custom:late")));
}

#[test]
fn sf_and_inbox_tabs_expose_bound_runs_and_human_notes() {
    let _g = guard();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = new_project(dir.path(), "https://example.test/");
    let counters = json!({"fetched": null, "queued": null, "inflight": null, "excluded": null});
    spiderhead_projects::run_observation::start(
        &root,
        "screaming_frog",
        "sf_exports",
        0,
        0,
        0,
        None,
        "offline-exports",
        None,
        false,
        None,
        None,
        None,
        counters.as_object(),
    )
    .expect("start run");
    spiderhead_projects::inbox::submit(
        &root,
        "Please inspect this competitor",
        "note",
        None,
        "specialist",
        None,
    )
    .expect("submit");
    let project = root.to_str().expect("utf8");
    let (snapshot, _) = app::watch_snapshot(project);
    let snapshot = snapshot.expect("snapshot");
    let palette = palette();
    let mut s = state("watch");
    s.handle_key("char:9");
    let sf = watch_views::watch_lines(project, &mut s, &palette, None, Some(&snapshot), None)
        .iter()
        .map(|t| t.plain.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(sf.contains("Supplied Screaming Frog exports"), "{sf}");
    s.handle_key("enter");
    let detail = watch_views::watch_detail_lines(project, &mut s, &palette, Some(&snapshot), None)
        .iter()
        .map(|t| t.plain.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        detail.contains("Collector") && detail.contains("Not recorded"),
        "{detail}"
    );
    s.handle_key("escape");
    s.handle_key("char:0");
    let inbox = watch_views::watch_lines(project, &mut s, &palette, None, Some(&snapshot), None)
        .iter()
        .map(|t| t.plain.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(inbox.contains("Please inspect this competitor"));
    assert!(inbox.to_lowercase().contains("waiting for agent"));
    s.handle_key("enter");
    assert_eq!(s.watch_detail_kind, "inbox");
}

#[test]
fn inbox_page_after_first_hundred_is_reachable() {
    let _g = guard();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = new_project(dir.path(), "https://example.test/");
    spiderhead_projects::inbox::submit(&root, "Note 0", "note", None, "specialist", None)
        .expect("submit");
    let path = root.join("inbox.json");
    let mut document: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
    let first = document["entries"][0].clone();
    document["entries"] = Value::Array(
        (0..101)
            .map(|i| {
                let mut e = first.clone();
                e["id"] = json!(format!("inbox:synthetic-{i}"));
                e["text"] = json!(format!("Note {i}"));
                e
            })
            .collect(),
    );
    std::fs::write(&path, document.to_string()).expect("write");
    let project = root.to_str().expect("utf8");
    let (snapshot, _) = app::watch_snapshot(project);
    let mut s = state("watch");
    s.watch_section = "inbox".into();
    s.watch_offset = 100;
    let lines =
        watch_views::watch_lines(project, &mut s, &palette(), None, snapshot.as_ref(), None);
    let text = lines
        .iter()
        .map(|t| t.plain.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Note 100"), "{text}");
}

#[test]
fn raw_input_preserves_newline_output_and_restores_attributes() {
    let (mut master, mut slave) = (0, 0);
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0);
    let attrs = |fd: RawFd| {
        let mut t = unsafe { std::mem::zeroed::<libc::termios>() };
        assert_eq!(unsafe { libc::tcgetattr(fd, &mut t) }, 0);
        // The kernel can flag pending input after a mode change.
        t.c_lflag &= !(libc::PENDIN as libc::tcflag_t);
        (t.c_iflag, t.c_oflag, t.c_cflag, t.c_lflag)
    };
    let original = attrs(slave);
    {
        let _raw = RawMode::enter(slave).expect("raw");
        put(slave, b"first\nsecond\n");
        let mut buf = [0u8; 64];
        let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(&buf[..n as usize], b"first\r\nsecond\r\n");
    }
    assert_eq!(attrs(slave), original);
    close(&[master, slave]);
}

#[test]
fn watch_frame_fits_viewport_with_long_evidence_rows() {
    let _g = guard();
    let dir = tempfile::tempdir().expect("tempdir");
    let target = format!("https://example.test/{}", "long/".repeat(80));
    let root = new_project(dir.path(), &target);
    let project = root.to_str().expect("utf8");
    for (width, height) in [(116, 28), (76, 22), (50, 12)] {
        let mut s = state("watch");
        let rows = frame_rows(&mut s, width, height, Some(project), None, None, None);
        assert!(
            rows.len() <= height,
            "{width}x{height}: {} rows",
            rows.len()
        );
        assert!(
            rows.iter().all(|r| r.chars().count() <= width),
            "{width}x{height}"
        );
    }
}

#[test]
fn progress_gauges_show_declared_denominator_and_unknown_evidence() {
    let _g = guard();
    let p = palette();
    assert!(panels::meter("Scenarios", Some(5), Some(10), &p)
        .plain
        .contains("5 / 10  50%"));
    assert!(panels::meter("Sitemap", None, None, &p)
        .plain
        .contains("Not measured"));
    assert!(
        panels::meter("Discovered URL coverage", Some(100), Some(1097), &p)
            .plain
            .contains("100 / 1,097  9%")
    );
}

#[test]
fn evidence_detail_can_scroll_and_return_to_selected_scan() {
    let mut s = state("watch");
    s.watch_section = "scans".into();
    keys(&mut s, &["enter", "page_down"]);
    assert_eq!(s.watch_detail_offset, 10);
    s.handle_key("up");
    assert_eq!(s.watch_detail_offset, 9);
    s.handle_key("escape");
    assert!(s.view == "watch" && s.watch_section == "scans");
}

#[test]
fn note_draft_is_visible_before_background_evidence_finishes() {
    let _g = guard();
    let mut s = state("note");
    s.note_text = "Typed draft remains visible".into();
    let rows = frame_rows(
        &mut s,
        120,
        30,
        Some("pending"),
        Some(&(None, Vec::new())),
        None,
        None,
    )
    .join("\n");
    assert!(rows.contains("Typed draft remains visible"));
    assert!(rows.contains("Enter Save"));
}

#[test]
fn imported_scan_unknown_frontier_does_not_crash_dashboard() {
    let _g = guard();
    let mut snapshot = fixture()["scan_snapshot"].clone();
    snapshot["scans"]["items"][0]["evidence"]["frontier"] =
        json!({"state": "unavailable", "counts": null, "reason": "imported input has no frontier"});
    let mut s = state("watch");
    let rows = frame_rows(
        &mut s,
        120,
        30,
        Some("unused"),
        Some(&(Some(snapshot), Vec::new())),
        Some(&json!({})),
        None,
    )
    .join("\n");
    assert!(rows.contains("Not measured"), "{rows}");
}

#[test]
fn dashboard_is_bounded_in_every_section() {
    let _g = guard();
    let fx = fixture();
    let snapshot = fx["scan_snapshot"].clone();
    for (width, height) in [(237, 68), (120, 30), (80, 22)] {
        for section in [
            "overview", "tasks", "methods", "scans", "findings", "views", "activity", "log", "sf",
            "inbox",
        ] {
            let mut s = state("watch");
            s.watch_section = section.into();
            let data = fx["pages"]["scan"]
                .get(section)
                .cloned()
                .unwrap_or(json!({}));
            let rows = frame_rows(
                &mut s,
                width,
                height,
                Some("unused"),
                Some(&(Some(snapshot.clone()), Vec::new())),
                Some(&data),
                None,
            );
            assert!(
                rows.len() <= height,
                "{section} {width}x{height}: {} rows",
                rows.len()
            );
            assert!(
                rows.iter().all(|r| r.chars().count() <= width),
                "{section} {width}x{height}"
            );
        }
    }
}

#[test]
fn watch_state_can_filter_sort_page_and_open_a_retained_finding() {
    let mut s = state("watch");
    s.handle_key("char:5");
    assert_eq!(s.watch_section, "findings");
    s.handle_key("char:f");
    assert_eq!(s.view, "watch_filter");
    keys(&mut s, &["char:д", "enter"]);
    assert!(s.view == "watch" && s.watch_query == "д");
    s.handle_key("char:s");
    assert_eq!(s.watch_sort, "check");
    s.handle_key("char:r");
    assert!(s.watch_descending);
    s.handle_key("page_down");
    assert_eq!(s.watch_offset, 50);
    s.watch_detail_ordinal = Some(7);
    s.handle_key("enter");
    assert_eq!(s.view, "watch_detail");
    s.handle_key("escape");
    assert_eq!(s.view, "watch");
}

#[test]
fn watch_frame_browses_a_retained_finding_and_its_evidence() {
    let _g = guard();
    let fx = fixture();
    let snapshot = (Some(fx["scan_snapshot"].clone()), Vec::new());
    let data = fx["pages"]["scan"]["findings"].clone();
    let mut s = state("watch");
    s.watch_section = "findings".into();
    let rows = frame_rows(
        &mut s,
        100,
        30,
        Some("unused"),
        Some(&snapshot),
        Some(&data),
        None,
    )
    .join("\n");
    assert!(rows.contains("findings"));
    assert!(s.watch_detail_ordinal.is_some());
    s.view = "watch_detail".into();
    let rows = frame_rows(
        &mut s,
        100,
        30,
        Some("unused"),
        Some(&snapshot),
        Some(&data),
        None,
    )
    .join("\n");
    assert!(rows.to_lowercase().contains("retained evidence"), "{rows}");
}

#[test]
fn editor_suspend_resume_cursor_and_explicit_discard_preserve_other_draft() {
    let mut s = state("watch");
    keys(
        &mut s,
        &[
            "char:n", "char:H", "char:e", "char:l", "char:l", "char:o", "home", "delete",
        ],
    );
    assert_eq!(s.note_text, "ello");
    keys(&mut s, &["escape", "char:g", "char:G", "escape", "char:n"]);
    assert_eq!(s.note_text, "ello");
    s.handle_key("ctrl_x");
    assert_eq!(s.note_text, "");
    keys(&mut s, &["escape", "char:g"]);
    assert_eq!(s.note_text, "G");
}

#[test]
fn large_paste_keeps_every_character_and_refuses_oversized_submit() {
    let mut s = state("note");
    let text = "Проверка\n".repeat(1200);
    s.handle_key("paste_start");
    for c in text.chars() {
        if c == '\n' {
            s.handle_key("enter");
        } else {
            s.handle_key(&format!("char:{c}"));
        }
    }
    keys(&mut s, &["paste_end", "enter"]);
    assert_eq!(s.note_text, text);
    assert!(!s.note_ready && s.view == "note");
}

#[test]
fn filter_cancel_and_last_page_navigation_are_not_mutations() {
    let mut s = state("watch");
    s.watch_section = "tasks".into();
    s.watch_query = "old".into();
    keys(&mut s, &["char:f", "char:X", "escape"]);
    assert_eq!(s.watch_query, "old");
    s.watch_count = Some(2);
    s.watch_next_offset = None;
    keys(&mut s, &["page_down", "end", "down"]);
    assert!(s.watch_offset == 0 && s.watch_index == 1);
}

#[test]
fn refresh_preserves_selected_identity_without_overriding_arrow_navigation() {
    let mut s = state("watch");
    s.watch_section = "tasks".into();
    let items = vec![json!({"id": "a"}), json!({"id": "b"})];
    watch_views::page_selection(&mut s, &items, &json!({"total": 2}));
    s.handle_key("down");
    watch_views::page_selection(&mut s, &items, &json!({"total": 2}));
    assert_eq!(s.watch_selected_row_id.as_deref(), Some("b"));
    let mut grown = vec![json!({"id": "new"})];
    grown.extend(items);
    watch_views::page_selection(&mut s, &grown, &json!({"total": 3}));
    assert!(s.watch_index == 2 && s.watch_selected_row_id.as_deref() == Some("b"));
}

#[test]
fn cached_render_shows_receipt_without_a_project_on_disk() {
    let _g = guard();
    let fx = fixture();
    let snapshot = (Some(fx["scan_snapshot"].clone()), Vec::new());
    for (width, height) in [(80, 24), (120, 32), (237, 68)] {
        let mut s = state("watch");
        let rows = frame_rows(
            &mut s,
            width,
            height,
            Some("/nonexistent/project"),
            Some(&snapshot),
            Some(&json!({})),
            Some("Note saved locally: receipt 123"),
        )
        .join("\n");
        assert!(
            rows.contains("Note saved locally: receipt 123"),
            "{width}x{height}"
        );
        assert!(
            rows.contains("Inspect the retained product sample"),
            "{width}x{height}"
        );
    }
}

#[test]
fn note_can_start_from_evidence_detail_and_return_without_losing_context() {
    let mut s = state("watch_detail");
    s.watch_section = "findings".into();
    s.watch_detail_ordinal = Some(7);
    s.watch_selected_scan_uuid = Some("synthetic".into());
    keys(&mut s, &["char:n", "char:X", "escape"]);
    assert!(s.view == "watch_detail" && s.note_text == "X");
    keys(&mut s, &["char:n", "enter"]);
    assert!(s.view == "watch_detail" && s.note_ready);
    assert!(
        s.watch_detail_ordinal == Some(7)
            && s.watch_selected_scan_uuid.as_deref() == Some("synthetic")
    );
}

#[test]
fn wide_home_keeps_active_work_with_site_scoped_identifiers() {
    let _g = guard();
    let mut snapshot = fixture()["scan_snapshot"].clone();
    snapshot["active_tasks"]["items"][0]["id"] =
        json!("site:00000000-0000-4000-8000-000000000001/custom:late");
    let mut s = state("watch");
    let rows = frame_rows(
        &mut s,
        140,
        40,
        Some("unused"),
        Some(&(Some(snapshot), Vec::new())),
        Some(&json!({})),
        None,
    )
    .join("\n");
    assert!(rows.contains("CURRENT WORK"), "{rows}");
    assert!(rows.contains("Inspect the retained product sample"));
    assert!(!rows.contains("NEXT ACTIONS"));
}

#[test]
fn compact_home_keeps_collection_and_stale_state_visible() {
    let _g = guard();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = new_project(dir.path(), "https://example.test/");
    let run = spiderhead_projects::run_observation::start(
        &root,
        "native",
        "spider",
        100,
        0,
        0,
        None,
        "synthetic",
        None,
        false,
        None,
        None,
        None,
        None,
    )
    .expect("start");
    let id = run["id"].as_str().expect("id").to_string();
    spiderhead_projects::run_observation::progress(
        &root,
        &id,
        Some(20),
        Some(79),
        Some(1),
        Some(2),
        Some(4.0),
        Some(1.0),
        "separate",
        "native",
    )
    .expect("progress");
    let project = root.to_str().expect("utf8");
    let (snapshot, _) = app::watch_snapshot(project);
    let mut snapshot = snapshot.expect("snapshot");
    let render = |snapshot: &Value| {
        let mut s = state("watch");
        frame_rows(
            &mut s,
            76,
            22,
            Some(project),
            Some(&(Some(snapshot.clone()), Vec::new())),
            Some(&json!({})),
            None,
        )
        .join("\n")
    };
    let text = render(&snapshot);
    assert!(
        text.contains("20 / 100 discovered") && text.contains("4.00 pages/s"),
        "{text}"
    );
    assert!(
        text.contains("Queue 79") && text.contains("Sitemap:") && text.contains("Agreed tasks:"),
        "{text}"
    );
    snapshot["runs"]["items"][0]["telemetry"]["state"] = json!("stale");
    snapshot["runs"]["items"][0]["telemetry"]["current_rate_per_second"] = Value::Null;
    let stale = render(&snapshot);
    assert!(
        stale.contains("Stale")
            && !stale.contains("observed active")
            && !stale.contains("4.00 pages/s"),
        "{stale}"
    );
}

#[test]
fn explicit_language_wins_and_unknown_is_refused() {
    assert_eq!(
        localization::resolve_language(Some("en")).expect("en"),
        "en"
    );
    assert!(localization::resolve_language(Some("fr")).is_err());
}

#[test]
fn scan_selector_preserves_source_and_fails_explicitly() {
    let snapshot =
        json!({"scans": {"items": [{"uuid": "one"}, {"uuid": "two"}]}, "runs": {"items": []}});
    let before = snapshot.clone();
    let picked = scan_now::select(&snapshot, Some("one")).expect("one");
    assert_eq!(picked["scans"]["items"], json!([{"uuid": "one"}]));
    assert_eq!(snapshot, before);
    let err = scan_now::select(&snapshot, Some("missing")).expect_err("absent");
    assert!(err.to_string().contains("absent"));
}

#[test]
fn compact_uses_real_counts_and_does_not_invent_unknowns() {
    let _g = guard();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = new_project(dir.path(), "https://example.test/");
    let run = spiderhead_projects::run_observation::start(
        &root,
        "native",
        "spider",
        100,
        0,
        0,
        None,
        "synthetic",
        None,
        false,
        None,
        None,
        None,
        None,
    )
    .expect("start");
    let id = run["id"].as_str().expect("id").to_string();
    spiderhead_projects::run_observation::progress(
        &root,
        &id,
        Some(4),
        Some(5),
        Some(1),
        Some(0),
        Some(4.0),
        Some(1.0),
        "separate",
        "native",
    )
    .expect("progress");
    std::env::set_var("COLUMNS", "300");
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = app::run(
        true,
        root.to_str(),
        &[],
        Some("ru"),
        Some(&id),
        true,
        false,
        false,
        &mut out,
        &mut err,
        None,
    );
    let rendered = String::from_utf8(out).expect("utf8");
    assert_eq!(code, 0, "{rendered} {}", String::from_utf8_lossy(&err));
    assert!(rendered.contains("Скан сейчас"), "{rendered}");
    assert!(
        rendered.contains("4 / 10") && rendered.contains("40%"),
        "{rendered}"
    );
    localization::set_language("en");
}

#[test]
fn russian_numbers_run_label_and_finished_scan() {
    let _g = guard();
    localization::set_language("ru");
    assert_eq!(localization::number(1330), "1 330");
    assert_eq!(
        scan_now::run_label(&json!("ad994e4a90084c7eae15a8ab564b55f9")),
        "r-ad99"
    );
    let snapshot = json!({"runs": {"items": [{"id": "0143aa", "state": "running",
        "collector": {"mode": "list"},
        "counters": {"fetched": 1330, "queued": 3670, "inflight": 0},
        "telemetry": {"queue_semantics": "separate", "state": "fresh"}}]},
        "scans": {"items": []}});
    let line = scan_now::summary(Some(&snapshot));
    assert!(
        line.starts_with("r-0143 · Список URL · В работе · 1 330 / 5 000 · 27%"),
        "{line}"
    );
    assert!(!line.contains("0143aa") && !line.contains("Queue"));
    localization::set_language("en");
}
