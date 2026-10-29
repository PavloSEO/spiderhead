//! The TUI shell: observer loop, watch dashboard, and the run engine.
//!
//! `_watch_snapshot`/`_read_view` own the
//! bounded read path (tests pass overrides instead of monkeypatching);
//! `_ObserverRefresh` is the background reader; `run` drives the loop.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::labels::{label as human_label, value_lines};
use crate::localization::{number, ui};
use crate::panels;
use crate::pyjson::{self, OrPy};
use crate::render::{Renderable, Segment};
use crate::rlayout::{Column, Direction, Grid, Layout};
use crate::scan_now;
use crate::state::{ShellState, WATCH_SECTIONS};
use crate::text::Text;
use crate::theme::Palette;
use crate::watch_views;

/// `_watch_snapshot` — bounded snapshot or a failure frame.
pub fn watch_snapshot(project: &str) -> (Option<Value>, Vec<Text>) {
    match spiderhead_projects::observer::observe(Path::new(project), None, 20, 0, 20, None) {
        Ok(snapshot) => (Some(snapshot), Vec::new()),
        Err(exc) => (
            None,
            vec![
                Text::new(&format!("project unavailable: {exc}"), ""),
                Text::new("No action was started.", ""),
            ],
        ),
    }
}

/// `_read_view` — read one bounded page in the background, never during an
/// interactive render.
///
/// Guard conditions return `{"error": ...}` objects; project API
/// failures surface as `Err`, matching the exception the worker catches and
/// turns into `"Retained data unavailable: {exc}"`.
pub fn read_view(project: &str, state: &mut ShellState, snapshot: &Value) -> Result<Value, String> {
    let mut scan_project = PathBuf::from(project);
    if state.watch_site_uuid.is_some()
        && ["scans", "findings", "views"].contains(&state.watch_section.as_str())
    {
        let site = pyjson::path(snapshot, &["sites", "items"])
            .and_then(Value::as_array)
            .and_then(|items| {
                items.iter().find(|item| {
                    pyjson::str_of(item, "project_uuid")
                        == state.watch_site_uuid.as_deref().unwrap_or("")
                })
            })
            .cloned();
        let directory = site
            .as_ref()
            .and_then(|s| pyjson::get(s, "directory"))
            .and_then(Value::as_str);
        match directory {
            Some(dir) if !dir.is_empty() => {
                let root = scan_project
                    .canonicalize()
                    .unwrap_or_else(|_| scan_project.clone());
                let target = root
                    .join(dir)
                    .canonicalize()
                    .unwrap_or_else(|_| root.join(dir));
                if !target.starts_with(&root) {
                    return Ok(
                        json!({"error": "Selected site workspace is outside this project."}),
                    );
                }
                scan_project = target;
            }
            _ => {
                return Ok(
                    json!({"error": "Selected site workspace is unavailable; choose another site with p."}),
                );
            }
        }
    }
    if state.view == "watch_detail" {
        match state.watch_detail_kind.as_str() {
            "task" => {
                let Some(item_id) = state.watch_item_id.clone() else {
                    return Ok(json!({"error": "No task selected."}));
                };
                return spiderhead_projects::observer::task_detail(Path::new(project), &item_id)
                    .map_err(|e| e.to_string());
            }
            "finding" => {
                let Some(ordinal) = state.watch_detail_ordinal else {
                    return Ok(json!({"error": "No finding selected."}));
                };
                return spiderhead_projects::observer::finding_detail(
                    &scan_project,
                    state.watch_selected_scan_uuid.as_deref(),
                    ordinal,
                )
                .map_err(|e| e.to_string());
            }
            "view" => {
                let Some(name) = state.watch_view_name.clone() else {
                    return Ok(json!({"error": "No saved view selected."}));
                };
                return spiderhead_projects::observer::saved_view_page(
                    &scan_project,
                    &name,
                    state.watch_selected_scan_uuid.as_deref(),
                    state.watch_view_offset,
                )
                .map_err(|e| e.to_string());
            }
            _ => {}
        }
    }
    match state.watch_section.as_str() {
        "tasks" | "methods" | "schema" => spiderhead_projects::observer::checklist_page(
            Path::new(project),
            state.watch_offset,
            50,
            &state.watch_query,
            match state.watch_section.as_str() {
                "methods" => Some("method"),
                "schema" => Some("schema"),
                _ => None,
            },
            state.watch_status.as_deref(),
            "id",
            false,
            None,
        )
        .map_err(|e| e.to_string()),
        "findings" => spiderhead_projects::observer::findings_page(
            &scan_project,
            state.watch_selected_scan_uuid.as_deref(),
            state.watch_offset,
            50,
            &state.watch_query,
            None,
            &state.watch_sort,
            state.watch_descending,
        )
        .map_err(|e| e.to_string()),
        "scans" => {
            spiderhead_projects::observer::scans_page(&scan_project, state.watch_offset, 50, false)
                .map_err(|e| e.to_string())
        }
        "views" => {
            spiderhead_projects::finding_views::list_views(&scan_project).map_err(|e| e.to_string())
        }
        "inbox" => spiderhead_projects::inbox::list_entries(
            Path::new(project),
            "observer/local",
            state.watch_offset,
            50,
            true,
        )
        .map_err(|e| e.to_string()),
        _ => Ok(json!({})),
    }
}

/// `_save_note` — keep a draft visible when a local save is refused or fails.
pub fn save_note(project: &str, state: &mut ShellState) -> String {
    let mut references: Vec<Value> = vec![json!(format!("section:{}", state.watch_section))];
    if state.watch_site_uuid.is_some()
        && ["scans", "findings", "views"].contains(&state.watch_section.as_str())
    {
        references.push(json!(format!(
            "section:site/{}",
            state.watch_site_uuid.as_deref().unwrap_or("")
        )));
    } else if state.watch_section == "sites" && state.watch_site_openable {
        references.push(json!(format!(
            "section:site/{}",
            state.watch_site_detail_uuid.as_deref().unwrap_or("")
        )));
    }
    if state.watch_item_id.is_some()
        && ["tasks", "methods", "schema"].contains(&state.watch_section.as_str())
    {
        references.push(json!(format!(
            "task:{}",
            state.watch_item_id.as_deref().unwrap_or("")
        )));
    }
    if state.watch_selected_scan_uuid.is_some()
        && ["scans", "findings", "views"].contains(&state.watch_section.as_str())
    {
        let uuid = state.watch_selected_scan_uuid.clone().unwrap_or_default();
        references.push(json!(format!("scan:{uuid}")));
        if let Some(ordinal) = state.watch_detail_ordinal {
            references.push(json!(format!("finding:{uuid}/{ordinal}")));
        }
    }
    let receipt = match spiderhead_projects::inbox::submit(
        Path::new(project),
        &state.note_text,
        &state.note_kind,
        Some(&Value::Array(references)),
        "specialist",
        None,
    ) {
        Ok(r) => r,
        Err(exc) => {
            state.note_error = format!("Could not save: {exc}");
            state.view = "note".into();
            state.note_ready = false;
            return state.note_error.clone();
        }
    };
    state.note_text.clear();
    state.note_drafts.remove(&state.note_kind);
    state.note_cursor = Some(0);
    state.note_error.clear();
    state.note_ready = false;
    let entry_id = pyjson::path(&receipt, &["entry", "id"])
        .map(pyjson::py_str)
        .unwrap_or_default();
    format!(
        "{} saved · {} · waiting for agent triage",
        human_label(&json!(state.note_kind)),
        entry_id
    )
}

