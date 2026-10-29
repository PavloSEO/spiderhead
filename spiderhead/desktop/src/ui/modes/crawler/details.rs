//! Bottom details panel of the URL table: header (status, address, copy,
//! scan name), the canvas tab bar with per-tab counters and the tab content.
//! Payloads come from `details.load`; each tab keeps its own `state`.

use crate::ui::kit::icons as ph;
use eframe::egui::{self, vec2, RichText};
use serde_json::{json, Value};

use crate::i18n;
use crate::theme::{space, ty, Palette};
use crate::ui::kit::{self, bold};
use crate::ui::modes::format_cell;
use crate::ui::modes::DetailsCache;
use crate::ui::DesktopApp;

use super::detail_tabs;

/// Canvas tab bar: `(ui id, label)`; `links` resolves to `links_in`/`links_out`.
const TABS: &[(&str, &str)] = &[
    ("summary", i18n::TAB_OVERVIEW),
    ("headers", i18n::TAB_HEADERS),
    ("links", i18n::TAB_LINKS),
    ("html", i18n::TAB_HTML),
    ("resources", i18n::TAB_RESOURCES),
    ("redirects", i18n::TAB_REDIRECTS),
    ("history", i18n::TAB_HISTORY),
    ("structured", i18n::TAB_STRUCTURED),
];

/// `details.load` id behind a ui tab (links honours the chosen direction).
fn load_id(ui_id: &str, links_dir: &str) -> String {
    match ui_id {
        "links" => format!("links_{}", if links_dir == "out" { "out" } else { "in" }),
        other => other.to_string(),
    }
}

/// Whether the visible load id belongs to a ui tab.
fn tab_active(ui_id: &str, tab: &str) -> bool {
    match ui_id {
        "links" => matches!(tab, "links_in" | "links_out"),
        "headers" => matches!(tab, "headers" | "response"),
        "html" => matches!(tab, "html" | "body"),
        other => tab == other,
    }
}

/// Counter a tab badge shows from the `counts` payload (`0`/`None` hides it).
fn badge(tab: &str, counts: Option<&Value>) -> i64 {
    let Some(c) = counts else { return 0 };
    let get = |k: &str| c.get(k).and_then(Value::as_i64).unwrap_or(0);
    match tab {
        "links" => get("links_in") + get("links_out"),
        "headers" => get("headers"),
        "resources" => get("resources"),
        "redirects" => get("redirects"),
        "structured" => get("jsonld_blocks_found"),
        _ => 0,
    }
}

/// Draw the panel body inside the bottom panel.
pub fn show(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let Some(url_id) = app.model.table.selected else {
        ui.vertical_centered(|ui| {
            ui.add_space(space::XL);
            ui.label(RichText::new(ph::INFO).size(ty::KPI).color(p.faint));
            ui.add_space(space::XS);
            ui.label(RichText::new(i18n::DETAILS_PICK).color(p.weak));
        });
        return;
    };
    let scan_key = app
        .model
        .scan
        .as_ref()
        .map(|s| s.path.display().to_string())
        .unwrap_or_default();
    let scan_name = app
        .model
        .scan
        .as_ref()
        .map(|s| s.display_name())
        .unwrap_or_default();
    let summary = load(app, &scan_key, url_id, "summary");
    let counts = load(app, &scan_key, url_id, "counts");

    // Direction follows the loaded tab so ctl `details.load links_out` and a
    // segment click converge; `links_dir` stays the memory across tab switches.
    match app.model.details_tab.as_str() {
        "links_in" => app.view.links_dir = "in".into(),
        "links_out" => app.view.links_dir = "out".into(),
        _ => {}
    }

    header_row(ui, app, p, url_id, summary.as_deref(), &scan_name);
    tab_bar(ui, app, p, url_id, counts.as_deref());
    let line = ui.min_rect().bottom();
    ui.painter().hline(
        ui.max_rect().x_range(),
        line + 1.0,
        egui::Stroke::new(1.0_f32, p.border),
    );
    ui.add_space(space::SM);

    let tab = app.model.details_tab.clone();
    let links_dir = if app.view.links_dir.is_empty() {
        "in"
    } else {
        app.view.links_dir.as_str()
    }
    .to_string();
    let payload = match tab.as_str() {
        "counts" => counts.clone(),
        _ => load(app, &scan_key, url_id, &tab),
    };
    egui::ScrollArea::vertical()
        .id_salt("details-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| match payload {
            Some(v) if v.get("state").and_then(Value::as_str) == Some("unavailable") => {
                let reason = v
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unavailable");
                ui.label(RichText::new(reason).color(p.weak));
            }
            Some(v) => detail_tabs::render(ui, app, p, &tab, &v, counts.as_deref(), &links_dir),
            None => {
                ui.label(RichText::new(app.model.status.clone()).color(p.weak));
            }
        });
}

