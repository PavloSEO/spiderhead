//! The application model: everything the control channel and the window drive.
//! `dispatch` is egui-free — the headless tests and `spiderhead-desktop-ctl`
//! talk to exactly the same surface the widgets do.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::commands;
use crate::data::filters::{validate_filters, QueryError};
use crate::data::findings::Findings;
use crate::data::query::{self, Query};
use crate::data::session::ScanSession;
use crate::theme::Theme;

/// Dialog ids `ui.window` accepts (quick-crawl is a nav page, not a dialog).
const WINDOW_IDS: &[&str] = &[
    "settings",
    "mcp",
    "cli",
    "help",
    "profile",
    "palette",
    "about",
    "usage",
    "shortcuts",
];

/// Settings page ids `settings.page` accepts: the nav sections plus the source
/// and agent sub-pages that share the dialog.
fn settings_page(id: &str) -> Option<String> {
    const PAGES: &[&str] = &[
        "general",
        "view",
        "scan",
        "core",
        "mcp",
        "sources",
        "agent",
        "agent_config",
        "notify",
        "keys",
        "data",
        "updates",
        "about",
        "source_detail",
        "source_key",
        "sources_auth",
        "spend",
    ];
    PAGES.contains(&id).then(|| id.to_string())
}

/// Rows `table.copy` exports at most, paginated in `MAX_LIMIT` chunks.
const EXPORT_ROW_CAP: i64 = 5000;

/// Columns the simple-mode quick lookup returns.
const SIMPLE_COLUMNS: &[&str] = &[
    "url_id",
    "url",
    "status_code",
    "content_type",
    "title",
    "indexable",
    "crawl_depth",
    "word_count",
    "size_bytes",
    "response_time",
];

/// A clipboard/editing operation the shell performs on the focused widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    /// Undo the last text edit.
    Undo,
    /// Cut the selection to the clipboard.
    Cut,
    /// Copy the selection to the clipboard.
    Copy,
    /// Paste the clipboard into the focused field.
    Paste,
    /// Select the whole field content.
    SelectAll,
}

impl EditAction {
    /// `edit.clipboard` action id.
    pub fn name(self) -> &'static str {
        match self {
            EditAction::Undo => "undo",
            EditAction::Cut => "cut",
            EditAction::Copy => "copy",
            EditAction::Paste => "paste",
            EditAction::SelectAll => "select_all",
        }
    }
    /// Parse an action id.
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "undo" => Some(EditAction::Undo),
            "cut" => Some(EditAction::Cut),
            "copy" => Some(EditAction::Copy),
            "paste" => Some(EditAction::Paste),
            "select_all" => Some(EditAction::SelectAll),
            _ => None,
        }
    }
}

/// Side effects a dispatch asks the shell to perform.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Apply theme visuals.
    SetTheme(Theme),
    /// Close the window.
    Quit,
    /// Capture the viewport to this PNG path.
    Screenshot(PathBuf),
    /// Minimize the window to the Dock.
    Minimize,
    /// Toggle fullscreen on the window.
    Fullscreen,
    /// Set the window's inner size in points (`window.resize`).
    Resize([f32; 2]),
    /// Editing operation on the focused field (`edit.clipboard`).
    Edit(EditAction),
    /// Ask the user for a scan file, then `scan.open` it.
    PickScan,
}

/// The answer of one dispatch: a control reply plus shell side effects.
pub struct Outcome {
    /// Control-channel reply (`Ok` → `result`, `Err` → `error`).
    pub answer: Result<Value, String>,
    /// Shell work for the egui layer.
    pub effects: Vec<Effect>,
}

impl Outcome {
    fn ok(value: Value) -> Self {
        Self {
            answer: Ok(value),
            effects: vec![],
        }
    }
    fn err(e: QueryError) -> Self {
        Self {
            answer: Err(json!({"error": e.message, "reason_code": e.reason_code}).to_string()),
            effects: vec![],
        }
    }
    fn err_msg(message: impl Into<String>) -> Self {
        Self {
            answer: Err(message.into()),
            effects: vec![],
        }
    }
    fn effect(mut self, effect: Effect) -> Self {
        self.effects.push(effect);
        self
    }
}

/// Product mode: which of the three tools the shell shows. `None` on the
/// model means the start screen («Что вы хотите сделать?») is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Crawler: scan a site — URLs, problems, HTML search, scan compare.
    Crawler,
    /// Server-log analysis: bots, coverage, logs × scan.
    Logs,
    /// Agent workspace: a whole project (tasks, inbox, semantics, reports).
    Agent,
}

impl Mode {
    /// Every mode in start-screen order.
    pub const ALL: [Mode; 3] = [Mode::Crawler, Mode::Logs, Mode::Agent];

    /// `ui.mode` id.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Crawler => "crawler",
            Mode::Logs => "logs",
            Mode::Agent => "agent",
        }
    }
    /// Parse a mode id.
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "crawler" => Some(Mode::Crawler),
            "logs" => Some(Mode::Logs),
            "agent" => Some(Mode::Agent),
            _ => None,
        }
    }
    /// Home navigation section when entering the mode.
    pub fn home(self) -> Nav {
        match self {
            Mode::Crawler => Nav::Crawler,
            Mode::Logs => Nav::Logs,
            Mode::Agent => Nav::Work,
        }
    }
    /// Sections the side navigation lists in this mode.
    pub fn navs(self) -> &'static [Nav] {
        match self {
            Mode::Crawler => &[
                Nav::Crawler,
                Nav::Urls,
                Nav::Dashboard,
                Nav::Search,
                Nav::Compare,
                Nav::CompareUrl,
                Nav::Check,
            ],
            Mode::Logs => &[Nav::Logs, Nav::LogsBots, Nav::LogsCompare],
            Mode::Agent => &[
                Nav::Work,
                Nav::Inbox,
                Nav::Projects,
                Nav::Urls,
                Nav::Dashboard,
                Nav::Search,
                Nav::Compare,
                Nav::CompareUrl,
            ],
        }
    }
}

/// Navigation section of the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    /// URL table.
    Urls,
    /// Findings dashboard (canvas «Проблемы»).
    Dashboard,
    /// Quick-crawl page (crawler-mode home).
    Crawler,
    /// HTML content search.
    Search,
    /// Two-scan comparison.
    Compare,
    /// One-URL comparison (reached from a compare row, not the left nav).
    CompareUrl,
    /// One-URL check: the former simple page, now a crawler section.
    Check,
    /// Log-analysis overview (logs-mode home).
    Logs,
    /// Bot breakdown of the logs.
    LogsBots,
    /// Logs × scan coverage.
    LogsCompare,
    /// Project workbench (agent-mode home).
    Work,
    /// Inbox placeholder.
    Inbox,
    /// Projects placeholder.
    Projects,
}

impl Nav {
    /// `ui.navigate` id.
    pub fn name(self) -> &'static str {
        match self {
            Nav::Urls => "urls",
            Nav::Dashboard => "dashboard",
            Nav::Crawler => "crawler",
            Nav::Search => "search",
            Nav::Compare => "compare",
            Nav::CompareUrl => "compare_url",
            Nav::Check => "check",
            Nav::Logs => "logs",
            Nav::LogsBots => "logs_bots",
            Nav::LogsCompare => "logs_compare",
            Nav::Work => "work",
            Nav::Inbox => "inbox",
            Nav::Projects => "projects",
        }
    }
    /// Parse a navigation id.
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "urls" => Some(Nav::Urls),
            "dashboard" => Some(Nav::Dashboard),
            "crawler" => Some(Nav::Crawler),
            "search" => Some(Nav::Search),
            "compare" => Some(Nav::Compare),
            "compare_url" => Some(Nav::CompareUrl),
            "check" => Some(Nav::Check),
            "logs" => Some(Nav::Logs),
            "logs_bots" => Some(Nav::LogsBots),
            "logs_compare" => Some(Nav::LogsCompare),
            "work" => Some(Nav::Work),
            "inbox" => Some(Nav::Inbox),
            "projects" => Some(Nav::Projects),
            _ => None,
        }
    }
}

/// URL-table state: the query it would re-run plus the last answer.
#[derive(Debug, Default)]
pub struct TableState {
    /// Filters/sort/direction/columns the table currently applies.
    pub query: Query,
    /// Currently selected page row (`url_id`), drives the details panel.
    pub selected: Option<i64>,
    /// The last `table.page` answer, for the status strip.
    pub last: Option<Value>,
    /// Scroll cache: rows fetched starting at `cache_start` under `cache_key`.
    pub cache_start: i64,
    /// Scroll cache rows (row index → `cache_start + i`).
    pub cache_rows: Vec<Value>,
    /// Query fingerprint the cache belongs to; stale on any table change.
    pub cache_key: String,
    /// The previous window (same shape): it keeps answering while a new one
    /// loads, so fast scrolling shows data, not placeholders.
    pub cache_prev: Option<(String, i64, Vec<Value>)>,
}