/// A read result: snapshot pair plus the view page for a `_view_key`.
struct ReadResult {
    key: Value,
    snapshot: Option<Value>,
    failure: Vec<Text>,
    page: Value,
}

enum RefreshEvent {
    Saved(Box<ShellState>, String, String),
    Read(Box<ReadResult>),
}

/// `_ObserverRefresh` — one worker owns snapshot/page/detail reads and
/// explicit note writes.
pub struct ObserverRefresh {
    project: String,
    scan: Option<String>,
    /// Last published `(snapshot, failure)` pair.
    pub result: (Option<Value>, Vec<Text>),
    /// Page for the key that produced `result`.
    pub page: Value,
    /// `_view_key` of `page`.
    pub key: Option<Value>,
    /// Completed save `(state, message, submitted_text)` awaiting pickup.
    pub saved: Option<(ShellState, String, String)>,
    /// A note write is in flight.
    pub saving: bool,
    /// When the last read landed (`time.monotonic`).
    pub updated_at: Option<Instant>,
    next_refresh: Instant,
    pending_note: Arc<Mutex<Option<ShellState>>>,
    events: (Sender<RefreshEvent>, Receiver<RefreshEvent>),
    worker_alive: Arc<Mutex<bool>>,
}

impl ObserverRefresh {
    /// `_ObserverRefresh(project, scan=scan)`.
    pub fn new(project: &str, scan: Option<String>) -> Self {
        ObserverRefresh {
            project: project.into(),
            scan,
            result: (
                None,
                vec![Text::new(&ui("Loading retained project evidence…"), "")],
            ),
            page: json!({"loading": true}),
            key: None,
            saved: None,
            saving: false,
            updated_at: None,
            next_refresh: Instant::now(),
            pending_note: Arc::new(Mutex::new(None)),
            events: channel(),
            worker_alive: Arc::new(Mutex::new(false)),
        }
    }

    /// `refresh.save(state)` — queue a note write for the worker.
    pub fn save(&mut self, state: &ShellState) {
        if !self.saving {
            *self.pending_note.lock().unwrap_or_else(|e| e.into_inner()) = Some(state.clone());
            self.saving = true;
        }
    }

    /// `refresh.page_for(state)` — the cached page or `{"loading": true}`.
    pub fn page_for(&self, state: &ShellState) -> Value {
        if self.key.as_ref() == Some(&watch_views::view_key(state)) {
            self.page.clone()
        } else {
            json!({"loading": true})
        }
    }

    /// `refresh.poll(state)` — drain completed reads, spawn the worker when
    /// the key changed or the refresh window elapsed. Returns the snapshot pair.
    pub fn poll(&mut self, mut state: Option<&mut ShellState>) -> (Option<Value>, Vec<Text>) {
        while let Ok(event) = self.events.1.try_recv() {
            match event {
                RefreshEvent::Saved(saved, message, text) => {
                    self.saved = Some((*saved, message, text));
                    self.saving = false;
                }
                RefreshEvent::Read(read) => {
                    let mut key = read.key;
                    if let Some(state) = state.as_deref_mut() {
                        if key == watch_views::view_key(state)
                            && state.watch_selected_scan_uuid.is_none()
                        {
                            let resolved = pyjson::path(&read.page, &["scan", "uuid"])
                                .or_py(|| pyjson::get(&read.page, "scan_uuid"))
                                .map(pyjson::py_str);
                            if let Some(uuid) = resolved {
                                state.watch_selected_scan_uuid = Some(uuid);
                                key = watch_views::view_key(state);
                            }
                        }
                    }
                    self.key = Some(key);
                    self.result = (read.snapshot, read.failure);
                    self.page = read.page;
                    let now = Instant::now();
                    self.updated_at = Some(now);
                    self.next_refresh = now + Duration::from_millis(500);
                }
            }
        }
        let wanted = state.as_deref().map(watch_views::view_key);
        let alive = *self.worker_alive.lock().unwrap_or_else(|e| e.into_inner());
        let pending = self
            .pending_note
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        if !alive && (pending || wanted != self.key || Instant::now() >= self.next_refresh) {
            let note = self
                .pending_note
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            *self.worker_alive.lock().unwrap_or_else(|e| e.into_inner()) = true;
            let sender = self.events.0.clone();
            let project = self.project.clone();
            let scan = self.scan.clone();
            let alive_flag = Arc::clone(&self.worker_alive);
            let state_copy = state.as_deref().cloned();
            let prev_result = self.result.clone();
            std::thread::spawn(move || {
                read_worker(project, scan, state_copy, note, prev_result, sender);
                *alive_flag.lock().unwrap_or_else(|e| e.into_inner()) = false;
            });
        }
        self.result.clone()
    }
}

/// `_ObserverRefresh._read` — the worker body. On a retained-data failure the
/// published result reverts to the previous snapshot.
fn read_worker(
    project: String,
    scan: Option<String>,
    state: Option<ShellState>,
    note: Option<ShellState>,
    prev_result: (Option<Value>, Vec<Text>),
    sender: Sender<RefreshEvent>,
) {
    if let Some(mut note) = note {
        let submitted = note.note_text.clone();
        let message = save_note(&project, &mut note);
        let _ = sender.send(RefreshEvent::Saved(Box::new(note), message, submitted));
    }
    let key = state.as_ref().map(watch_views::view_key);
    let mut state = state;
    let unavailable =
        |exc: &dyn std::fmt::Display| json!({"error": format!("Retained data unavailable: {exc}")});
    // try: snapshot + scan selection + page read; except → previous result.
    let (result, page) = {
        let (snapshot, failure) = watch_snapshot(&project);
        match snapshot {
            None => ((None, failure), json!({})),
            Some(snap) => match scan_now::select(&snap, scan.as_deref()) {
                Err(exc) => (prev_result, unavailable(&exc)),
                Ok(selected) => match state.as_mut() {
                    Some(state) => match read_view(&project, state, &selected) {
                        Ok(page) => ((Some(selected), failure), page),
                        Err(exc) => (prev_result, unavailable(&exc)),
                    },
                    None => ((Some(selected), failure), json!({})),
                },
            },
        }
    };
    let _ = sender.send(RefreshEvent::Read(Box::new(ReadResult {
        key: key.unwrap_or(Value::Null),
        snapshot: result.0,
        failure: result.1,
        page,
    })));
}

