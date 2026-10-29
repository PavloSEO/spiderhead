//! Screens of the three product modes (Краулер, Анализ логов, Агентский
//! режим) plus the start-screen chooser; one app, one shell, one registry.

mod agent;
mod crawler;
mod log_analyzer;
pub mod start;

use eframe::egui;
use serde_json::Value;

use crate::app::Nav;
use crate::i18n;
use crate::ui::DesktopApp;

/// Details-panel payload cache of one selected row, reset on any
/// scan/selection change. Keys are tab ids (`summary`, `links_in`, …).
#[derive(Default)]
pub struct DetailsCache {
    /// Scan path the payloads belong to.
    pub scan: String,
    /// Selected `url_id` the payloads belong to.
    pub url_id: i64,
    /// Tab id → its `details.load` answer (`Rc` — the UI reads a cached
    /// payload every frame without deep-cloning it).
    pub tabs: std::collections::BTreeMap<String, std::rc::Rc<Value>>,
}

/// Line-split and lowercased body text of the html/body details tab, cached
/// per payload. The frame loop renders ~60 fps; splitting and lowering up to
/// 512 KiB of source text must happen once per payload, not once per frame.
#[derive(Default)]
pub(crate) struct HtmlCache {
    /// Payload the lines were split from (`Rc::ptr_eq` identity).
    pub payload: Option<std::rc::Rc<Value>>,
    /// Byte ranges of each line inside `payload["text"]`.
    pub lines: Vec<std::ops::Range<usize>>,
    /// Lowercased lines, built lazily on the first non-empty search.
    pub lowered: Option<Vec<String>>,
}

/// Extra view state that belongs to the widgets, not the command surface.
#[derive(Default)]
pub struct ViewState {
    /// Details payload cache of the current selection.
    pub details: Option<DetailsCache>,
    /// html/body tab: split lines of the current payload.
    pub(crate) html_cache: HtmlCache,
    /// Preset selected in the filter bar.
    pub preset: String,
    /// Links tab: `in` or `out` direction of the visible segment.
    pub links_dir: String,
    /// Selection history for the details "back" button.
    pub sel_hist: Vec<i64>,
    /// Condition dialog: unfolded.
    pub cond_open: bool,
    /// Condition dialog: picked column.
    pub cond_col: String,
    /// Condition dialog: picked operator.
    pub cond_op: String,
    /// Condition dialog: typed value.
    pub cond_val: String,
    /// Column-picker menu: unfolded.
    pub cols_menu: bool,
    /// HTML tab: in-source search text.
    pub html_find: String,
    /// Structured-data tab: picked evidence item.
    pub schema_sel: usize,
    /// Resources tab: kind chip filter (`""` = all).
    pub res_kind: String,
    /// Simple page: address input text.
    pub simple_input: String,
    /// Crawler page: scan-path input text.
    pub crawl_path: String,
    /// Dashboard: text of the check search box.
    pub dash_search: String,
    /// Issues screen: page offset of the selected check's URL list.
    pub issues_offset: i64,
    /// Search screen: text of the query field.
    pub search_text: String,
    /// Search screen: CSS-selector text of the `css` scope.
    pub search_selector: String,
    /// Compare screen: cached `compare.scans` answer (one directory scan).
    pub compare_scans: Option<Value>,
    /// Compare screen: page offset of the active tab's row list.
    pub compare_offset: i64,
    /// Palette: query text and highlighted row.
    pub palette_query: String,
    /// Palette: highlighted row index.
    pub palette_sel: usize,
    /// Profile menu: theme list unfolded.
    pub theme_open: bool,
    /// Start URL of the open scan, cached per scan path.
    pub start_url: Option<(String, Option<String>)>,
    /// Settings dialog: edit buffers, validation messages, cached source lists.
    pub settings: crate::ui::settings::SettingsView,
    /// Row context menu: the row key and where it was opened.
    pub row_menu: Option<(String, eframe::egui::Pos2, u64)>,
    /// URL table: the active section tab (`""` = the first one).
    pub section: String,
    /// URL table: the last unfiltered page answer; its facets are the
    /// crawl-wide counters of the tree and the filter drop-down.
    pub crawl_counts: Option<(String, Value)>,
    /// URL table: the right «Обзор» panel is folded to its rail.
    pub overview_folded: bool,
    /// Column header menu: the column id and where it was opened.
    pub column_menu: Option<(String, eframe::egui::Pos2, u64)>,
    /// Column-chooser dialog: layout snapshot for Cancel and the search box.
    pub cols_snapshot: Option<crate::ui::grid::ColLayout>,
    /// Column-chooser dialog: filter text.
    pub cols_search: String,
}

/// Route the central panel to the active section.
pub fn show(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    match app.model.nav {
        Nav::Crawler => crawler::home(ui, app, &p),
        Nav::Urls => crawler::urls::show(ui, app, &p),
        Nav::Dashboard => crawler::dashboard::show(ui, app, &p),
        Nav::Search => crawler::search::show(ui, app, &p),
        Nav::Compare => crawler::compare::show(ui, app, &p),
        Nav::CompareUrl => crawler::compare_url::show(ui, app, &p),
        Nav::Check => crawler::check::show(ui, app, &p),
        Nav::Logs | Nav::LogsBots | Nav::LogsCompare => log_analyzer::show(ui, &p),
        Nav::Work | Nav::Inbox | Nav::Projects => agent::show(ui, app.model.nav, &p),
    }
}

/// Cell text of a JSON value: `—` for nothing, Да/Нет for booleans.
pub(crate) fn format_cell(v: &Value) -> String {
    match v {
        Value::Null => "—".into(),
        Value::Bool(b) => (if *b { i18n::YES } else { i18n::NO }).into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) if s.is_empty() => "—".into(),
        Value::String(s) => repair_cp1251(s),
        other => other.to_string(),
    }
}

/// Undo Latin-1 mojibake of a windows-1251 page. A server that sends the
/// default `charset=iso-8859-1` header over a cp1251 page makes the crawl
/// record «Ñòàðàÿ» for «Старая»; the stored value stays as crawled, the
/// table shows it readable. Applied only when most letters sit in the
/// U+00C0..U+00FF block — real Latin-1 text is mostly ASCII and stays as is.
pub(crate) fn repair_cp1251(s: &str) -> String {
    let letters = s.chars().filter(|c| c.is_alphabetic()).count();
    let high = s
        .chars()
        .filter(|c| ('\u{C0}'..='\u{FF}').contains(c))
        .count();
    if high < 3 || high * 10 < letters * 6 || s.chars().any(|c| c as u32 > 0xFF) {
        return s.to_string();
    }
    let bytes: Vec<u8> = s.chars().map(|c| c as u32 as u8).collect();
    encoding_rs::WINDOWS_1251
        .decode_without_bom_handling(&bytes)
        .0
        .into_owned()
}

#[cfg(test)]
mod charset_tests {
    use super::repair_cp1251;

    #[test]
    fn latin1_mojibake_of_cp1251_reads_as_cyrillic() {
        assert_eq!(repair_cp1251("Ñòàðàÿ êîäèðîâêà"), "Старая кодировка");
        assert_eq!(repair_cp1251("Café crème"), "Café crème");
        assert_eq!(repair_cp1251("Старая"), "Старая");
        assert_eq!(repair_cp1251("Basic site home"), "Basic site home");
    }
}