impl TableState {
    /// One row of the grid at absolute filtered offset, fetching as needed.
    ///
    /// The fetch window is 500 rows centered on the request (`start =
    /// max(0, i − 250)`); a request fires only when the visible row left the
    /// sweet zone of 300 rows inside the current window, so scrolling inside
    /// it costs nothing. While a new window loads, the previous one still
    /// answers; rows outside both paint as placeholders. At most two windows
    /// (current + previous) are held — memory does not grow with the table.
    /// `None` = no scan or the row is past `filtered_total`.
    pub fn row_at(&mut self, scan: Option<&mut ScanSession>, index: i64) -> Option<Value> {
        let scan = scan?;
        let key = format!(
            "{}|{}|{}|{}|{}|{}|{}",
            scan.path.display(),
            serde_json::to_string(&self.query.filters).unwrap_or_default(),
            self.query.sort.as_deref().unwrap_or(""),
            self.query.direction,
            self.query.preset.as_deref().unwrap_or(""),
            self.query.issue_check,
            self.query.issue_severity,
        );
        let in_window = |start: i64, rows: &[Value], i: i64| {
            i >= start && (i as usize) < start as usize + rows.len()
        };
        if self.cache_key != key {
            // The query changed: the old window keeps answering while the
            // new one loads.
            self.cache_prev = Some((
                std::mem::take(&mut self.cache_key),
                self.cache_start,
                std::mem::take(&mut self.cache_rows),
            ));
        }
        if in_window(self.cache_start, &self.cache_rows, index) {
            // The sweet zone: a fetch happens only near the window edges.
            let len = self.cache_rows.len() as i64;
            let in_sweet = index >= self.cache_start + 100 && index < self.cache_start + len - 100;
            if in_sweet || len < 500 {
                return self
                    .cache_rows
                    .get((index - self.cache_start) as usize)
                    .cloned();
            }
        }
        if let Some((prev_key, prev_start, prev_rows)) = &self.cache_prev {
            if *prev_key == key && in_window(*prev_start, prev_rows, index) {
                return prev_rows.get((index - prev_start) as usize).cloned();
            }
        }
        // The 500-row window goes out as pages of MAX_LIMIT (the control
        // command's parity cap), merged back into one window.
        let want_start = (index - 250).max(0);
        let mut rows: Vec<Value> = Vec::with_capacity(500);
        let mut q = self.query.clone();
        q.facets = serde_json::Value::Null;
        let mut filtered = self
            .last
            .as_ref()
            .and_then(|l| l.get("filtered_total"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        while (rows.len() as i64) < 500 {
            q.offset = want_start + rows.len() as i64;
            q.limit = crate::data::columns::MAX_LIMIT;
            let page = match query::run(scan, &q) {
                Ok(p) => p,
                Err(_) => break,
            };
            if let Some(f) = page.get("filtered_total").and_then(Value::as_i64) {
                filtered = f;
            }
            let batch = page
                .get("rows")
                .and_then(|r| r.as_array())
                .cloned()
                .unwrap_or_default();
            let done = batch.len() < crate::data::columns::MAX_LIMIT as usize;
            rows.extend(batch);
            if done {
                break;
            }
        }
        if rows.is_empty() || !in_window(want_start, &rows, index) {
            return None;
        }
        self.cache_prev = Some((
            std::mem::take(&mut self.cache_key),
            self.cache_start,
            std::mem::take(&mut self.cache_rows),
        ));
        self.cache_key = key;
        self.cache_start = want_start;
        self.cache_rows = rows;
        // Scroll fetches skip the facet counts; carry the last full answer's
        // facets and totals so the group strip does not flicker empty.
        let (facets, states, total) = match &self.last {
            Some(old) => (
                old.get("facets").cloned().unwrap_or(Value::Null),
                old.get("facet_states").cloned().unwrap_or(Value::Null),
                old.get("total").cloned().unwrap_or(Value::Null),
            ),
            None => (Value::Null, Value::Null, Value::Null),
        };
        self.last = Some(serde_json::json!({
            "rows": self.cache_rows,
            "filtered_total": filtered,
            "total": total,
            "facets": facets,
            "facet_states": states,
        }));
        self.cache_rows
            .get((index - self.cache_start) as usize)
            .cloned()
    }
}

/// One TSV cell: `null`/empty → blank, booleans as `true`/`false`,
/// whitespace flattened so a field never breaks a row.
fn tsv_cell(v: &Value) -> String {
    let text = match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    text.replace(['\t', '\n', '\r'], " ")
}

/// HTML-search screen state (`search.options`/`search.run`/`search.select`).
#[derive(Debug, Default)]
pub struct SearchState {
    /// Query text of the last `search.run`.
    pub query: String,
    /// Scope chip id (`text`/`html`/`head`/`css`/`dom`).
    pub scope: String,
    /// CSS selector text of the `css` scope.
    pub selector: String,
    /// Case-sensitive literal matching.
    pub case_sensitive: bool,
    /// «Только HTML 200» checkbox.
    pub only_200: bool,
    /// Last `search.run` answer.
    pub result: Option<Value>,
    /// Hit index shown in the preview pane.
    pub selected: usize,
    /// Occurrence index inside the selected hit (`match_index`-1).
    pub occurrence: usize,
}

impl SearchState {
    /// Fresh state: canvas defaults (visible-text scope).
    pub fn new() -> Self {
        Self {
            scope: "text".into(),
            ..Self::default()
        }
    }
}

/// Scan-comparison screen state (`compare.*`).
#[derive(Debug)]
pub struct CompareState {
    /// `before` scan path.
    pub before: Option<String>,
    /// `after` scan path.
    pub after: Option<String>,
    /// Active tab (`fixed`/`better`/`worse`/`gone`/`new`).
    pub tab: String,
    /// Metric of the better/worse tabs (`words`/`inlinks`/`response`).
    pub metric: String,
    /// Last `compare.run` answer (summary + first page of the active tab).
    pub result: Option<Value>,
    /// URL opened on the URL-compare screen.
    pub url: String,
    /// Last `compare.url` answer.
    pub url_result: Option<Value>,
}

impl Default for CompareState {
    fn default() -> Self {
        Self {
            before: None,
            after: None,
            tab: "fixed".into(),
            metric: "words".into(),
            result: None,
            url: String::new(),
            url_result: None,
        }
    }
}

/// The whole application. Construct once; `dispatch` mutates.
pub struct AppModel {
    /// Product mode (`ui.mode`); `None` = the start screen chooser.
    pub mode: Option<Mode>,
    /// Active theme (`ui.theme`).
    pub theme: Theme,
    /// Active navigation section (`ui.navigate`).
    pub nav: Nav,
    /// The open scan, when one is.
    pub scan: Option<ScanSession>,
    /// Dashboard model, loaded once per scan (`None` = companion missing).
    pub findings: Option<Option<Findings>>,
    /// URL-table state.
    pub table: TableState,
    /// Widget state of the big table: zoom, column layout, selection,
    /// scroll, transients. Lives in the model so the control channel drives
    /// exactly what the window shows (`table.column.*`, `table.zoom`, …).
    pub grid: crate::ui::grid::GridUi,
    /// Visible details tab (`details.load`); clicks and ctl converge here.
    pub details_tab: String,
    /// Last `simple.check` reply, shown on the simple page.
    pub simple_result: Option<Value>,
    /// Dialog windows currently open (`ui.window`).
    pub windows: std::collections::BTreeSet<String>,
    /// One-line status message (last action result).
    pub status: String,
    /// Check selected on the dashboard (`dashboard.select`), registry case.
    pub dashboard_selected: Option<String>,
    /// Severity chip of the issues list (`issues.severity`; `""` = all).
    pub issues_severity: String,
    /// Last `issues.urls` answer of the selected check: `(check, offset, page)`.
    pub issue_page: Option<(String, i64, Value)>,
    /// HTML-search screen state.
    pub search: SearchState,
    /// Scan-comparison screen state.
    pub compare: CompareState,
    /// Navigation folded to the icon rail (`ui.sidebar`).
    pub nav_collapsed: bool,
    /// Details panel visible under the URL table (`details.toggle`).
    pub details_open: bool,
    /// Per-computer settings store (`settings.*` commands).
    pub settings: crate::settings::AppSettings,
    /// Visible settings page (`settings.page`).
    pub settings_page: String,
    /// Provider a source detail/key page is bound to (`settings.page`).
    pub settings_provider: Option<String>,
    /// OS is in dark mode (`ctx.system_theme`); resolves `view.theme = system`.
    pub system_dark: bool,
    /// `ui.find` set this: the URL table focuses its filter field once.
    pub find_focus: bool,
}

impl AppModel {
    /// A fresh model: start-screen chooser, dark theme, no scan; the grid
    /// layout comes back from the settings file.
    pub fn new() -> Self {
        Self::with_settings(crate::settings::AppSettings::load(
            crate::settings::default_path(&spiderhead_core::host::SystemHost),
        ))
    }

    /// A fresh model over an explicit settings store (tests use in-memory).
    pub fn with_settings(settings: crate::settings::AppSettings) -> Self {
        let mut grid = crate::ui::grid::GridUi::default();
        let mut table = TableState::default();
        let stored = settings.get_str("table.urls.layout");
        if stored.is_empty() {
            // A fresh layout pins the address column (CL-20's default).
            let names: Vec<&str> = crate::data::columns::COLUMNS
                .iter()
                .map(|(n, _)| *n)
                .collect();
            grid.layout.freeze(1, &names);
        }
        if let Ok(v) = serde_json::from_str::<Value>(&stored) {
            if let Some(layout) = v.get("layout") {
                grid.layout = crate::ui::grid::ColLayout::from_json(layout);
            }
            if let Some(p) = v.get("zoom").and_then(Value::as_f64) {
                grid.zoom.set_percent(p as f32);
            }
            // The sort comes back with the layout (PS-03); the filter does not.
            if let Some(sort) = v.get("sort").and_then(Value::as_str) {
                table.query.sort = Some(sort.to_string());
            }
            if let Some(dir) = v.get("direction").and_then(Value::as_str) {
                table.query.direction = dir.to_string();
            }
        }
        Self {
            mode: None,
            theme: Theme::default(),
            nav: Nav::Urls,
            scan: None,
            findings: None,
            table,
            grid,
            details_tab: "summary".into(),
            simple_result: None,
            windows: std::collections::BTreeSet::new(),
            status: String::new(),
            dashboard_selected: None,
            issues_severity: String::new(),
            issue_page: None,
            search: SearchState::new(),
            compare: CompareState::default(),
            nav_collapsed: false,
            details_open: true,
            settings,
            settings_page: "general".into(),
            settings_provider: None,
            system_dark: false,
            find_focus: false,
        }
    }

    /// Snapshot the grid's column layout, zoom and sort into the settings
    /// store (widths, order, hidden columns, freeze, zoom and the sort
    /// survive the restart; the filter does not — PS-07).
    pub fn persist_grid(&mut self) {
        let v = json!({
            "layout": self.grid.layout.to_json(),
            "zoom": self.grid.zoom.percent(),
            "sort": self.table.query.sort,
            "direction": self.table.query.direction,
        });
        if self.settings.set("table.urls.layout", v).is_ok() {
            let _ = self.settings.save();
        }
    }

    /// The column list the big table shows: `url_id` for row identity plus
    /// the layout's visible order. The live query's columns follow it, so a
    /// hide/show/move re-queries the rows with the same keys.
    pub fn grid_columns_wanted(&self) -> Vec<String> {
        let mut base: Vec<&str> = if self.table.query.columns.is_empty() {
            crate::data::columns::DEFAULT_COLUMNS.to_vec()
        } else {
            self.table
                .query
                .columns
                .iter()
                .map(String::as_str)
                .collect()
        };
        base.retain(|c| *c != "url_id");
        // Columns the chooser brought in ride on top of the tab's set.
        let extra: Vec<&str> = crate::data::columns::COLUMNS
            .iter()
            .map(|(n, _)| *n)
            .filter(|n| !base.contains(n) && !self.grid.layout.is_hidden(n))
            .collect();
        let _ = extra;
        let mut cols = vec!["url_id".to_string()];
        cols.extend(
            self.grid
                .layout
                .visible(&base)
                .into_iter()
                .filter(|c| c != "url_id"),
        );
        cols
    }

    /// Re-query the table when the layout's column set/order diverges from
    /// the live query (a drag or a hide/show happened).
    pub fn sync_table_columns(&mut self) {
        let want = self.grid_columns_wanted();
        if self.table.query.columns != want {
            let params = json!({"columns": want, "offset": self.table.query.offset});
            let _ = self.table_page(&params);
        }
    }

    /// The registry snapshot for `commands.list` and menu building.
    pub fn commands(&self) -> Vec<spiderhead_control::CommandInfo> {
        commands::registry(self.scan.is_some())
    }

    /// One control request → answer + shell effects.
    pub fn dispatch(&mut self, method: &str, params: &Value) -> Outcome {
        match method {
            "app.quit" => Outcome::ok(json!({"quitting": true})).effect(Effect::Quit),
            "app.install_cli" => match crate::integrate::install_cli() {
                Ok(done) => {
                    self.status = done["message"].as_str().unwrap_or_default().to_owned();
                    Outcome::ok(done)
                }
                Err(message) => Outcome::err_msg(message),
            },
            "app.connect_claude" => {
                let done = crate::integrate::connect_claude();
                self.status = done["message"].as_str().unwrap_or_default().to_owned();
                Outcome::ok(done)
            }
            "commands.list" => {
                let search = params.get("search").and_then(Value::as_str).unwrap_or("");
                Outcome::ok(commands::list_value(&self.commands(), search))
            }
            "ui.theme" => {
                let mode = params
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| self.theme.name());
                match Theme::parse(mode) {
                    Some(t) => {
                        self.theme = t;
                        Outcome::ok(json!({"mode": t.name()})).effect(Effect::SetTheme(t))
                    }
                    None => Outcome::err_msg(format!("unknown theme {mode:?} (light, dark or hc)")),
                }
            }
            "ui.mode" => {
                let id = params
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| self.mode.map(Mode::name).unwrap_or("start"));
                match id {
                    "start" | "choose" => {
                        self.mode = None;
                        Outcome::ok(json!({"mode": "start"}))
                    }
                    _ => match Mode::parse(id) {
                        Some(mode) => self.set_mode(mode, params),
                        None => Outcome::err_msg(format!(
                            "unknown mode {id:?} (crawler, logs, agent or start)"
                        )),
                    },
                }
            }
            "ui.find" => {
                // The filter box lives on the urls table; if the active mode
                // cannot host it (chooser, logs), land on the crawler section.
                if self.mode.is_none_or(|m| !m.navs().contains(&Nav::Urls)) {
                    self.mode = Some(Mode::Crawler);
                }
                self.nav = Nav::Urls;
                self.find_focus = true;
                Outcome::ok(json!({"nav": "urls", "focus": "filter"}))
            }
            "edit.clipboard" => {
                let action = params.get("action").and_then(Value::as_str).unwrap_or("");
                match EditAction::parse(action) {
                    Some(op) => Outcome::ok(json!({"action": op.name()})).effect(Effect::Edit(op)),
                    None => Outcome::err_msg(format!(
                        "unknown edit action {action:?} (undo, cut, copy, paste, select_all)"
                    )),
                }
            }
            "window.minimize" => Outcome::ok(json!({"minimized": true})).effect(Effect::Minimize),
            "window.resize" => {
                let side = |k: &str| params.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
                let (w, h) = (side("width"), side("height"));
                // Below the shell's own minimum the layout is undefined, not just cramped.
                if !(640.0..=8192.0).contains(&w) || !(480.0..=8192.0).contains(&h) {
                    return Outcome::err_msg("width 640..8192, height 480..8192");
                }
                Outcome::ok(json!({"width": w, "height": h})).effect(Effect::Resize([w, h]))
            }
            "window.fullscreen" => {
                Outcome::ok(json!({"fullscreen": "toggle"})).effect(Effect::Fullscreen)
            }
            "ui.navigate" => {
                let id = params.get("id").and_then(Value::as_str).unwrap_or("");
                match Nav::parse(id) {
                    Some(nav) => {
                        self.nav = nav;
                        Outcome::ok(json!({"nav": nav.name()}))
                    }
                    None => Outcome::err_msg(format!(
                        "unknown section {id:?} (urls, dashboard, crawler, search, compare, compare_url, check, logs, logs_bots, logs_compare, work, inbox, projects)"
                    )),
                }
            }
            "ui.window" => {
                let id = params.get("id").and_then(Value::as_str).unwrap_or("");
                if !WINDOW_IDS.contains(&id) {
                    return Outcome::err_msg(format!(
                        "unknown window {id:?} ({})",
                        WINDOW_IDS.join(", ")
                    ));
                }
                let open = params.get("open").and_then(Value::as_bool).unwrap_or(true);
                if open {
                    self.windows.insert(id.to_string());
                } else {
                    self.windows.remove(id);
                }
                Outcome::ok(json!({"id": id, "open": open}))
            }
            "ui.screenshot" => {
                let path = params
                    .get("path")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("screenshot.png"));
                Outcome::ok(json!({"queued": true, "path": path})).effect(Effect::Screenshot(path))
            }
            "ui.inspect" => {
                let filtered = self
                    .table
                    .last
                    .as_ref()
                    .and_then(|l| l.get("filtered_total"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let table_state = json!({
                    "empty": filtered == 0,
                    "rows": {"count": filtered},
                    "selection": {"count": self.grid.selection.len_total(filtered.max(0) as usize)},
                    "columns": self.grid_columns_wanted(),
                    "zoom": self.grid.zoom.percent(),
                    "sort": {"column": self.table.query.sort, "dir": self.table.query.direction},
                });
                Outcome::ok(json!({
                    "backend": "egui",
                    "project": spiderhead_core::project_root::discover_here(),
                    "theme": self.theme.name(),
                    "mode": self.mode.map(Mode::name).unwrap_or("start"),
                    "nav": self.nav.name(),
                    "dashboard_selected": self.dashboard_selected,
                    "issues_severity": self.issues_severity,
                    "search_scope": self.search.scope,
                    "compare_before": self.compare.before,
                    "compare_after": self.compare.after,
                    "compare_tab": self.compare.tab,
                    "nav_collapsed": self.nav_collapsed,
                    "scan": self.scan.as_ref().map(|s| s.path.display().to_string()),
                    "table": table_state,
                    "windows": [json!({"window": 1, "title": "SpiderHead", "active": true, "dirty": false})],
                    "dialogs": self.windows.iter().collect::<Vec<_>>(),
                    "activeWindow": 1,
                }))
            }
            "window.list" => Outcome::ok(json!([
                {"window": 1, "title": "SpiderHead", "active": true, "dirty": false}
            ])),
            "scan.open" => {
                if params
                    .get("dialog")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    Outcome::ok(json!({"dialog": true})).effect(Effect::PickScan)
                } else {
                    self.scan_open(params)
                }
            }
            "scan.close" => {
                self.scan = None;
                self.findings = None;
                self.table = TableState::default();
                self.dashboard_selected = None;
                self.issues_severity = String::new();
                self.issue_page = None;
                self.search = SearchState::new();
                self.compare = CompareState::default();
                self.status = String::new();
                Outcome::ok(json!({"closed": true}))
            }
            "scan.info" => self.with_scan(|s| {
                let pages: i64 = s
                    .con
                    .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
                    .unwrap_or(0);
                let urls: i64 = s
                    .con
                    .query_row("SELECT COUNT(*) FROM urls", [], |r| r.get(0))
                    .unwrap_or(0);
                Ok(json!({
                    "path": s.path.display().to_string(),
                    "source": s.source,
                    "pages": pages,
                    "urls": urls,
                    "audit": s.audit_available(),
                }))
            }),
            "table.page" => self.table_page(params),
            "table.sort" | "table.filter" => {
                let is_filter = method == "table.filter";
                let mut out = self.table_update(params);
                // The sort choice survives the restart next to the layout.
                if !is_filter && out.answer.is_ok() {
                    self.persist_grid();
                }
                // A new filter reshapes the row set: the selection starts
                // over (FL-02). A pure reload (F5) keeps it.
                if is_filter
                    && params.as_object().is_some_and(|o| {
                        o.contains_key("filters")
                            || o.contains_key("preset")
                            || o.contains_key("issue_check")
                            || o.contains_key("issue_severity")
                    })
                {
                    self.grid.selection.clear();
                    self.table.selected = None;
                    if let Ok(v) = &out.answer {
                        let mut v = v.clone();
                        if let Some(o) = v.as_object_mut() {
                            o.insert("selection".into(), json!("cleared"));
                        }
                        out = Outcome::ok(v);
                    }
                }
                out
            }
            "table.select" => {
                if self.scan.is_none() {
                    Outcome::err_msg("no scan is open")
                } else {
                    match params.get("url_id") {
                        Some(Value::Null) => {
                            self.table.selected = None;
                            self.grid.selection.clear();
                            Outcome::ok(json!({"selected": Value::Null}))
                        }
                        Some(v) => match v.as_i64() {
                            Some(id) => {
                                self.table.selected = Some(id);
                                self.grid.selection.click(&id.to_string());
                                Outcome::ok(json!({"selected": id}))
                            }
                            None => Outcome::err_msg("url_id must be a number or null"),
                        },
                        None => Outcome::err_msg("url_id is required"),
                    }
                }
            }
            "table.copy" => {
                let selected = params.get("selected").and_then(Value::as_array);
                let header = params
                    .get("header")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                self.table_copy(selected, header)
            }
            // Column set and order live in the grid's layout; the visible
            // columns are the layout's business (CL-01/CL-23).
            "table.column.hide" => {
                let name = params.get("column").and_then(Value::as_str).unwrap_or("");
                match crate::data::columns::column(name) {
                    Some(_) => {
                        let names: Vec<&str> = crate::data::columns::COLUMNS
                            .iter()
                            .map(|(n, _)| *n)
                            .collect();
                        self.grid.layout.hide(name, &names);
                        // Sorting by a hidden column reads nothing (SO-10).
                        if self.table.query.sort.as_deref() == Some(name) {
                            self.table.query.sort = None;
                            self.table.query.direction = "asc".into();
                        }
                        self.sync_table_columns();
                        self.persist_grid();
                        Outcome::ok(json!({"column": name, "visible": false}))
                    }
                    None => Outcome::err_msg(format!("unknown column {name:?}")),
                }
            }
            "table.column.show" => {
                let name = params.get("column").and_then(Value::as_str).unwrap_or("");
                match crate::data::columns::column(name) {
                    Some(_) => {
                        self.grid.layout.show(name);
                        // A column outside the tab's set joins the query.
                        if !self.table.query.columns.is_empty()
                            && !self.table.query.columns.iter().any(|c| c == name)
                        {
                            let mut cols = self.table.query.columns.clone();
                            cols.push(name.to_string());
                            let _ = self.table_page(
                                &json!({"columns": cols, "offset": self.table.query.offset}),
                            );
                        }
                        self.sync_table_columns();
                        self.persist_grid();
                        Outcome::ok(json!({"column": name, "visible": true}))
                    }
                    None => Outcome::err_msg(format!("unknown column {name:?}")),
                }
            }
            "table.column.move" => {
                let name = params.get("column").and_then(Value::as_str).unwrap_or("");
                let before = params.get("before").and_then(Value::as_str);
                let names: Vec<&str> = crate::data::columns::COLUMNS
                    .iter()
                    .map(|(n, _)| *n)
                    .collect();
                if crate::data::columns::column(name).is_none() {
                    return Outcome::err_msg(format!("unknown column {name:?}"));
                }
                self.grid.layout.move_before(name, before, &names);
                self.sync_table_columns();
                self.persist_grid();
                Outcome::ok(json!({"column": name, "before": before}))
            }
            "table.column.pin" => {
                let names: Vec<&str> = crate::data::columns::COLUMNS
                    .iter()
                    .map(|(n, _)| *n)
                    .collect();
                let upto = params.get("upto").and_then(Value::as_str);
                let frozen = match upto {
                    None | Some("") => Some(0_usize),
                    Some(name) => self
                        .grid
                        .layout
                        .visible(&names)
                        .iter()
                        .position(|c| c == name)
                        .map(|i| i + 1),
                };
                match frozen {
                    Some(count) => {
                        self.grid.layout.freeze(count, &names);
                        self.persist_grid();
                        Outcome::ok(json!({"frozen": self.grid.layout.frozen()}))
                    }
                    None => Outcome::err_msg(format!("column {upto:?} is not visible")),
                }
            }
            "table.columns.reset" => {
                self.grid.layout.reset();
                self.grid.zoom.set_percent(100.0);
                // The tab's set returns to the default too, not just the
                // visibility/order/widths (CL-08).
                let def: Vec<String> = crate::data::columns::UI_DEFAULT_COLUMNS
                    .iter()
                    .map(|c| c.to_string())
                    .collect();
                let _ = self.table_page(&json!({"columns": def, "offset": 0}));
                self.persist_grid();
                Outcome::ok(json!({"reset": true}))
            }
            "table.columns.configure" => {
                let open = params.get("open").and_then(Value::as_bool).unwrap_or(true);
                if open {
                    self.windows.insert("columns".into());
                } else {
                    self.windows.remove("columns");
                }
                Outcome::ok(json!({"open": open}))
            }
            "table.zoom" => {
                if let Some(p) = params.get("percent").and_then(Value::as_f64) {
                    self.grid.zoom.set_percent(p as f32);
                } else {
                    let dir = params.get("step").and_then(Value::as_str).unwrap_or("in");
                    self.grid.zoom.step(if dir == "out" { -1 } else { 1 });
                }
                self.persist_grid();
                Outcome::ok(json!({"percent": self.grid.zoom.percent()}))
            }
            "ui.scroll_to" => {
                let row = params.get("row").and_then(Value::as_i64);
                match row {
                    Some(r) if r >= 0 => {
                        // Base-point offset: the row tops the view at any zoom.
                        // The position is the top row number (f64 model).
                        self.grid.scroll_y = r as f64;
                        self.grid.scroll_target_y = r as f64;
                        self.grid.active_index = Some(r);
                        Outcome::ok(json!({"row": r}))
                    }
                    _ => Outcome::err_msg("ui.scroll_to needs {\"row\": n ≥ 0}"),
                }
            }
            "details.load" => {
                let tab = params
                    .get("tab")
                    .and_then(Value::as_str)
                    .unwrap_or("summary")
                    .to_string();
                let out = self
                    .with_scan(|s| crate::data::details::load(s, params).map_err(|e| e.message));
                if out.answer.is_ok() {
                    self.details_tab = tab;
                }
                out
            }
            "details.copy" => self.details_copy(params),
            "details.toggle" => {
                self.details_open = params
                    .get("open")
                    .and_then(Value::as_bool)
                    .unwrap_or(!self.details_open);
                Outcome::ok(json!({"open": self.details_open}))
            }
            "simple.check" => {
                let out = self.simple_check(params);
                if let Ok(v) = &out.answer {
                    self.simple_result = Some(v.clone());
                }
                out
            }
            "dashboard.inspect" => match self.findings.as_ref().and_then(|f| f.as_ref()) {
                Some(f) => Outcome::ok(f.inspect()),
                None => Outcome::ok(json!({
                    "state": "unavailable",
                    "reason": "no audit.v2 companion next to the open scan",
                })),
            },
            "dashboard.filter" => self.dashboard_filter(params),
            "ui.sidebar" => {
                self.nav_collapsed = params
                    .get("collapsed")
                    .and_then(Value::as_bool)
                    .unwrap_or(!self.nav_collapsed);
                Outcome::ok(json!({"collapsed": self.nav_collapsed}))
            }
            "dashboard.select" => {
                self.dashboard_selected = params
                    .get("check")
                    .and_then(Value::as_str)
                    .map(str::to_uppercase);
                self.issue_page = None;
                Outcome::ok(json!({"selected": self.dashboard_selected}))
            }
            "settings.list" => Outcome::ok(self.settings.describe()),
            "settings.get" => match params.get("key").and_then(Value::as_str) {
                Some(key) => match crate::settings::definition(key) {
                    Some(_) => Outcome::ok(json!({key: self.settings.get(key)})),
                    None => Outcome::err_msg(format!("unknown setting {key:?}")),
                },
                None => Outcome::ok(
                    self.settings
                        .export()
                        .get("settings")
                        .cloned()
                        .unwrap_or(Value::Null),
                ),
            },
            "settings.set" => {
                let key = params.get("key").and_then(Value::as_str).unwrap_or("");
                let value = params.get("value").cloned().unwrap_or(Value::Null);
                match self.settings.set(key, value) {
                    Ok(changed) => {
                        let saved = self.settings.save().is_ok();
                        let mut out = Outcome::ok(json!({
                            "key": key, "value": self.settings.get(key),
                            "changed": changed, "saved": saved,
                        }));
                        for effect in self.apply_setting(key) {
                            out = out.effect(effect);
                        }
                        out
                    }
                    Err(message) => Outcome::err_msg(message),
                }
            }
            "settings.reset" => {
                let prefix = params.get("prefix").and_then(Value::as_str).unwrap_or("");
                let cleared = self.settings.reset(prefix);
                let saved = self.settings.save().is_ok();
                Outcome::ok(json!({"prefix": prefix, "cleared": cleared, "saved": saved}))
            }
            "settings.page" => {
                let id = params.get("id").and_then(Value::as_str).unwrap_or("");
                match settings_page(id) {
                    Some(page) => {
                        self.settings_page = page.clone();
                        self.settings_provider = params
                            .get("provider")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        self.windows.insert("settings".to_string());
                        Outcome::ok(json!({"page": page}))
                    }
                    None => Outcome::err_msg(format!("unknown settings page {id:?}")),
                }
            }
            "settings.crawl_defaults" => Outcome::ok(self.settings.crawl_defaults()),
            // The release feed is not wired in this build — the checker is
            // honest about it instead of pretending «latest».
            "settings.update_check" => Outcome::ok(json!({
                "installed": env!("CARGO_PKG_VERSION"),
                "core": env!("CARGO_PKG_VERSION"),
                "last_check": Value::Null,
                "state": "unavailable",
            })),
            "settings.export" => {
                let path = params.get("path").and_then(Value::as_str).unwrap_or("");
                if path.is_empty() {
                    Outcome::err_msg("settings.export needs {\"path\": \"…\"}")
                } else {
                    let body =
                        serde_json::to_vec_pretty(&self.settings.export()).unwrap_or_default();
                    match std::fs::write(path, body) {
                        Ok(()) => Outcome::ok(json!({"path": path})),
                        Err(e) => Outcome::err_msg(format!("export failed: {e}")),
                    }
                }
            }
            "settings.import" => {
                let path = params.get("path").and_then(Value::as_str).unwrap_or("");
                match std::fs::read_to_string(path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                {
                    Some(data) => {
                        let (applied, rejected) = self.settings.import(&data);
                        let _ = self.settings.save();
                        Outcome::ok(json!({"applied": applied, "rejected": rejected}))
                    }
                    None => Outcome::err_msg(format!("cannot read {path}")),
                }
            }
            "sources.list" => {
                let root = spiderhead_sources::credentials::default_config_root(
                    &spiderhead_core::host::SystemHost,
                );
                Outcome::ok(crate::settings::sources_list(
                    &spiderhead_core::host::SystemHost,
                    &root,
                ))
            }
            "sources.credential" => {
                let provider = params.get("provider").and_then(Value::as_str).unwrap_or("");
                let component = params
                    .get("component")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let action = params.get("action").and_then(Value::as_str).unwrap_or("");
                let value = params.get("value").and_then(Value::as_str).unwrap_or("");
                let root = spiderhead_sources::credentials::default_config_root(
                    &spiderhead_core::host::SystemHost,
                );
                match crate::settings::credential_action(&root, provider, component, action, value)
                {
                    Ok(v) => Outcome::ok(v),
                    Err(e) => Outcome::err_msg(e),
                }
            }
            "sources.spend" => {
                let since = params.get("since").and_then(Value::as_str);
                Outcome::ok(spiderhead_sources::spend::report(
                    &spiderhead_core::host::SystemHost,
                    since,
                ))
            }
            "issues.severity" => {
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if !id.is_empty() && crate::ui::kit::Severity::parse(&id).is_none() {
                    return Outcome::err_msg(format!("unknown severity {id:?}"));
                }
                self.issues_severity = id.clone();
                Outcome::ok(json!({"severity": id}))
            }
            "issues.urls" => self.issues_urls(params),
            "search.options" => {
                if let Some(scope) = params.get("scope").and_then(Value::as_str) {
                    if crate::data::search::scope(scope).is_none() {
                        return Outcome::err_msg(format!(
                            "unknown scope {scope:?} (text, html, head, css, dom)"
                        ));
                    }
                    self.search.scope = scope.to_string();
                }
                if let Some(b) = params.get("case_sensitive").and_then(Value::as_bool) {
                    self.search.case_sensitive = b;
                }
                if let Some(b) = params.get("only_200").and_then(Value::as_bool) {
                    self.search.only_200 = b;
                }
                if let Some(s) = params.get("selector").and_then(Value::as_str) {
                    self.search.selector = s.to_string();
                }
                Outcome::ok(json!({
                    "scope": self.search.scope,
                    "case_sensitive": self.search.case_sensitive,
                    "only_200": self.search.only_200,
                    "selector": self.search.selector,
                }))
            }
            "search.run" => self.search_run(params),
            "search.select" => {
                let hit = params.get("hit").and_then(Value::as_i64).unwrap_or(0);
                let occurrence = params.get("match").and_then(Value::as_i64).unwrap_or(0);
                self.search.selected = hit.max(0) as usize;
                self.search.occurrence = occurrence.max(0) as usize;
                Outcome::ok(json!({
                    "hit": self.search.selected,
                    "match": self.search.occurrence,
                }))
            }
            "search.copy" => self.search_copy(),
            "compare.scans" => {
                let out = self.with_scan(|s| {
                    crate::data::compare::sibling_scans(&s.path).map_err(|e| e.message)
                });
                if let Ok(v) = &out.answer {
                    // First run preselects the two newest scans — canvas defaults.
                    if self.compare.before.is_none() && self.compare.after.is_none() {
                        if let Some(list) = v.get("scans").and_then(Value::as_array) {
                            if let Some(a) = list.first() {
                                self.compare.after =
                                    a.get("path").and_then(Value::as_str).map(str::to_string);
                            }
                            if let Some(b) = list.get(1).or_else(|| list.first()) {
                                self.compare.before =
                                    b.get("path").and_then(Value::as_str).map(str::to_string);
                            }
                        }
                    }
                }
                out
            }
            "compare.pick" => {
                let side = params.get("side").and_then(Value::as_str).unwrap_or("");
                let path = params.get("path").and_then(Value::as_str).unwrap_or("");
                if path.is_empty() || !["before", "after"].contains(&side) {
                    return Outcome::err_msg(
                        "compare.pick needs {\"side\": \"before\"|\"after\", \"path\": \"…\"}",
                    );
                }
                match side {
                    "before" => self.compare.before = Some(path.to_string()),
                    _ => self.compare.after = Some(path.to_string()),
                }
                self.compare.result = None;
                Outcome::ok(json!({
                    "side": side,
                    "before": self.compare.before,
                    "after": self.compare.after,
                }))
            }
            "compare.swap" => {
                std::mem::swap(&mut self.compare.before, &mut self.compare.after);
                self.compare.result = None;
                Outcome::ok(json!({
                    "before": self.compare.before,
                    "after": self.compare.after,
                }))
            }
            "compare.tab" => {
                let id = params.get("id").and_then(Value::as_str).unwrap_or("");
                if !crate::data::compare::TABS.contains(&id) {
                    return Outcome::err_msg(format!(
                        "unknown compare tab {id:?} ({})",
                        crate::data::compare::TABS.join(", ")
                    ));
                }
                self.compare.tab = id.to_string();
                Outcome::ok(json!({"tab": id}))
            }
            "compare.metric" => {
                let id = params.get("id").and_then(Value::as_str).unwrap_or("");
                if !crate::data::compare::METRICS.contains(&id) {
                    return Outcome::err_msg(format!(
                        "unknown compare metric {id:?} ({})",
                        crate::data::compare::METRICS.join(", ")
                    ));
                }
                self.compare.metric = id.to_string();
                Outcome::ok(json!({"metric": id}))
            }
            "compare.run" => self.compare_run(params),
            "compare.url" => {
                let url = params.get("url").and_then(Value::as_str).unwrap_or("");
                if url.is_empty() {
                    return Outcome::err_msg("compare.url needs {\"url\": \"…\"}");
                }
                let Some((b, a)) = self.compare.before.clone().zip(self.compare.after.clone())
                else {
                    return Outcome::err_msg("pick two scans first (compare.pick)");
                };
                let out = crate::data::compare::url_compare(
                    std::path::Path::new(&b),
                    std::path::Path::new(&a),
                    url,
                );
                match out {
                    Ok(v) => {
                        self.compare.url = url.to_string();
                        if params.get("open").and_then(Value::as_bool).unwrap_or(true) {
                            self.nav = Nav::CompareUrl;
                        }
                        self.compare.url_result = Some(v.clone());
                        Outcome::ok(v)
                    }
                    Err(e) => Outcome::err_msg(e),
                }
            }
            other => Outcome::err_msg(format!("unknown method: {other} (see commands.list)")),
        }
    }

    /// Side effects of a stored setting: theme and display apply at once,
    /// the rest stays written for the consumers that read it.
    fn apply_setting(&mut self, key: &str) -> Vec<Effect> {
        match key {
            "view.theme" => {
                let value = self.settings.get_str(key);
                let resolved = if value == "system" {
                    Some(if self.system_dark {
                        Theme::Dark
                    } else {
                        Theme::Light
                    })
                } else {
                    Theme::parse(&value)
                };
                match resolved {
                    Some(t) => {
                        self.theme = t;
                        vec![Effect::SetTheme(t)]
                    }
                    None => vec![],
                }
            }
            // Default product mode for the next start; the chooser still opens first.
            "shell.mode" => vec![],
            _ => vec![],
        }
    }

    /// Enter a product mode: keep the section if it belongs there, else go
    /// to the mode's home. `{"nav": "…"}` overrides the landing section.
    fn set_mode(&mut self, mode: Mode, params: &Value) -> Outcome {
        // `nav` on the chooser is stale — entering a product mode from it
        // always lands on the mode home; mode→mode keeps a shared section.
        let had_mode = self.mode.is_some();
        self.mode = Some(mode);
        // The crawler workspace is dense by default: the nav rides as an
        // icon rail until the user unfolds it (Cmd+B).
        if mode == Mode::Crawler && !had_mode {
            self.nav_collapsed = true;
        }
        if let Some(id) = params.get("nav").and_then(Value::as_str) {
            match Nav::parse(id) {
                Some(nav) => self.nav = nav,
                None => {
                    return Outcome::err_msg(format!("unknown section {id:?} for nav override"))
                }
            }
        } else if !had_mode || !mode.navs().contains(&self.nav) {
            self.nav = mode.home();
        }
        Outcome::ok(json!({"mode": mode.name(), "nav": self.nav.name()}))
    }

    fn scan_open(&mut self, params: &Value) -> Outcome {
        let path = params.get("path").and_then(Value::as_str).unwrap_or("");
        if path.is_empty() {
            return Outcome::err_msg("scan.open needs {\"path\": \"…\"}");
        }
        match ScanSession::open(Path::new(path)) {
            Ok(session) => {
                self.findings = Some(Findings::load(&session.path));
                self.table = TableState {
                    query: Query::base(),
                    ..TableState::default()
                };
                self.table.query.facets = json!("all");
                self.table.query.columns = crate::data::columns::UI_DEFAULT_COLUMNS
                    .iter()
                    .map(|c| c.to_string())
                    .collect();
                self.dashboard_selected = None;
                self.issues_severity = String::new();
                self.issue_page = None;
                self.search = SearchState::new();
                self.compare = CompareState::default();
                self.status = String::new();
                self.scan = Some(session);
                let mut q = self.table.query.clone();
                q.limit = 50;
                match self.scan.as_mut() {
                    Some(s) => match query::run(s, &q) {
                        Ok(page) => {
                            self.table.last = Some(page.clone());
                            Outcome::ok(json!({
                                "opened": true,
                                "path": path,
                                "source": s.source,
                                "total": page.get("total").cloned().unwrap_or(Value::Null),
                                "audit": s.audit_available(),
                            }))
                        }
                        Err(e) => Outcome::err(e),
                    },
                    None => Outcome::err_msg("scan state lost"),
                }
            }
            Err(e) => match e {
                crate::data::session::DataError::Query(qe) => Outcome::err(qe),
                other => Outcome::err_msg(other.to_string()),
            },
        }
    }

    /// `table.page` — run the table's current query at a new offset.
    fn table_page(&mut self, params: &Value) -> Outcome {
        let mut q = match Query::from_params(params, &self.table.query) {
            Ok(q) => q,
            Err(e) => return Outcome::err(e),
        };
        if q.limit == 0 {
            q.limit = 50;
        }
        let outcome = self.with_scan_mut(|s| {
            query::run(s, &q)
                .map_err(|e| json!({"error": e.message, "reason_code": e.reason_code}).to_string())
        });
        match outcome.answer {
            Ok(page) => {
                self.table.query = q;
                self.table.last = Some(page.clone());
                Outcome::ok(page)
            }
            Err(e) => Outcome::err_msg(e),
        }
    }

    /// `table.copy` — rows as TSV, paged until `EXPORT_ROW_CAP`. Without
    /// `selected` the whole filtered set goes out (header + rows); with
    /// `selected` only those `url_id`s, in the on-screen order, optionally
    /// without the header row (Ctrl+C copy-pastes into sheets either way).
    /// Header names stay the allow-list ids so the sheet round-trips.
    fn table_copy(&mut self, selected: Option<&Vec<Value>>, header: bool) -> Outcome {
        let Some(session) = self.scan.as_mut() else {
            return Outcome::err_msg("no scan is open");
        };
        let mut q = self.table.query.clone();
        q.facets = Value::Null;
        q.limit = crate::data::columns::MAX_LIMIT;
        if let Some(ids) = selected {
            // The selected rows only, screen order = current sort order.
            if ids.is_empty() {
                return Outcome::ok(json!({"tsv": "", "rows": 0, "filtered": 0, "capped": false}));
            }
            q.filters = vec![json!({"column": "url_id", "op": "in", "value": ids})];
            if ids.len() as i64 > crate::data::columns::MAX_IN_VALUES as i64 {
                return Outcome::err_msg(format!(
                    "at most {} rows can be copied at once",
                    crate::data::columns::MAX_IN_VALUES
                ));
            }
        }
        let cols: Vec<String> = if q.columns.is_empty() {
            crate::data::columns::DEFAULT_COLUMNS
                .iter()
                .map(|c| c.to_string())
                .collect()
        } else {
            q.columns.clone()
        };
        // Hidden columns never leak into the buffer (CP-07).
        let cols: Vec<String> = cols
            .into_iter()
            .filter(|c| c == "url_id" || !self.grid.layout.is_hidden(c))
            .collect();
        let mut tsv = if header {
            format!("{}\n", cols.join("\t"))
        } else {
            String::new()
        };
        let (mut offset, mut returned, mut filtered) = (0_i64, 0_i64, 0_i64);
        loop {
            q.offset = offset;
            let page = match query::run(session, &q) {
                Ok(p) => p,
                Err(e) => {
                    return Outcome::err_msg(
                        json!({"error": e.message, "reason_code": e.reason_code}).to_string(),
                    )
                }
            };
            if let Some(f) = page.get("filtered_total").and_then(Value::as_i64) {
                filtered = f;
            }
            let empty = Vec::new();
            let rows = page.get("rows").and_then(Value::as_array).unwrap_or(&empty);
            if rows.is_empty() {
                break;
            }
            for row in rows {
                let cells: Vec<String> = cols
                    .iter()
                    .map(|c| {
                        let cell = row.get(c).cloned().unwrap_or(Value::Null);
                        tsv_cell(&cell)
                    })
                    .collect();
                tsv.push_str(&cells.join("\t"));
                tsv.push('\n');
            }
            returned += rows.len() as i64;
            offset += rows.len() as i64;
            if returned >= EXPORT_ROW_CAP || returned >= filtered {
                break;
            }
        }
        Outcome::ok(json!({
            "tsv": tsv,
            "rows": returned,
            "filtered": filtered,
            "capped": filtered > returned,
        }))
    }

    /// `details.copy` — the retained URL text of `url_id`, for the clipboard.
    fn details_copy(&mut self, params: &Value) -> Outcome {
        let url_id = params.get("url_id").and_then(Value::as_i64);
        let Some(url_id) = url_id else {
            return Outcome::err_msg("details.copy needs {\"url_id\": n}");
        };
        self.with_scan(|s| {
            let url = s
                .con
                .query_row("SELECT url FROM urls WHERE url_id=?1", [url_id], |r| {
                    r.get::<_, String>(0)
                })
                .map_err(|e| format!("cannot read scan: {e}"))?;
            Ok(json!({"text": url, "url_id": url_id}))
        })
    }

    /// `table.sort`/`table.filter` — update the query and re-run the first page.
    fn table_update(&mut self, params: &Value) -> Outcome {
        let mut params = params.clone();
        if let Some(obj) = params.as_object_mut() {
            // `table.sort` names the column "column"; the query model calls it "sort".
            if let Some(col) = obj.shift_remove("column") {
                obj.insert("sort".into(), col);
            }
            obj.insert("offset".into(), json!(0));
        }
        if let Some(obj) = params.as_object_mut() {
            if !obj.contains_key("limit") {
                obj.insert("limit".into(), json!(50));
            }
        }
        self.table_page(&params)
    }

    /// `simple.check` — look one address up in the open scan. Exact match
    /// first (with the trailing slash toggled), then substring suggestions.
    fn simple_check(&mut self, params: &Value) -> Outcome {
        let url = params
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        if url.is_empty() {
            return Outcome::err_msg("simple.check needs {\"url\": \"…\"}");
        }
        if self.scan.is_none() {
            return Outcome::ok(json!({"state": "no_scan", "url": url}));
        }
        self.with_scan_mut(|s| {
            let mut q = Query::base();
            q.facets = Value::Null;
            q.limit = 5;
            q.columns = SIMPLE_COLUMNS.iter().map(|c| c.to_string()).collect();
            let alt = match url.strip_suffix('/') {
                Some(rest) => rest.to_string(),
                None => format!("{url}/"),
            };
            for candidate in [&url, &alt] {
                q.filters = vec![json!({"column": "url", "op": "eq", "value": candidate})];
                let page = query::run(s, &q).map_err(|e| e.message)?;
                if let Some(row) = page
                    .get("rows")
                    .and_then(Value::as_array)
                    .and_then(|r| r.first())
                {
                    return Ok(json!({"state": "found", "url": url, "row": row}));
                }
            }
            q.filters = vec![json!({"column": "url", "op": "contains", "value": url})];
            let page = query::run(s, &q).map_err(|e| e.message)?;
            let rows = page
                .get("rows")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if rows.is_empty() {
                Ok(json!({"state": "not_found", "url": url}))
            } else {
                Ok(json!({"state": "similar", "url": url, "rows": rows}))
            }
        })
    }

    /// `dashboard.filter` — apply an issue facet and open the table.
    fn dashboard_filter(&mut self, params: &Value) -> Outcome {
        let mut merged = json!({
            "offset": 0,
            "limit": 50,
            "filters": [],
            "preset": Value::Null,
        });
        for key in ["issue_check", "issue_severity"] {
            if let Some(v) = params.get(key) {
                merged[key] = v.clone();
            }
        }
        let _ = validate_filters(&json!([]));
        self.nav = Nav::Urls;
        self.table_page(&merged)
    }

    /// `issues.urls` — a page of the URLs the selected check fired on.
    fn issues_urls(&mut self, params: &Value) -> Outcome {
        let check = params
            .get("check")
            .and_then(Value::as_str)
            .or(self.dashboard_selected.as_deref())
            .unwrap_or("")
            .to_string();
        if check.is_empty() {
            return Outcome::err_msg("issues.urls needs {\"check\": \"…\"} (or dashboard.select)");
        }
        let offset = params.get("offset").and_then(Value::as_i64).unwrap_or(0);
        let limit = params.get("limit").and_then(Value::as_i64).unwrap_or(100);
        if let Some((c, o, page)) = &self.issue_page {
            if *c == check && *o == offset {
                return Outcome::ok(page.clone());
            }
        }
        let out = self.with_scan(|s| {
            crate::data::issue_urls::urls_for_check(s, &check, offset, limit)
                .map_err(|e| json!({"error": e.message, "reason_code": e.reason_code}).to_string())
        });
        if let Ok(v) = &out.answer {
            self.issue_page = Some((check, offset, v.clone()));
        }
        out
    }

    /// `search.run` — literal content search with the stored options.
    fn search_run(&mut self, params: &Value) -> Outcome {
        let query = params
            .get("query")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| self.search.query.clone());
        if query.trim().is_empty() {
            return Outcome::err_msg("search.run needs {\"query\": \"…\"}");
        }
        let Some(scope) = crate::data::search::scope(&self.search.scope) else {
            return Outcome::err_msg(format!("unknown scope {:?}", self.search.scope));
        };
        let selector = (scope.id == "css").then(|| self.search.selector.clone());
        let only_200 = self.search.only_200;
        let case = self.search.case_sensitive;
        let out = self.with_scan(|s| {
            crate::data::search::run(
                &s.path,
                query.trim(),
                scope,
                selector.as_deref(),
                case,
                only_200,
            )
            .map_err(|e| json!({"error": e.message, "reason_code": e.reason_code}).to_string())
        });
        if let Ok(v) = &out.answer {
            self.search.query = query;
            self.search.selected = 0;
            self.search.occurrence = 0;
            self.search.result = Some(v.clone());
        }
        out
    }