/// `_watch_dashboard` — responsive terminal workspace from retained evidence.
#[allow(clippy::too_many_arguments)]
pub fn watch_dashboard(
    state: &mut ShellState,
    project: &str,
    palette: &Palette,
    width: usize,
    height: usize,
    message: Option<&str>,
    snapshot_override: Option<&(Option<Value>, Vec<Text>)>,
    view_override: Option<&Value>,
) -> Renderable {
    let owned_snapshot;
    let (snapshot, failure): (&Option<Value>, Vec<Text>) = match snapshot_override {
        Some((snap, failure)) => (snap, failure.clone()),
        None => {
            owned_snapshot = watch_snapshot(project);
            (&owned_snapshot.0, owned_snapshot.1.clone())
        }
    };
    let accent = if palette.color { &palette.accent } else { "" };
    let muted = if palette.color { &palette.muted } else { "" };
    let border = if palette.color { "#475569" } else { "none" };
    let mut root = Layout::dashboard(height);
    root.split_column(vec![
        {
            let mut l = Layout::named("header");
            l.size = Some(3);
            l
        },
        Layout::named("body"),
        {
            let mut l = Layout::named("footer");
            l.size = Some(4);
            l
        },
    ]);
    let mut site = snapshot
        .as_ref()
        .and_then(|s| pyjson::path(s, &["project", "site"]).cloned())
        .unwrap_or(json!({}));
    let mut selected_site: Option<Value> = None;
    if snapshot.is_some()
        && state.watch_site_uuid.is_some()
        && ["scans", "findings", "views"].contains(&state.watch_section.as_str())
    {
        selected_site = snapshot
            .as_ref()
            .and_then(|s| pyjson::path(s, &["sites", "items"]).and_then(Value::as_array))
            .and_then(|items| {
                items.iter().find(|item| {
                    pyjson::str_of(item, "project_uuid")
                        == state.watch_site_uuid.as_deref().unwrap_or("")
                })
            })
            .cloned();
        if let Some(found) = &selected_site {
            site = pyjson::get(found, "site").cloned().unwrap_or(site);
        }
    }
    let mut header = Grid::new(true, (0, 0, 0, 0));
    header.add_column(Column {
        ratio: Some(1),
        no_wrap: true,
        overflow: Some(crate::text::Overflow::Ellipsis),
        ..Default::default()
    });
    header.add_column(Column {
        justify: Some(crate::render::Justify::Right),
        max_width: Some((width / 2).max(20)),
        no_wrap: true,
        overflow: Some(crate::text::Overflow::Ellipsis),
        ..Default::default()
    });
    header.add_row(vec![
        Text::new(&ui("SEOHEAD  /  PROJECT OBSERVER"), accent).into(),
        Text::new(
            &if state.view == "note" {
                ui("NOTE DRAFT  ·  LOCAL")
            } else {
                ui("READ ONLY  ·  LOCAL")
            },
            muted,
        )
        .into(),
    ]);
    header.add_row(vec![
        Text::new(
            &pyjson::get(&site, "label")
                .or_py(|| pyjson::get(&site, "host"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| ui("Project")),
            "bold",
        )
        .into(),
        Text::new(&format!("{} x {}", width + 4, height + 2), muted).into(),
    ]);
    if let Some(snap) = snapshot {
        let run_source = selected_site.as_ref().unwrap_or(snap);
        let recent_run = pyjson::path(run_source, &["runs", "items"])
            .and_then(Value::as_array)
            .and_then(|items| items.first().cloned());
        let mut run_hint = ui("No project run recorded");
        if let Some(run) = recent_run {
            let events = pyjson::list_of(&run, "events");
            let last_phase = events
                .last()
                .and_then(|e| pyjson::get(e, "phase"))
                .map(pyjson::py_str)
                .unwrap_or_else(|| "unknown phase".into());
            let mut parts = vec![
                human_label(pyjson::get(&run, "kind").unwrap_or(&Value::Null)),
                human_label(pyjson::get(&run, "state").unwrap_or(&Value::Null)),
            ];
            if pyjson::str_of(&run, "state") == "running" {
                parts.push(human_label(&json!(last_phase)));
            }
            run_hint = parts.join(" · ");
            let sample_state = pyjson::path(&run, &["telemetry", "state"])
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(sample) = &sample_state {
                let label = if sample == "stale" {
                    ui("Stale sample")
                } else {
                    human_label(&json!(sample))
                };
                run_hint = format!("{label} · {run_hint}");
            }
            let runtime = if pyjson::str_of(&run, "kind") == "screaming_frog" {
                pyjson::get(&run, "collector_runtime")
            } else {
                pyjson::get(&run, "controller")
            };
            if pyjson::str_of(&run, "state") == "running"
                && pyjson::str_of(runtime.unwrap_or(&Value::Null), "state") == "live"
                && sample_state.as_deref() == Some("fresh")
            {
                let marker = if state.motion_enabled && palette.color {
                    ["◐", "◓", "◑", "◒"][secs_monotonic() % 4]
                } else {
                    "+"
                };
                run_hint = format!("{marker} {} · {run_hint}", ui("observed active"));
            }
        }
        header.add_row(vec![
            Text::new(
                &format!(
                    "{} {}  ·  {} {}",
                    ui("Notes"),
                    pyjson::path(snap, &["inbox", "pagination", "total"])
                        .map(pyjson::py_str)
                        .unwrap_or_default(),
                    ui("Competitors"),
                    pyjson::list_of(pyjson::obj(snap, "preparation"), "competitors").len()
                ),
                muted,
            )
            .into(),
            Text::new(&run_hint, accent).into(),
        ]);
    }
    if snapshot.is_some() && state.watch_section == "overview" {
        header.add_row(vec![Text::new(
            &format!(
                "{}: {}",
                ui("Scan now"),
                scan_now::summary(snapshot.as_ref())
            ),
            accent,
        )
        .into()]);
        if let Some(h) = root.get_mut("header") {
            h.size = Some(4);
        }
    }
    if let Some(h) = root.get_mut("header") {
        h.update(Renderable::Grid(header));
    }
    let sidebar = width >= 130 && height >= 26;
    if sidebar {
        if let Some(body) = root.get_mut("body") {
            body.direction = Direction::Row;
            body.children = vec![
                {
                    let mut nav = Layout::named("nav");
                    nav.size = Some(23);
                    nav
                },
                Layout::named("content"),
            ];
        }
        let mut nav = vec![Text::new(&ui("WORKSPACE"), muted), Text::blank()];
        for (index, section) in WATCH_SECTIONS.iter().enumerate() {
            let selected = *section == state.watch_section;
            let shortcut = if index < 10 {
                ((index + 1) % 10).to_string()
            } else {
                match *section {
                    "schema" => "a".into(),
                    "sites" => "p".into(),
                    _ => String::new(),
                }
            };
            let label = match *section {
                "sf" => ui("SF scans"),
                "schema" => ui("Schema"),
                other => human_label(&json!(other)),
            };
            nav.push(Text::new(
                &format!(
                    " {} {}  {}",
                    if selected { ">" } else { " " },
                    shortcut,
                    label
                ),
                if selected && palette.color {
                    &palette.highlight
                } else {
                    ""
                },
            ));
        }
        nav.extend([
            Text::blank(),
            Text::new(&ui("n  Add note"), accent),
            Text::new(&ui("g  Propose goal"), accent),
            Text::blank(),
            Text::new(&ui("Local evidence"), muted),
        ]);
        if let Some(nav_layout) = root.get_mut("nav") {
            nav_layout.update(Renderable::Panel(crate::render::Panel {
                renderable: Box::new(Renderable::Group(
                    nav.into_iter().map(Renderable::Text).collect(),
                )),
                title: None,
                title_align: "center".into(),
                border_style: border.into(),
                padding: (1, 1, 1, 1),
                height: None,
                expand: true,
            }));
        }
    }
    let body: Vec<Text> = match state.view.as_str() {
        "note" => panels::note_lines(state, palette),
        "watch_filter" => vec![
            Text::new("Filter findings", accent),
            Text::blank(),
            Text::new(
                if state.filter_draft.is_empty() {
                    "Type text, then press Enter"
                } else {
                    &state.filter_draft
                },
                "",
            ),
        ],
        "watch_help" => [
            "1-9 / 0  Switch section · a Structured data · p Sites",
            "Sites: Enter coverage/artifacts · s Browse site's scans from details",
            "↑/↓  Select · Home/End  First/last row on page",
            "PgUp/PgDn  Previous/next data page",
            "Enter  Open details · Esc  Return from details",
            "f  Filter tasks/methods/findings · c  Clear filter",
            "t  Cycle task/method state filter",
            "Findings: s Sort · r Reverse",
            "n  Note / resume draft · g  Proposed goal / resume draft",
            "Editor: ←/→ Home/End move · Delete/Backspace edit",
            "Editor: Enter save · Esc keep draft · Ctrl-X discard",
            "m  Toggle motion · q / Ctrl-C  Quit · ?  Help",
        ]
        .iter()
        .map(|line| Text::new(line, ""))
        .collect(),
        _ if snapshot.is_none() => failure.clone(),
        "watch_detail" => watch_views::watch_detail_lines(
            project,
            state,
            palette,
            snapshot.as_ref(),
            view_override,
        ),
        _ => watch_views::watch_lines(
            project,
            state,
            palette,
            message,
            snapshot.as_ref(),
            view_override,
        ),
    };
    let mut body = body;
    let available = (height as i64 - 9).max(1) as usize;
    if state.view == "watch_detail" {
        let content_width = (width as i64 - if sidebar { 27 } else { 4 }).max(10) as usize;
        body = body
            .iter()
            .flat_map(|line| {
                line.wrap(
                    content_width,
                    Some(crate::text::Overflow::Fold),
                    Some(false),
                )
            })
            .collect();
        state.watch_detail_offset = state
            .watch_detail_offset
            .min((body.len() as i64 - available as i64).max(0));
        body = body.split_off((state.watch_detail_offset as usize).min(body.len()));
    } else if state.view == "note" {
        let content_width = (width as i64 - if sidebar { 27 } else { 4 }).max(10) as usize;
        let draft_lines: Vec<Text> = body
            .last()
            .map(|line| line.wrap(content_width, None, None))
            .unwrap_or_default();
        let count = (available as i64 - 3).max(1) as usize;
        let cursor = state
            .note_cursor
            .unwrap_or_else(|| state.note_text.chars().count());
        let before_cursor = Text::new(
            &state.note_text.chars().take(cursor).collect::<String>(),
            "",
        )
        .wrap(content_width, None, None);
        let first = (before_cursor.len() as i64 - count as i64).max(0) as usize;
        let mut head = body.clone();
        head.pop();
        head.extend(draft_lines.into_iter().skip(first).take(count));
        body = head;
    }
    if state.view == "watch" && state.watch_section != "overview" {
        let first = (state.watch_index - ((available as i64 - 6).max(1)) + 1).max(0) as usize;
        let mut head: Vec<Text> = body.iter().take(4).cloned().collect();
        head.extend(body.iter().skip(4 + first).cloned());
        body = head;
    }
    for line in &mut body {
        line.no_wrap = state.view != "note";
        line.overflow = Some(if state.view == "note" {
            crate::text::Overflow::Fold
        } else {
            crate::text::Overflow::Ellipsis
        });
    }
    let title = if state.view == "note" {
        ui("Compose note")
    } else {
        human_label(&json!(state.watch_section))
    };
    if snapshot.is_some()
        && state.view == "watch"
        && state.watch_section == "overview"
        && height >= 32
        && width >= 100
        && !pyjson::get(view_override.unwrap_or(&json!({})), "error").is_some()
    {
        overview_panels(state, snapshot, palette, border, width, height, &mut root);
    } else {
        let content = if sidebar {
            root.get_mut("content")
        } else {
            root.get_mut("body")
        };
        if let Some(content) = content {
            content.update(Renderable::Panel(crate::render::Panel {
                renderable: Box::new(Renderable::Group(
                    body.iter()
                        .take(available)
                        .cloned()
                        .map(Renderable::Text)
                        .collect(),
                )),
                title: Some(title),
                title_align: "left".into(),
                border_style: border.into(),
                padding: (0, 1, 0, 1),
                height: None,
                expand: true,
            }));
        }
    }
    // Footer.
    let mut footer = Text::blank();
    match state.view.as_str() {
        "note" => {
            footer.append("Enter Save   Esc Keep draft   Ctrl-X Discard   ←/→ Edit\n", accent);
            footer.append(
                "Saved locally; unread notice requires a project-bound agent",
                muted,
            );
        }
        "watch_filter" => {
            footer.append("Type filter   Enter Apply   Esc Cancel", accent);
        }
        "watch_detail" => {
            footer.append(
                "↑ ↓ Scroll evidence   PgUp/PgDn Scroll page   Enter/Esc Back",
                accent,
            );
            footer.append("\nn Note / g Goal on this evidence", accent);
            if state.watch_detail_kind == "view" {
                footer.append("\n[ Previous / ] Next result page", accent);
            } else if state.watch_detail_kind == "site" && state.watch_site_openable {
                footer.append("\ns Browse this site's scan history", accent);
            }
        }
        "watch_help" => footer.append("Enter/Esc Back\n", accent),
        _ if !sidebar => footer.append(
            "1 Home  2 Tasks  3 Methods  4 Scans  5 Findings\n6 Views  7 Activity  8 Log  9 SF  0 Inbox  a Schema  p Sites\n",
            muted,
        ),
        _ if state.watch_section == "findings" => footer.append(
            "↑ ↓ Browse   Enter Evidence   PgUp/PgDn Page   f Filter   s Sort\n",
            muted,
        ),
        _ if state.watch_section == "overview" => footer.append(
            &ui("2 Task details   3 Methods   a Structured data   p Sites\n"),
            muted,
        ),
        _ if ["tasks", "methods", "schema"].contains(&state.watch_section.as_str()) => {
            footer.append(
                "↑ ↓ Browse   Enter Details   PgUp/PgDn Page   f Filter   t State\n",
                muted,
            )
        }
        _ => footer.append(
            &ui("↑ ↓ Browse   Enter Details   PgUp/PgDn Page   f Filter\n"),
            muted,
        ),
    }
    if state.view == "watch" {
        if !sidebar && state.watch_section != "overview" {
            footer.append("↑↓ Enter   ", accent);
            if ["tasks", "methods", "schema", "findings"].contains(&state.watch_section.as_str()) {
                footer.append("f Filter   ", accent);
            }
        }
        footer.append(&ui("n Note   g Goal   ? Keys   q Quit"), accent);
        if !state.note_text.is_empty() || state.note_drafts.values().any(|d| !d.is_empty()) {
            footer.append("   Draft kept", muted);
        }
    }
    if let Some(message) = message {
        footer.append(&format!("\n{message}"), accent);
    }
    footer.no_wrap = true;
    footer.overflow = Some(crate::text::Overflow::Ellipsis);
    if let Some(f) = root.get_mut("footer") {
        f.update(Renderable::Text(footer));
    }
    Renderable::Layout(Box::new(root))
}

/// `int(time.monotonic()) % 4` source for the spinner.
fn secs_monotonic() -> usize {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)) as usize
}