/// Panel header: status chip, address, copy, scan name, back and hide.
fn header_row(
    ui: &mut egui::Ui,
    app: &mut DesktopApp,
    p: &Palette,
    url_id: i64,
    summary: Option<&Value>,
    scan_name: &str,
) {
    let url = summary
        .and_then(|v| v.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("—")
        .to_string();
    let status = summary
        .and_then(|v| v.get("status_code"))
        .and_then(Value::as_i64);
    ui.horizontal(|ui| {
        let color = kit::status_color(p, status);
        ui.label(RichText::new("●").size(ty::OVERLINE).color(color));
        match status {
            Some(c) => {
                ui.label(bold(c.to_string()).color(color));
            }
            None => {
                ui.label(RichText::new("—").color(p.faint));
            }
        }
        ui.add(egui::Label::new(bold(&url).size(ty::BODY).color(p.text)).truncate())
            .on_hover_text(&url);
        let icon = egui::Button::new(
            RichText::new(ph::CONTENT_COPY)
                .size(ty::CONTROL)
                .color(p.weak),
        )
        .frame(false);
        if ui
            .add(icon)
            .on_hover_text(i18n::DETAILS_COPY)
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            if let Some(v) = app.act("details.copy", json!({"url_id": url_id})) {
                if let Some(text) = v.get("text").and_then(Value::as_str) {
                    ui.ctx().copy_text(text.to_string());
                }
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(ph::BOTTOM_PANEL_CLOSE)
                            .size(ty::BODY)
                            .color(p.weak),
                    )
                    .frame(false),
                )
                .on_hover_text(i18n::DETAILS_HIDE)
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                app.act("details.toggle", json!({"open": false}));
            }
            ui.label(RichText::new(scan_name).size(ty::CAPTION).color(p.faint));
            if !app.view.sel_hist.is_empty()
                && ui
                    .add(
                        egui::Button::new(
                            RichText::new(ph::ARROW_BACK).size(ty::BODY).color(p.weak),
                        )
                        .frame(false),
                    )
                    .on_hover_text(i18n::DETAILS_BACK)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
            {
                if let Some(prev) = app.view.sel_hist.pop() {
                    app.act("table.select", json!({"url_id": prev}));
                }
            }
        });
    });
}

/// The canvas tab bar with per-tab counters from the `counts` payload.
fn tab_bar(
    ui: &mut egui::Ui,
    app: &mut DesktopApp,
    p: &Palette,
    url_id: i64,
    counts: Option<&Value>,
) {
    let tab = app.model.details_tab.clone();
    let links_dir = app.view.links_dir.clone();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (ui_id, label) in TABS {
            let n = badge(ui_id, counts);
            let text = if n > 0 {
                format!("{label} {n}")
            } else {
                (*label).to_string()
            };
            if kit::tab(ui, p, &text, tab_active(ui_id, &tab)).clicked() {
                let id = load_id(ui_id, &links_dir);
                app.act("details.load", json!({"url_id": url_id, "tab": id}));
            }
        }
    });
}