    /// `search.copy` — the hit list as TSV (`url`, matches, snippet).
    fn search_copy(&mut self) -> Outcome {
        let Some(v) = self.search.result.clone() else {
            return Outcome::err_msg("nothing to copy — run search.run first");
        };
        let mut tsv = String::from("url\tmatches\tsnippet\n");
        for hit in v
            .get("hits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let cells = [
                hit.get("url"),
                hit.get("occurrence_count"),
                hit.get("snippet"),
            ]
            .into_iter()
            .map(|c| tsv_cell(c.unwrap_or(&Value::Null)))
            .collect::<Vec<_>>();
            tsv.push_str(&cells.join("\t"));
            tsv.push('\n');
        }
        let rows = v.get("hits_returned").and_then(Value::as_i64).unwrap_or(0);
        Outcome::ok(json!({"tsv": tsv, "rows": rows}))
    }

    /// `compare.run` — summary plus a page of the active tab.
    fn compare_run(&mut self, params: &Value) -> Outcome {
        let Some((before, after)) = self.compare.before.clone().zip(self.compare.after.clone())
        else {
            return Outcome::err_msg("pick two scans first (compare.pick/compare.scans)");
        };
        if before == after {
            return Outcome::err_msg("pick two different scans");
        }
        let tab = params
            .get("tab")
            .and_then(Value::as_str)
            .unwrap_or(&self.compare.tab)
            .to_string();
        if !crate::data::compare::TABS.contains(&tab.as_str()) {
            return Outcome::err_msg(format!("unknown compare tab {tab:?}"));
        }
        self.compare.tab = tab.clone();
        let metric = params
            .get("metric")
            .and_then(Value::as_str)
            .unwrap_or(&self.compare.metric)
            .to_string();
        self.compare.metric = metric.clone();
        let offset = params.get("offset").and_then(Value::as_i64).unwrap_or(0);
        let limit = params.get("limit").and_then(Value::as_i64).unwrap_or(50);
        match crate::data::compare::Pair::open(
            std::path::Path::new(&before),
            std::path::Path::new(&after),
        ) {
            Ok(pair) => {
                let summary = pair.summary();
                match pair.page(&tab, &metric, offset, limit) {
                    Ok(page) => {
                        let mut out = summary;
                        out["page"] = page;
                        self.compare.result = Some(out.clone());
                        Outcome::ok(out)
                    }
                    Err(e) => Outcome::err_msg(e),
                }
            }
            Err(e) => Outcome::err_msg(e),
        }
    }

    fn with_scan(&self, f: impl FnOnce(&ScanSession) -> Result<Value, String>) -> Outcome {
        match self.scan.as_ref() {
            Some(s) => match f(s) {
                Ok(v) => Outcome::ok(v),
                Err(e) => Outcome::err_msg(e),
            },
            None => Outcome::err_msg("no scan is open"),
        }
    }

    fn with_scan_mut(
        &mut self,
        f: impl FnOnce(&mut ScanSession) -> Result<Value, String>,
    ) -> Outcome {
        match self.scan.as_mut() {
            Some(s) => match f(s) {
                Ok(v) => Outcome::ok(v),
                Err(e) => Outcome::err_msg(e),
            },
            None => Outcome::err_msg("no scan is open"),
        }
    }
}