/// The large overview-only composition (metrics cards, scope panel,
/// checklist and findings/notes columns). Split out for file size.
fn overview_panels(
    _state: &mut ShellState,
    snapshot: &Option<Value>,
    palette: &Palette,
    border: &str,
    width: usize,
    height: usize,
    root: &mut Layout,
) {
    let Some(snap) = snapshot else { return };
    let accent = if palette.color { &palette.accent } else { "" };
    let muted = if palette.color { &palette.muted } else { "" };
    let scans = pyjson::path(snap, &["scans", "items"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let latest = scans.first().cloned().unwrap_or(json!({}));
    let evidence = pyjson::obj(&latest, "evidence");
    let counts = pyjson::path(evidence, &["frontier", "counts"])
        .cloned()
        .filter(|v| !v.is_null())
        .unwrap_or(json!({}));
    let latest_artifact = pyjson::path(snap, &["sites", "items"])
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|row| pyjson::str_of(row, "role") == "primary")
        })
        .and_then(|row| {
            pyjson::list_of(row, "scans")
                .iter()
                .find_map(|v| v.as_array())
                .cloned()
        })
        .unwrap_or_default()
        .into_iter()
        .find(|scan| pyjson::str_of(scan, "uuid") == pyjson::str_of(&latest, "uuid"))
        .and_then(|scan| pyjson::path(&scan, &["artifact", "path"]).cloned());
    let native_run = pyjson::path(snap, &["runs", "items"])
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find(|run| {
                pyjson::str_of(run, "kind") == "native"
                    && latest_artifact.is_some()
                    && pyjson::get(run, "artifact") == latest_artifact.as_ref()
                    && pyjson::path(run, &["collector", "config_fingerprint"])
                        == pyjson::get(&latest, "config_fingerprint")
            })
        })
        .cloned();
    let outcomes = pyjson::get(evidence, "committed_page_outcomes");
    let retained_pages = pyjson::get(evidence, "committed_pages")
        .cloned()
        .or_else(|| {
            outcomes
                .and_then(Value::as_object)
                .map(|m| Value::from(m.values().filter_map(Value::as_i64).sum::<i64>()))
        });
    let findings = pyjson::path(evidence, &["findings", "total"]).cloned();
    let mut cards = Grid::new(true, (0, 1, 0, 1));
    for _ in 0..3 {
        cards.add_column(Column {
            ratio: Some(1),
            ..Default::default()
        });
    }
    let card = |value: String, label: String| {
        Renderable::Panel(crate::render::Panel {
            renderable: Box::new(Renderable::Text(Text::new(
                &format!("{value}\n{label}"),
                accent,
            ))),
            title: None,
            title_align: "center".into(),
            border_style: border.into(),
            padding: (0, 1, 0, 1),
            height: None,
            expand: true,
        })
    };
    cards.add_row(vec![
        card(
            retained_pages
                .as_ref()
                .filter(|v| !v.is_null())
                .and_then(Value::as_i64)
                .map(number)
                .unwrap_or_else(|| "—".into()),
            ui("Pages retained"),
        ),
        card(
            findings
                .as_ref()
                .filter(|v| !v.is_null())
                .and_then(Value::as_i64)
                .map(number)
                .unwrap_or_else(|| "—".into()),
            ui("Findings"),
        ),
        card(
            number(
                pyjson::path(snap, &["scans", "total"])
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
            ),
            ui("Saved scans"),
        ),
    ]);
    let metrics = {
        let mut l = Layout::named("metrics");
        l.size = Some(4);
        l
    };
    let crawl = {
        let mut l = Layout::named("crawl");
        l.size = Some(if native_run.is_some() && height >= 32 {
            12
        } else {
            8
        });
        l
    };
    let evidence_layout = Layout::named("evidence");
    let content_children = vec![metrics, crawl, evidence_layout];
    let content_name = if width >= 130 && height >= 26 {
        "content"
    } else {
        "body"
    };
    let content = if let Some(c) = root.get_mut(content_name) {
        c
    } else {
        return;
    };
    content.split_column(content_children);
    if let Some(m) = content.get_mut("metrics") {
        m.update(Renderable::Grid(cards));
    }
    let scan_state =
        if pyjson::truthy(pyjson::get(&latest, "crawl_partial").unwrap_or(&Value::Null)) {
            "PARTIAL".to_string()
        } else {
            pyjson::str_of(&latest, "lifecycle")
                .to_uppercase()
                .replace("NOT_RUN", "NOT RUN")
                .to_string()
        };
    let scan_state = if scan_state.is_empty() {
        "NOT RUN".into()
    } else {
        scan_state
    };
    let frontier_values: Vec<Value> = ["done", "queued", "inflight"]
        .iter()
        .map(|k| pyjson::get(&counts, k).cloned().unwrap_or(Value::Null))
        .collect();
    let discovered = if frontier_values.iter().all(pyjson::is_int) {
        Some(
            frontier_values
                .iter()
                .filter_map(Value::as_i64)
                .sum::<i64>(),
        )
    } else {
        None
    };
    let sitemap = pyjson::path(evidence, &["sitemaps", "fetch_summaries"]).cloned();
    let scan_labels: Vec<String> = [
        scan_state.to_lowercase(),
        pyjson::get(&latest, "source_kind")
            .map(pyjson::py_str)
            .unwrap_or_default(),
        pyjson::get(&latest, "finish_reason")
            .map(pyjson::py_str)
            .unwrap_or_default(),
    ]
    .into_iter()
    .map(|v| human_label(&json!(v)))
    .fold(Vec::new(), |mut acc, label| {
        if !acc.contains(&label) {
            acc.push(label);
        }
        acc
    });
    let mut crawl_lines = vec![
        Text::new(&scan_labels.join("  ·  "), accent),
        panels::meter(
            &if scan_state == "FINISHED" {
                ui("Discovered URL coverage")
            } else {
                ui("Discovered URL coverage · scope can grow")
            },
            pyjson::int_of(&counts, "done"),
            discovered,
            palette,
        ),
        Text::new(
            &format!(
                "{} {}  ·  {} {}  ·  {} {}",
                ui("Queue"),
                pyjson::get(&counts, "queued")
                    .map(pyjson::py_str)
                    .unwrap_or_else(|| ui("unknown")),
                ui("In flight"),
                pyjson::get(&counts, "inflight")
                    .map(pyjson::py_str)
                    .unwrap_or_else(|| ui("unknown")),
                ui("Excluded"),
                pyjson::get(&counts, "excluded")
                    .map(pyjson::py_str)
                    .unwrap_or_else(|| ui("unknown")),
            ),
            "",
        ),
        Text::new(
            &format!(
                "{}: {}",
                ui("Sitemap"),
                if sitemap.as_ref().is_some_and(pyjson::truthy) {
                    value_lines(sitemap.as_ref().unwrap(), "").join(" · ")
                } else {
                    ui("Not measured")
                }
            ),
            if palette.color && !sitemap.as_ref().is_some_and(pyjson::truthy) {
                "#fbbf24"
            } else {
                ""
            },
        ),
    ];
    if let Some(run) = &native_run {
        let collector = pyjson::obj(run, "collector");
        let telemetry = pyjson::obj(run, "telemetry");
        crawl_lines.insert(
            1,
            Text::new(
                &format!(
                    "{} {} · {} · {}",
                    ui("Mode"),
                    human_label(pyjson::get(collector, "mode").unwrap_or(&Value::Null)),
                    human_label(pyjson::get(telemetry, "state").unwrap_or(&Value::Null)),
                    watch_views::rate_text(telemetry)
                ),
                "",
            ),
        );
        if height >= 32 {
            crawl_lines.push(panels::meter(
                &ui("Run URL budget · not whole-site completion"),
                pyjson::path(run, &["counters", "fetched"]).and_then(Value::as_i64),
                pyjson::int_of(collector, "max_urls"),
                palette,
            ));
        }
    }
    if let Some(c) = content.get_mut("crawl") {
        c.update(Renderable::Panel(crate::render::Panel {
            renderable: Box::new(Renderable::Group(
                crawl_lines.into_iter().map(Renderable::Text).collect(),
            )),
            title: Some(ui("Collection & scope")),
            title_align: "left".into(),
            border_style: border.into(),
            padding: (0, 1, 0, 1),
            height: None,
            expand: true,
        }));
    }
    let primary = pyjson::path(snap, &["sites", "items"])
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| pyjson::str_of(item, "role") == "primary")
        })
        .cloned()
        .unwrap_or(json!({}));
    let method_counts = pyjson::path(&primary, &["methods", "kinds"])
        .cloned()
        .unwrap_or(json!({}));
    let mut meters = Grid::new(true, (0, 1, 0, 1));
    meters.add_column(Column {
        ratio: Some(1),
        ..Default::default()
    });
    meters.add_column(Column {
        ratio: Some(1),
        ..Default::default()
    });
    let mut meter_cells = Vec::new();
    for (kind, label) in [("scenario", ui("Scenarios")), ("skill", ui("Skills"))] {
        let record = pyjson::get(&method_counts, kind)
            .cloned()
            .unwrap_or(json!({}));
        meter_cells.push(Renderable::Text(panels::meter(
            &label,
            pyjson::int_of(&record, "completed"),
            pyjson::int_of(&record, "expected"),
            palette,
        )));
    }
    meters.add_row(meter_cells);
    let mut checklist: Vec<Renderable> = vec![Renderable::Grid(meters), Text::blank().into()];
    let active = pyjson::path(snap, &["active_tasks", "items"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    checklist.push(
        Text::new(
            &if active.is_empty() {
                ui("NEXT ACTIONS")
            } else {
                ui("CURRENT WORK")
            },
            accent,
        )
        .into(),
    );
    let work_items = if active.is_empty() {
        pyjson::list_of(pyjson::obj(snap, "progress"), "next_actions").to_vec()
    } else {
        active.clone()
    };
    for item in work_items
        .iter()
        .take((3usize).max(height.saturating_sub(26)))
    {
        let marker = if pyjson::str_of(item, "state") == "completed" {
            "+"
        } else {
            "-"
        };
        let title = pyjson::get(item, "title")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                ui(pyjson::get(item, "action")
                    .or_py(|| pyjson::get(item, "id"))
                    .map(pyjson::py_str)
                    .as_deref()
                    .unwrap_or(""))
            });
        checklist.push(Renderable::Text({
            let mut t = Text::new(
                &format!(
                    "{marker} {} · {}",
                    title,
                    human_label(
                        pyjson::get(item, "display_state")
                            .or_else(|| pyjson::get(item, "state"))
                            .unwrap_or(&Value::Null)
                    )
                ),
                "",
            );
            t.no_wrap = true;
            t.overflow = Some(crate::text::Overflow::Ellipsis);
            t
        }));
    }
    if work_items.is_empty() {
        checklist.push(
            Text::new(
                &ui("No agent task recorded · 2 opens the complete checklist"),
                muted,
            )
            .into(),
        );
    }
    let mut findings_lines: Vec<Renderable> = vec![
        Text::new(&ui("FINDINGS BY SEVERITY"), accent).into(),
        Text::blank().into(),
    ];
    let severity = pyjson::path(evidence, &["findings", "by_severity"])
        .cloned()
        .unwrap_or(json!({}));
    let peak = pyjson::list_of(&severity, "").len();
    let peak = {
        let _ = peak;
        severity
            .as_object()
            .map(|m| m.values().filter_map(Value::as_i64).max().unwrap_or(1))
            .unwrap_or(1)
            .max(1)
    };
    for name in ["critical", "warning", "notice", "opportunity"] {
        let value = if findings.as_ref().is_none_or(Value::is_null) {
            None
        } else {
            pyjson::int_of(&severity, name).or(Some(0))
        };
        let color = if palette.color {
            match name {
                "critical" => "#fb7185",
                "warning" => "#fbbf24",
                "notice" => "#67e8f9",
                _ => "",
            }
        } else {
            ""
        };
        let bar = "━".repeat((15.0 * value.unwrap_or(0) as f64 / peak as f64).round() as usize);
        findings_lines.push(
            Text::new(
                &format!(
                    "{:9} {:>6}  {}",
                    ui(name),
                    value.map(number).unwrap_or_else(|| "—".into()),
                    bar
                ),
                color,
            )
            .into(),
        );
    }
    findings_lines.extend([
        Text::blank().into(),
        Text::new(&ui("PROJECT NOTES"), accent).into(),
    ]);
    let inbox_key = if pyjson::get(snap, "latest_inbox").is_some() {
        "latest_inbox"
    } else {
        "inbox"
    };
    let entries = pyjson::list_of(pyjson::obj(snap, inbox_key), "entries");
    for item in entries.iter().rev().take(3).rev() {
        let mut t = Text::new(pyjson::str_of(item, "text"), "");
        t.no_wrap = true;
        t.overflow = Some(crate::text::Overflow::Ellipsis);
        findings_lines.push(t.into());
    }
    if entries.is_empty() {
        findings_lines.push(Text::new(&ui("n  Write or dictate a note"), muted).into());
    }
    findings_lines.extend([
        Text::blank().into(),
        Text::new(&ui("COMPETITORS"), accent).into(),
    ]);
    let competitors: Vec<Value> = pyjson::path(snap, &["sites", "items"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|item| pyjson::str_of(item, "role") == "competitor")
        .collect();
    for item in competitors.iter().take(5) {
        let host = pyjson::path(item, &["site", "host"])
            .or_py(|| pyjson::path(item, &["site", "target"]))
            .map(pyjson::py_str)
            .unwrap_or_default();
        let total = pyjson::path(item, &["scans", "total"])
            .map(pyjson::py_str)
            .unwrap_or_default();
        let mut t = Text::new(&format!("{host} · scans {total}"), "");
        t.no_wrap = true;
        t.overflow = Some(crate::text::Overflow::Ellipsis);
        findings_lines.push(t.into());
    }
    if competitors.is_empty() {
        findings_lines.push(Text::new(&ui("No competitors configured"), muted).into());
    }
    if height >= 38 {
        findings_lines.extend([
            Text::blank().into(),
            Text::new(&ui("WORKFLOW"), accent).into(),
            {
                let next = pyjson::path(snap, &["execution", "next_action"])
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| "no active workflow".into());
                let mut t = Text::new(&format!("{}: {}", ui("Next"), ui(&next)), "");
                t.no_wrap = true;
                t.overflow = Some(crate::text::Overflow::Ellipsis);
                t.into()
            },
            Text::blank().into(),
            Text::new(&ui("PROJECT LOG"), accent).into(),
        ]);
        let log_text = pyjson::path(snap, &["log", "text"])
            .and_then(Value::as_str)
            .unwrap_or("");
        let keep = (3usize).max(height.saturating_sub(35));
        let log_lines: Vec<&str> = log_text
            .lines()
            .filter(|line| *line != "# Project log")
            .collect();
        for line in log_lines.iter().skip(log_lines.len().saturating_sub(keep)) {
            let mut t = Text::new(line, "");
            t.no_wrap = true;
            t.overflow = Some(crate::text::Overflow::Ellipsis);
            findings_lines.push(t.into());
        }
    }
    let evidence_area = content.get_mut("evidence");
    if let Some(evidence_area) = evidence_area {
        if width >= 130 {
            evidence_area.direction = Direction::Row;
            evidence_area.children = vec![Layout::named("checklist"), Layout::named("insights")];
            if let Some(c) = evidence_area.get_mut("checklist") {
                c.update(Renderable::Panel(crate::render::Panel {
                    renderable: Box::new(Renderable::Group(checklist)),
                    title: Some(ui("Scenarios & skills")),
                    title_align: "left".into(),
                    border_style: border.into(),
                    padding: (0, 1, 0, 1),
                    height: None,
                    expand: true,
                }));
            }
            if let Some(i) = evidence_area.get_mut("insights") {
                i.update(Renderable::Panel(crate::render::Panel {
                    renderable: Box::new(Renderable::Group(findings_lines)),
                    title: Some(ui("Findings & notes")),
                    title_align: "left".into(),
                    border_style: border.into(),
                    padding: (0, 1, 0, 1),
                    height: None,
                    expand: true,
                }));
            }
        } else {
            evidence_area.update(Renderable::Panel(crate::render::Panel {
                renderable: Box::new(Renderable::Group(checklist)),
                title: Some(ui("Scenarios & skills")),
                title_align: "left".into(),
                border_style: border.into(),
                padding: (0, 1, 0, 1),
                height: None,
                expand: true,
            }));
        }
    }
}