/// The payload of the visible tab, cached per scan/url in [`DetailsCache`].
/// The `Rc` clone is a refcount bump — payloads run to hundreds of KiB and
/// every frame reads them.
pub(super) fn load(
    app: &mut DesktopApp,
    scan_key: &str,
    url_id: i64,
    tab: &str,
) -> Option<std::rc::Rc<Value>> {
    let needs_reset = app
        .view
        .details
        .as_ref()
        .is_none_or(|c| c.scan != scan_key || c.url_id != url_id);
    if needs_reset {
        app.view.details = Some(DetailsCache {
            scan: scan_key.to_string(),
            url_id,
            tabs: Default::default(),
        });
    }
    if let Some(v) = app
        .view
        .details
        .as_ref()
        .and_then(|c| c.tabs.get(tab).cloned())
    {
        return Some(v);
    }
    let v = std::rc::Rc::new(app.act("details.load", json!({"url_id": url_id, "tab": tab}))?);
    if let Some(c) = app.view.details.as_mut() {
        c.tabs.insert(tab.to_string(), v.clone());
    }
    Some(v)
}

/// Summary groups: caption and the fields in them, in reading order.
const GROUPS: &[(&str, &[&str])] = &[
    (
        "Ответ сервера",
        &[
            "status_code",
            "content_type",
            "response_time",
            "size_bytes",
            "charset",
            "content_encoding",
            "redirect_url",
            "final_url",
            "error",
            "error_kind",
            "cache_status",
            "representation",
        ],
    ),
    (
        "Содержимое",
        &[
            "title",
            "meta_description",
            "h1",
            "h1_2",
            "word_count",
            "text_ratio",
            "images_total",
            "images_missing_alt_attr",
            "jsonld_blocks_found",
        ],
    ),
    (
        "Индексация",
        &["canonical", "final_canonical", "meta_robots", "x_robots"],
    ),
    (
        "Структура и ссылки",
        &[
            "crawl_depth",
            "inlinks",
            "outlinks",
            "external_outlinks",
            "url_id",
            "page_ordinal",
        ],
    ),
];

/// One value, styled by field: status colour, ms, bytes, faint dash.
pub(super) fn field_value(ui: &mut egui::Ui, p: &Palette, name: &str, val: &Value) {
    let faint = |ui: &mut egui::Ui| ui.label(RichText::new("—").color(p.faint));
    if val.is_null() || val.as_str() == Some("") {
        faint(ui);
        return;
    }
    match (name, val) {
        ("status_code", v) => {
            let code = v.as_i64();
            ui.label(bold(format_cell(v)).color(kit::status_color(p, code)));
        }
        ("response_time", v) if v.is_number() => {
            let s = v.as_f64().unwrap_or(0.0);
            ui.label(if s < 1.0 {
                format!("{} {}", (s * 1000.0).round() as i64, i18n::UNIT_MS)
            } else {
                format!("{} с", format!("{s:.2}").replace('.', ","))
            });
        }
        ("size_bytes" | "reported_size_bytes", v) if v.is_number() => {
            ui.label(kit::human_bytes(v.as_i64().unwrap_or(0)));
        }
        (_, v) if v.is_i64() => {
            ui.label(kit::thousands(v.as_i64().unwrap_or(0)));
        }
        (_, v) => {
            ui.add(egui::Label::new(format_cell(v)).wrap());
        }
    }
}

/// Summary tab: grouped field cards in three columns.
pub(super) fn summary(ui: &mut egui::Ui, p: &Palette, v: &Value) {
    let columns: [&[usize]; 3] = [&[0, 3], &[1], &[2]];
    ui.columns(3, |cols| {
        for (col, group_ids) in cols.iter_mut().zip(columns) {
            for &g in group_ids {
                let (title, fields) = GROUPS[g];
                kit::caption(col, p, title);
                col.add_space(space::XS);
                egui::Grid::new(format!("summary-{g}"))
                    .num_columns(2)
                    .spacing(vec2(space::MD, 5.0))
                    .show(col, |ui| {
                        for name in fields {
                            let Some(val) = v.get(*name) else { continue };
                            ui.label(RichText::new(i18n::field_label(name)).color(p.weak));
                            field_value(ui, p, name, val);
                            ui.end_row();
                        }
                    });
                col.add_space(space::MD);
            }
        }
    });
}