impl Default for AppModel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn raw(site: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../oracle/raw-scans")
            .join(site)
            .join("scan.sqlite")
    }

    fn app_with_basic() -> AppModel {
        let mut app = AppModel::with_settings(crate::settings::AppSettings::in_memory());
        let out = app.dispatch(
            "scan.open",
            &json!({"path": raw("basic").display().to_string()}),
        );
        assert!(out.answer.is_ok(), "scan.open: {:?}", out.answer);
        app
    }

    #[test]
    fn registry_gates_scan_commands_until_open() {
        let app = AppModel::new();
        let commands = app.commands();
        let page = commands.iter().find(|c| c.id == "table.page").unwrap();
        assert!(!page.enabled);
        assert_eq!(page.disabled_reason.as_deref(), Some("no scan is open"));
        let app = app_with_basic();
        let commands = app.commands();
        assert!(
            commands
                .iter()
                .find(|c| c.id == "table.page")
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn theme_and_display_switch() {
        let mut app = AppModel::new();
        let out = app.dispatch("ui.theme", &json!({"mode": "light"}));
        assert_eq!(out.answer.unwrap()["mode"], "light");
        assert_eq!(out.effects, vec![Effect::SetTheme(Theme::Light)]);
        assert_eq!(app.mode, None);
        let out = app.dispatch("ui.mode", &json!({"mode": "agent"}));
        assert_eq!(out.answer.unwrap()["mode"], "agent");
        assert_eq!(app.mode, Some(Mode::Agent));
        assert_eq!(app.nav, Nav::Work);
        let out = app.dispatch("ui.mode", &json!({"mode": "bogus"}));
        assert!(out.answer.is_err());
        let out = app.dispatch("ui.theme", &json!({"mode": "sepia"}));
        assert!(out.answer.is_err());
    }

    #[test]
    fn mode_switch_lands_on_mode_home() {
        let mut app = AppModel::new();
        // From the chooser a mode lands on its home.
        app.dispatch("ui.mode", &json!({"mode": "crawler"}));
        assert_eq!(app.nav, Nav::Crawler);
        // Mode→mode keeps a section the new mode also owns.
        app.dispatch("ui.navigate", &json!({"id": "urls"}));
        app.dispatch("ui.mode", &json!({"mode": "agent"}));
        assert_eq!(app.nav, Nav::Urls);
        // Logs mode owns none of those sections → its home.
        app.dispatch("ui.mode", &json!({"mode": "logs"}));
        assert_eq!(app.nav, Nav::Logs);
        // `nav` param overrides the landing section.
        app.dispatch("ui.mode", &json!({"mode": "agent", "nav": "inbox"}));
        assert_eq!(app.mode, Some(Mode::Agent));
        assert_eq!(app.nav, Nav::Inbox);
        // Back to the chooser.
        app.dispatch("ui.mode", &json!({"mode": "start"}));
        assert_eq!(app.mode, None);
    }

    #[test]
    fn scan_open_then_page_matches_reference_shape() {
        let mut app = app_with_basic();
        let out = app.dispatch("table.page", &json!({"limit": 5}));
        let page = out.answer.unwrap();
        assert_eq!(page["format"], "spiderhead.scan-url-query.v1");
        assert_eq!(page["total"], 10);
        assert_eq!(page["filtered_total"], 10);
        assert_eq!(page["rows"].as_array().unwrap().len(), 5);
        assert_eq!(page["has_more"], true);
        assert_eq!(page["next_offset"], 5);
        assert_eq!(page["sort"]["column"], "page_ordinal");
        assert_eq!(page["sort"]["mode"], "materialized");
        let first = &page["rows"][0];
        assert_eq!(first["url"], "http://127.0.0.1:18701/");
        assert_eq!(first["status_code"], 200);
        assert_eq!(first["title"], "Basic site home");
        assert_eq!(first["indexable"], true);
        // Facet counters must equal the reference baseline on the same file:
        // all:10, 2xx:9, 4xx:1, title_short:10, canonical_missing:10,
        // directives_no(index|follow):1, internal/with_inlinks:10, outlinks:9,
        // title_duplicate:2, sitemap/resource facets unavailable (null).
        let f = &page["facets"];
        for (k, want) in [
            ("all", 10),
            ("status_2xx", 9),
            ("status_3xx", 0),
            ("status_4xx", 1),
            ("status_5xx", 0),
            ("title_short", 10),
            ("title_duplicate", 2),
            ("title_missing", 0),
            ("title_long", 0),
            ("canonical_missing", 10),
            ("canonical_other", 0),
            ("canonical_self", 0),
            ("directives_noindex", 1),
            ("directives_nofollow", 1),
            ("internal", 10),
            ("external", 0),
            ("with_inlinks", 10),
            ("with_outlinks", 9),
        ] {
            assert_eq!(f[k], json!(want), "facet {k}");
        }
        for k in [
            "in_sitemap",
            "with_images",
            "with_resources",
            "with_scripts",
            "with_stylesheets",
        ] {
            assert!(f[k].is_null(), "facet {k} must be unavailable");
        }
    }

    #[test]
    fn sort_filter_and_preset() {
        let mut app = app_with_basic();
        // 4xx preset → one row (the 404 page).
        let out = app.dispatch("table.filter", &json!({"preset": "status_4xx"}));
        let page = out.answer.unwrap();
        assert_eq!(page["filtered_total"], 1);
        assert_eq!(page["rows"][0]["status_code"], 404);
        // Sort descending by status_code: the 404 first.
        let out = app.dispatch(
            "table.sort",
            &json!({"column": "status_code", "direction": "desc", "filters": []}),
        );
        let page = out.answer.unwrap();
        assert_eq!(page["rows"][0]["status_code"], 404);
        // Text filter on url (contains → urls join path). The 4xx preset from
        // above survives unless explicitly reset — match the oracle: send null.
        let out = app.dispatch(
            "table.filter",
            &json!({"filters": [{"column": "url", "op": "contains", "value": "dup"}],
                     "preset": null}),
        );
        let page = out.answer.unwrap();
        assert_eq!(page["filtered_total"], 2);
        // Unknown column → invalid error surfaced.
        let out = app.dispatch("table.sort", &json!({"column": "bogus"}));
        let err = out.answer.unwrap_err();
        assert!(err.contains("unknown_column"), "{err}");
    }

    #[test]
    fn issue_filter_and_dashboard_numbers() {
        let mut app = app_with_basic();
        let out = app.dispatch("dashboard.inspect", &json!({}));
        let dash = out.answer.unwrap();
        assert_eq!(dash["state"], "available");
        assert_eq!(dash["issues_total"], 154);
        assert_eq!(dash["by_severity"]["critical"], 2);
        assert_eq!(dash["by_severity"]["warning"], 43);
        // Facet → issue filter: HTTP_URL fires on 9 pages (oracle audit.v2).
        let out = app.dispatch("dashboard.filter", &json!({"issue_check": "HTTP_URL"}));
        let page = out.answer.unwrap();
        assert_eq!(page["filtered_total"], 9, "{page}");
        // Severity-only filter: 2 critical issues — both on the 404 page, so
        // the filtered URL count is 1 (findings count ≠ page count).
        let out = app.dispatch(
            "table.filter",
            &json!({"issue_check": null, "issue_severity": "critical"}),
        );
        let page = out.answer.unwrap();
        assert_eq!(page["filtered_total"], 1, "{page}");
    }

    #[test]
    fn details_tabs_return_payloads() {
        let mut app = app_with_basic();
        let out = app.dispatch("details.load", &json!({"url_id": 1, "tab": "summary"}));
        let sum = out.answer.unwrap();
        assert_eq!(sum["url"], "http://127.0.0.1:18701/");
        assert_eq!(sum["title"], "Basic site home");
        let out = app.dispatch("details.load", &json!({"url_id": 1, "tab": "links_out"}));
        let links = out.answer.unwrap();
        assert!(links["total"].as_i64().unwrap() > 0);
        let out = app.dispatch("details.load", &json!({"url_id": 1, "tab": "links_in"}));
        assert!(out.answer.is_ok());
        let out = app.dispatch("details.load", &json!({"url_id": 1, "tab": "response"}));
        let resp = out.answer.unwrap();
        assert_eq!(resp["state"], "available");
        assert_eq!(resp["status_code"], 200);
        let out = app.dispatch("details.load", &json!({"url_id": 1, "tab": "body"}));
        let body = out.answer.unwrap();
        assert_eq!(body["state"], "available");
        assert!(body["text"].as_str().unwrap().contains("Basic"));
    }

    #[test]
    fn simple_check_finds_and_reports_misses() {
        let mut app = app_with_basic();
        let out = app.dispatch("simple.check", &json!({"url": "http://127.0.0.1:18701/"}));
        let v = out.answer.unwrap();
        assert_eq!(v["state"], "found");
        assert_eq!(v["row"]["status_code"], 200);
        // Trailing slash is toggled before giving up.
        let out = app.dispatch("simple.check", &json!({"url": "http://127.0.0.1:18701"}));
        assert_eq!(out.answer.unwrap()["state"], "found");
        let out = app.dispatch("simple.check", &json!({"url": "http://absent.example/"}));
        assert_eq!(out.answer.unwrap()["state"], "not_found");
        let mut app = AppModel::new();
        let out = app.dispatch("simple.check", &json!({"url": "http://a/"}));
        assert_eq!(out.answer.unwrap()["state"], "no_scan");
        assert!(app.dispatch("simple.check", &json!({})).answer.is_err());
    }

    #[test]
    fn windows_open_close_and_inspect_lists_them() {
        let mut app = AppModel::new();
        let out = app.dispatch("ui.window", &json!({"id": "settings"}));
        assert_eq!(out.answer.unwrap()["open"], true);
        assert!(app.windows.contains("settings"));
        let out = app.dispatch("ui.inspect", &json!({}));
        assert!(out.answer.unwrap()["dialogs"]
            .as_array()
            .unwrap()
            .contains(&json!("settings")));
        app.dispatch("ui.window", &json!({"id": "settings", "open": false}));
        assert!(app.windows.is_empty());
        assert!(app
            .dispatch("ui.window", &json!({"id": "bogus"}))
            .answer
            .is_err());
    }

    #[test]
    fn details_counts_and_copy_match_oracle() {
        let mut app = app_with_basic();
        // url 1 (home): 10 outbound links, 8 inbound, 4 request + 4 response
        // headers, empty redirect chain, one structured-evidence item.
        let out = app.dispatch("details.load", &json!({"url_id": 1, "tab": "counts"}));
        let c = out.answer.unwrap();
        assert_eq!(c["links_out"], 10);
        assert_eq!(c["links_in"], 8);
        assert_eq!(c["headers"], 8);
        assert_eq!(c["redirects"], 0);
        assert_eq!(c["structured"], 1);
        // details.copy returns the retained URL text verbatim.
        let out = app.dispatch("details.copy", &json!({"url_id": 1}));
        let v = out.answer.unwrap();
        assert_eq!(v["text"], "http://127.0.0.1:18701/");
        assert!(app.dispatch("details.copy", &json!({})).answer.is_err());
    }

    #[test]
    fn details_canvas_tabs_and_aliases() {
        let mut app = app_with_basic();
        // `headers`/`html` are the canvas names of `response`/`body`.
        for (tab, alias) in [("headers", "response"), ("html", "body")] {
            let v = app
                .dispatch("details.load", &json!({"url_id": 1, "tab": tab}))
                .answer
                .unwrap();
            let w = app
                .dispatch("details.load", &json!({"url_id": 1, "tab": alias}))
                .answer
                .unwrap();
            assert_eq!(v["state"], "available", "tab {tab}");
            assert_eq!(v["state"], w["state"], "{tab} vs {alias}");
        }
        // Links rows carry the attribute evidence (rel/target/raw_href).
        let out = app.dispatch("details.load", &json!({"url_id": 6, "tab": "links_in"}));
        let links = out.answer.unwrap();
        assert_eq!(links["total"], 1);
        let item = &links["items"][0];
        assert_eq!(item["url"], "http://127.0.0.1:18701/");
        assert!(item.get("rel").is_some() && item.get("target").is_some());
        // Redirects: direct URL → empty chain, canonical fields still present.
        let out = app.dispatch("details.load", &json!({"url_id": 1, "tab": "redirects"}));
        let r = out.answer.unwrap();
        assert_eq!(r["state"], "available");
        assert_eq!(r["hops"], 0);
        assert!(r["chain"].as_array().unwrap().is_empty());
        // Resources: scan.v1 has no measured refs; images live in `images`.
        let out = app.dispatch("details.load", &json!({"url_id": 6, "tab": "resources"}));
        let r = out.answer.unwrap();
        assert_eq!(r["total"], 0);
        assert_eq!(r["images"]["total"], 4);
        assert_eq!(r["images"]["missing_alt"], 1);
        // Structured evidence: one item per page; basic has no markup.
        let out = app.dispatch("details.load", &json!({"url_id": 1, "tab": "structured"}));
        let s = out.answer.unwrap();
        assert_eq!(s["items"].as_array().unwrap().len(), 1);
        assert_eq!(s["items"][0]["payload"]["state"], "absent");
        // History: the scan file's own directory has exactly this scan.
        let out = app.dispatch("details.load", &json!({"url_id": 1, "tab": "history"}));
        let h = out.answer.unwrap();
        assert_eq!(h["ok"], true);
        assert_eq!(h["scan_total"], 1);
        assert_eq!(h["scans"][0]["state"], "present");
        assert_eq!(h["scans"][0]["status_code"], 200);
        // Unknown tab stays a caller error.
        assert!(app
            .dispatch("details.load", &json!({"url_id": 1, "tab": "bogus"}))
            .answer
            .is_err());
    }

    #[test]
    fn table_copy_returns_tsv_of_the_filtered_set() {
        let mut app = app_with_basic();
        let out = app.dispatch("table.copy", &json!({}));
        let v = out.answer.unwrap();
        assert_eq!(v["rows"], 10);
        assert_eq!(v["filtered"], 10);
        assert_eq!(v["capped"], false);
        let tsv = v["tsv"].as_str().unwrap();
        let lines: Vec<&str> = tsv.lines().collect();
        assert_eq!(lines.len(), 11);
        assert!(lines[0].starts_with("url_id\turl"));
        assert!(lines[1].contains("http://127.0.0.1:18701/"));
        // A filter narrows the export too.
        app.dispatch("table.filter", &json!({"preset": "status_4xx"}));
        let out = app.dispatch("table.copy", &json!({}));
        let v = out.answer.unwrap();
        assert_eq!(v["rows"], 1);
        assert_eq!(v["filtered"], 1);
    }

    #[test]
    fn details_toggle_and_selection_history() {
        let mut app = app_with_basic();
        assert!(app.details_open);
        let out = app.dispatch("details.toggle", &json!({"open": false}));
        assert_eq!(out.answer.unwrap()["open"], false);
        assert!(!app.details_open);
        let out = app.dispatch("details.toggle", &json!({}));
        assert_eq!(out.answer.unwrap()["open"], true);
    }

    #[test]
    fn commands_cover_the_whole_ui_surface() {
        let app = app_with_basic();
        for id in [
            "scan.open",
            "scan.close",
            "scan.info",
            "table.page",
            "table.sort",
            "table.filter",
            "table.select",
            "table.copy",
            "details.load",
            "details.copy",
            "details.toggle",
            "dashboard.inspect",
            "dashboard.filter",
            "dashboard.select",
            "ui.sidebar",
            "ui.theme",
            "ui.mode",
            "ui.navigate",
            "ui.window",
            "ui.find",
            "ui.screenshot",
            "ui.inspect",
            "window.list",
            "window.minimize",
            "window.fullscreen",
            "window.resize",
            "edit.clipboard",
            "commands.list",
            "app.quit",
            "simple.check",
            "issues.severity",
            "issues.urls",
            "search.options",
            "search.run",
            "search.select",
            "search.copy",
            "compare.scans",
            "compare.pick",
            "compare.swap",
            "compare.tab",
            "compare.metric",
            "compare.run",
            "compare.url",
        ] {
            assert!(
                app.commands().iter().any(|c| c.id == id),
                "registry misses {id}"
            );
        }
    }

    #[test]
    fn issues_urls_paginate_and_report_unmeasured() {
        let mut app = app_with_basic();
        let out = app.dispatch("dashboard.select", &json!({"check": "http_url"}));
        assert_eq!(out.answer.unwrap()["selected"], "HTTP_URL");
        let out = app.dispatch("issues.urls", &json!({}));
        let page = out.answer.unwrap();
        assert_eq!(page["state"], "available");
        assert_eq!(page["total"], 9, "{page}");
        assert_eq!(page["rows"][0]["url"], "http://127.0.0.1:18701/");
        let out = app.dispatch(
            "issues.urls",
            &json!({"check": "HTTP_URL", "offset": 7, "limit": 5}),
        );
        let page = out.answer.unwrap();
        assert_eq!(page["rows"].as_array().unwrap().len(), 2);
        assert_eq!(page["has_more"], false);
        // A check the saved audit never measured is a state, not an error.
        let out = app.dispatch("issues.urls", &json!({"check": "NO_SUCH_CHECK"}));
        assert_eq!(out.answer.unwrap()["state"], "unmeasured");
        // Cached (check, offset) answers repeat without another audit pass.
        let again = app.dispatch("issues.urls", &json!({"check": "HTTP_URL", "offset": 7}));
        assert_eq!(again.answer.unwrap()["rows"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn issues_severity_validates_ids() {
        let mut app = app_with_basic();
        let out = app.dispatch("issues.severity", &json!({"id": "critical"}));
        assert_eq!(out.answer.unwrap()["severity"], "critical");
        assert_eq!(app.issues_severity, "critical");
        let out = app.dispatch("issues.severity", &json!({"id": "bogus"}));
        assert!(out.answer.is_err());
        let out = app.dispatch("issues.severity", &json!({"id": ""}));
        assert_eq!(out.answer.unwrap()["severity"], "");
        assert_eq!(app.issues_severity, "");
    }

    #[test]
    fn search_run_hits_copy_and_unavailable_scope() {
        let mut app = app_with_basic();
        let out = app.dispatch("search.options", &json!({"scope": "text"}));
        assert_eq!(out.answer.unwrap()["scope"], "text");
        let out = app.dispatch("search.options", &json!({"scope": "bogus"}));
        assert!(out.answer.is_err());
        let out = app.dispatch("search.run", &json!({"query": "Basic"}));
        let v = out.answer.unwrap();
        assert_eq!(v["state"], "complete", "{v}");
        assert!(v["hits_total"].as_i64().unwrap() >= 1, "{v}");
        let out = app.dispatch("search.select", &json!({"hit": 0, "match": 0}));
        assert_eq!(out.answer.unwrap()["hit"], 0);
        let out = app.dispatch("search.copy", &json!({}));
        let tsv = out.answer.unwrap()["tsv"].as_str().unwrap().to_string();
        assert!(tsv.starts_with("url\tmatches\tsnippet\n"), "{tsv}");
        assert!(tsv.contains("http://"), "{tsv}");
        // The rendered-DOM scope is honest unavailability, never a panic.
        app.dispatch("search.options", &json!({"scope": "dom"}));
        let out = app.dispatch("search.run", &json!({"query": "x"}));
        assert_ne!(out.answer.unwrap()["state"], "complete");
    }

    #[test]
    fn compare_commands_validate_and_gate() {
        let mut app = app_with_basic();
        let out = app.dispatch("compare.scans", &json!({}));
        assert_eq!(out.answer.unwrap()["state"], "available");
        // Unknown ids and missing picks are structured errors, not panics.
        assert!(app
            .dispatch("compare.tab", &json!({"id": "bogus"}))
            .answer
            .is_err());
        assert!(app
            .dispatch("compare.metric", &json!({"id": "bogus"}))
            .answer
            .is_err());
        // One sibling scan gets preselected on both sides — same-path runs
        // stay errors, while a URL in neither scan is the honest "absent".
        assert!(app.dispatch("compare.run", &json!({})).answer.is_err());
        let out = app.dispatch("compare.url", &json!({"url": "http://x/", "open": false}));
        assert_eq!(out.answer.unwrap()["state"], "absent");
        app.dispatch("compare.tab", &json!({"id": "better"}));
        app.dispatch("compare.metric", &json!({"id": "response"}));
        assert_eq!(app.compare.tab, "better");
        assert_eq!(app.compare.metric, "response");
        app.compare.before = Some("a".into());
        app.compare.after = Some("b".into());
        app.dispatch("compare.swap", &json!({}));
        assert_eq!(app.compare.before.as_deref(), Some("b"));
        assert_eq!(app.compare.after.as_deref(), Some("a"));
        let out = app.dispatch("ui.navigate", &json!({"id": "compare_url"}));
        assert_eq!(out.answer.unwrap()["nav"], "compare_url");
        assert_eq!(app.nav, Nav::CompareUrl);
    }

    #[test]
    fn column_hide_show_and_reset_follow_the_layout() {
        let mut app = app_with_basic();
        // The default set, minus row identity.
        let base = grid_columns_wanted_of(&app);
        assert!(base.contains(&"url".to_string()));
        // Hide: the column leaves the query, sorting by it drops (SO-10).
        app.dispatch("table.sort", &json!({"column": "h1", "direction": "asc"}));
        let out = app.dispatch("table.column.hide", &json!({"column": "h1"}));
        assert_eq!(out.answer.unwrap()["visible"], false);
        assert!(!grid_columns_wanted_of(&app).contains(&"h1".to_string()));
        assert_eq!(
            app.table.query.sort, None,
            "sorting by a hidden column resets"
        );
        // A hidden column is not re-hidable and comes back on show (CL-07).
        assert!(app
            .dispatch("table.column.hide", &json!({"column": "h1"}))
            .answer
            .is_ok());
        app.dispatch("table.column.show", &json!({"column": "h1"}));
        assert!(grid_columns_wanted_of(&app).contains(&"h1".to_string()));
        // Reset returns the default set (CL-08).
        app.dispatch("table.columns.reset", &json!({}));
        assert_eq!(grid_columns_wanted_of(&app), base);
        assert_eq!(app.grid.zoom.percent(), 100.0);
        // Unknown names are honest errors.
        assert!(app
            .dispatch("table.column.hide", &json!({"column": "nope"}))
            .answer
            .is_err());
    }

    fn grid_columns_wanted_of(app: &AppModel) -> Vec<String> {
        app.grid_columns_wanted()[1..].to_vec()
    }

    #[test]
    fn column_move_and_pin_reorder_and_freeze() {
        let mut app = app_with_basic();
        // Move title before url (CL-02 through the control channel).
        let moved = app.dispatch(
            "table.column.move",
            &json!({"column": "title", "before": "url"}),
        );
        assert!(moved.answer.is_ok(), "{:?}", moved.answer);
        let want = grid_columns_wanted_of(&app);
        assert_eq!(want[0], "title");
        assert_eq!(want[1], "url");
        // The live query keeps the columns it can serve on this scan (this
        // fixture has no sitemap membership, so a wider set is refused);
        // the display order is the layout's business either way.
        let before = app.table.query.columns.clone();
        assert!(!before.is_empty());
        assert!(before.contains(&"title".to_string()));
        // Freeze the leading columns up to url (CL-20).
        let out = app.dispatch("table.column.pin", &json!({"upto": "url"}));
        assert!(out.answer.is_ok());
        assert!(app.grid.layout.frozen() >= 1);
        // An invisible pin target is an error.
        assert!(app
            .dispatch("table.column.pin", &json!({"upto": "nope"}))
            .answer
            .is_err());
        // Unfreeze.
        app.dispatch("table.column.pin", &json!({"upto": null}));
        assert_eq!(app.grid.layout.frozen(), 0);
    }

    #[test]
    fn zoom_walks_the_ladder_and_scroll_to_moves_the_view() {
        let mut app = app_with_basic();
        let out = app.dispatch("table.zoom", &json!({"step": "in"}));
        assert_eq!(out.answer.unwrap()["percent"], 120.0);
        app.dispatch("table.zoom", &json!({"percent": 85.0}));
        assert_eq!(app.grid.zoom.percent(), 85.0);
        // Out-of-ladder values snap.
        app.dispatch("table.zoom", &json!({"percent": 119.0}));
        assert_eq!(app.grid.zoom.percent(), 120.0);
        let out = app.dispatch("ui.scroll_to", &json!({"row": 7}));
        assert_eq!(out.answer.unwrap()["row"], 7);
        assert!(app.grid.scroll_y > 0.0);
        assert!(app
            .dispatch("ui.scroll_to", &json!({"row": -1}))
            .answer
            .is_err());
    }

    #[test]
    fn column_chooser_dialog_opens_and_closes() {
        let mut app = app_with_basic();
        app.dispatch("table.columns.configure", &json!({"open": true}));
        assert!(app.windows.contains("columns"));
        app.dispatch("table.columns.configure", &json!({"open": false}));
        assert!(!app.windows.contains("columns"));
    }

    #[test]
    fn filter_clears_the_selection_sort_keeps_it() {
        let mut app = app_with_basic();
        // Two rows selected: one click, one ctrl-click.
        app.dispatch("table.select", &json!({"url_id": 1}));
        app.grid.selection.ctrl_click("3");
        assert_eq!(app.grid.selection.len_total(10), 2);
        // Sorting keeps the same rows highlighted (SO-05).
        app.dispatch("table.sort", &json!({"column": "url", "direction": "desc"}));
        assert_eq!(app.grid.selection.len_total(10), 2);
        assert!(app.grid.selection.contains("1") && app.grid.selection.contains("3"));
        // A filter reshapes the set: the selection starts over (FL-02).
        app.dispatch("table.filter", &json!({"preset": "status_4xx"}));
        assert!(app.grid.selection.is_empty());
        assert_eq!(app.table.selected, None);
    }

    #[test]
    fn ctrl_a_marks_all_and_copy_selected_returns_just_those_rows() {
        let mut app = app_with_basic();
        app.grid.selection.set_all(vec!["1".into(), "3".into()]);
        let out = app.dispatch(
            "table.copy",
            &json!({"selected": [1, 3], "header": false, "columns": ["url"]}),
        );
        let v = out.answer.unwrap();
        assert_eq!(v["rows"], 2);
        let tsv = v["tsv"].as_str().unwrap();
        assert_eq!(tsv.lines().count(), 2, "no header row for Ctrl+C");
        // The empty selection copies nothing.
        let out = app.dispatch("table.copy", &json!({"selected": [], "header": false}));
        assert_eq!(out.answer.unwrap()["rows"], 0);
    }

    #[test]
    fn ui_state_snapshot_reports_the_table() {
        let mut app = app_with_basic();
        let out = app.dispatch("ui.inspect", &json!({}));
        let v = out.answer.unwrap();
        assert_eq!(v["table"]["rows"]["count"], 10);
        assert_eq!(v["table"]["empty"], false);
        assert!(v["table"]["columns"].as_array().unwrap().len() > 1);
        app.grid.selection.set_all(vec!["1".into()]);
        let out = app.dispatch("ui.inspect", &json!({}));
        assert_eq!(out.answer.unwrap()["table"]["selection"]["count"], 1);
    }
}