/// `run` — launch the interactive shell; returns a process exit code.
///
/// The shell keeps the same flag semantics (`no_color`, `project`,
/// `scan`, `compact`, `lang`). Interactive PTY mode renders through the same
/// frame pipeline and reads keys through `keys`; `stdin_is_tty` decides the
/// compact/noninteractive paths.
#[allow(clippy::too_many_arguments)]
pub fn run(
    no_color: bool,
    project: Option<&str>,
    commands: &[String],
    lang: Option<&str>,
    scan: Option<&str>,
    compact: bool,
    stdin_is_tty: bool,
    stdout_is_tty: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    docs: Option<&dyn panels::CommandDocs>,
) -> i64 {
    let resolved = match crate::localization::resolve_language(lang) {
        Ok(code) => code,
        Err(_) => "en".into(),
    };
    crate::localization::set_language(if resolved == "ru" { "ru" } else { "en" });
    let project = project.map(scan_now::project_directory);
    if (scan.is_some() || compact) && project.is_none() {
        let _ = writeln!(stderr, "--scan and --compact require --project");
        return 1;
    }
    let use_color = crate::theme::color_enabled(no_color, None, stdout_is_tty);
    let palette = crate::theme::resolve_palette(use_color);
    if compact && project.is_some() && !stdin_is_tty {
        let (snapshot, _errors) = watch_snapshot(project.as_deref().unwrap_or_default());
        let snapshot = snapshot.and_then(|s| scan_now::select(&s, scan).ok());
        let panel = scan_now::panel(snapshot.as_ref(), &palette, true);
        let lines = crate::render::render_lines(
            &panel,
            &crate::render::Options {
                max_width: console_width(),
                height: None,
                ..Default::default()
            },
        );
        for line in lines {
            let text: String = line.iter().map(|s| s.text.clone()).collect();
            let _ = writeln!(stdout, "{}", text.trim_end());
        }
        if snapshot.is_none() {
            if scan.is_some() {
                let _ = writeln!(
                    stderr,
                    "selected scan is absent from the retained observation"
                );
            }
            return 1;
        }
        return 0;
    }
    if !stdin_is_tty {
        let _ = writeln!(
            stderr,
            "seohead tui is interactive and needs a terminal; use the noninteractive commands (seohead --help) or `seohead mcp`."
        );
        return 1;
    }
    let mut state = ShellState::new(panels::command_rows(commands));
    if project.is_some() {
        state.view = "watch".into();
        state.note_limit = spiderhead_projects::inbox::MAX_TEXT as i64;
    }
    let host = spiderhead_core::host::SystemHost;
    state.motion_enabled = !matches!(
        spiderhead_core::envvar::read_env(&host, "REDUCED_MOTION")
            .as_deref()
            .map(str::to_lowercase)
            .as_deref(),
        Some("1") | Some("true") | Some("yes")
    );
    let mut refresh = project
        .as_ref()
        .map(|p| ObserverRefresh::new(p, scan.map(str::to_string)));
    let mut message: Option<String> = None;
    // Interactive loop over /dev/tty-style stdin: raw fd is 0.
    let fd = 0;
    let mut raw = match crate::keys::RawMode::enter(fd) {
        Ok(raw) => raw,
        Err(_) => return 1,
    };
    let _ = stdout.write_all(b"\x1b[?2004h");
    let _ = stdout.flush();
    let mut redraw = true;
    let mut next_frame = Instant::now();
    while !state.quit_requested {
        let snapshot = refresh.as_mut().map(|r| r.poll(Some(&mut state)));
        if let Some(r) = refresh.as_mut() {
            if let Some((saved, msg, submitted)) = r.saved.take() {
                message = Some(msg);
                redraw = true;
                if !saved.note_error.is_empty() {
                    if state.note_kind == saved.note_kind {
                        state.note_error = saved.note_error;
                        state.view = "note".into();
                    }
                } else {
                    if state.note_drafts.get(&saved.note_kind) == Some(&submitted) {
                        state.note_drafts.remove(&saved.note_kind);
                    }
                    if state.note_kind == saved.note_kind && state.note_text == submitted {
                        state.note_text.clear();
                        state.note_cursor = Some(0);
                    }
                }
            }
        }
        if redraw || Instant::now() >= next_frame {
            // Terminal size: fall back to 120x32 when ioctl is unavailable.
            let (w, h) = terminal_size().unwrap_or((120, 32));
            let page = refresh.as_ref().map(|r| r.page_for(&state));
            let frame = panels::build_frame(
                &mut state,
                (w as i64 - 4).max(1) as usize,
                (h as i64 - 2).max(1) as usize,
                &palette,
                project.as_deref(),
                message.as_deref(),
                snapshot.as_ref(),
                page.as_ref(),
                compact,
                docs,
            );
            let lines = crate::render::render_lines(
                &frame,
                &crate::render::Options {
                    max_width: w,
                    height: Some(h),
                    ..Default::default()
                },
            );
            let mut buffer = String::from("\x1b[H");
            for line in &lines {
                for seg in line {
                    buffer.push_str(&seg.text);
                }
                buffer.push_str("\x1b[K\n");
            }
            let _ = stdout.write_all(buffer.as_bytes());
            let _ = stdout.flush();
            next_frame = Instant::now() + Duration::from_millis(500);
        }
        let key = match crate::keys::read_key(fd, Some(0.05)) {
            Ok(k) => k,
            Err(_) => "timeout".into(),
        };
        redraw = key != "timeout";
        let deadline = Instant::now() + Duration::from_millis(10);
        let mut key = key;
        for _ in 0..256 {
            let before_view = state.view.clone();
            if key == "enter"
                && state.view == "watch"
                && refresh.as_ref().is_some_and(|r| {
                    pyjson::truthy(
                        pyjson::get(&r.page_for(&state), "loading").unwrap_or(&Value::Null),
                    )
                })
            {
                message = Some("Loading this page; select an entry once it arrives.".into());
                break;
            }
            state.handle_key(&key);
            if state.note_ready
                || state.quit_requested
                || before_view == "watch"
                || state.view == "watch"
                || Instant::now() >= deadline
            {
                break;
            }
            key = match crate::keys::read_key(fd, Some(0.0)) {
                Ok(k) => k,
                Err(_) => break,
            };
            if key == "timeout" {
                break;
            }
        }
        if state.note_ready {
            if let Some(r) = refresh.as_mut() {
                r.save(&state);
                state.note_ready = false;
                message = Some("Saving note locally…".into());
            }
        }
        if refresh.as_ref().is_some_and(|r| r.saving) && state.quit_requested {
            state.quit_requested = false;
            message = Some("Save in progress; waiting for local receipt before exit.".into());
        }
    }
    let _ = stdout.write_all(b"\x1b[?2004l");
    let _ = stdout.flush();
    raw.restore();
    let mut drafts = state.note_drafts.clone();
    drafts.insert(state.note_kind.clone(), state.note_text.clone());
    for (kind, draft) in drafts {
        if !draft.is_empty() {
            let _ = writeln!(
                stdout,
                "Unsaved {} — retained below for recovery:\n{}",
                human_label(&json!(kind)).to_lowercase(),
                draft
            );
        }
    }
    0
}

/// Width of the console the compact line prints to: `COLUMNS`, else the
/// terminal, else 80 columns (Rich's default for a non-terminal).
fn console_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|w| *w > 0)
        .or_else(|| terminal_size().map(|(w, _)| w))
        .unwrap_or(80)
}

/// `console.size` — terminal dimensions via `TIOCGWINSZ` (120x32 fallback).
#[cfg(unix)]
fn terminal_size() -> Option<(usize, usize)> {
    let mut ws = unsafe { std::mem::zeroed::<libc::winsize>() };
    if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
        Some((ws.ws_col as usize, ws.ws_row as usize))
    } else {
        None
    }
}

/// `console.size` where `TIOCGWINSZ` does not exist: caller falls back to 120x32.
#[cfg(not(unix))]
fn terminal_size() -> Option<(usize, usize)> {
    None
}

/// `main` — module entry (`spiderhead-tui --no-color`).
pub fn main(argv: &[String]) -> i64 {
    let no_color = argv.iter().any(|a| a == "--no-color");
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    run(
        no_color,
        None,
        &[],
        None,
        None,
        false,
        true,
        true,
        &mut out,
        &mut err,
        None,
    )
}

/// Keep `Segment` referenced for docs builds.
#[allow(dead_code)]
fn _touch(_: &Segment) {}
