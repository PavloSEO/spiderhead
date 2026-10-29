//! The URL table's grid section: the big virtualized table view, its row and
//! header context menus, the column chooser dialog, the cell full-text
//! popover and the status strip. Every user action goes through
//! `AppModel::dispatch`, so the control channel repeats it exactly.

use eframe::egui::{self, Align, Frame, Layout, Pos2, RichText, ScrollArea, Sense, Vec2};
use serde_json::{json, Value};

use super::super::format_cell;
use crate::data::columns::{column, Kind, COLUMNS};
use crate::i18n;
use crate::theme::{radius, space, ty, Palette};
use crate::ui::grid::{self, GridColumn, GridOut, GridUi, RowSource};
use crate::ui::kit;
use crate::ui::DesktopApp;

/// Row access for the grid: the table's scroll cache backed by the open scan.
struct UrlRows<'a> {
    table: &'a mut crate::app::TableState,
    scan: Option<&'a mut crate::data::session::ScanSession>,
}

impl RowSource for UrlRows<'_> {
    fn rows(&mut self) -> i64 {
        self.table
            .last
            .as_ref()
            .and_then(|l| l.get("filtered_total"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    }

    fn row(&mut self, index: i64) -> Option<Value> {
        self.table.row_at(self.scan.as_deref_mut(), index)
    }
}

/// The grid columns in display order: the registry widened with header
/// captions, base widths and the number flag.
fn grid_columns(g: &GridUi, tab_columns: &[String]) -> Vec<GridColumn> {
    // The tab's column set (the live query, or the default set); the layout
    // orders it and hides entries from it. The chooser extends the set.
    let mut base: Vec<&str> = if tab_columns.is_empty() {
        crate::data::columns::DEFAULT_COLUMNS.to_vec()
    } else {
        tab_columns.iter().map(String::as_str).collect()
    };
    base.retain(|c| *c != "url_id");
    g.layout
        .visible(&base)
        .iter()
        .filter(|n| n.as_str() != "url_id")
        .filter_map(|n| column(n).map(|c| (n.as_str(), c.kind)))
        .map(|(id, kind)| GridColumn {
            id: id.to_string(),
            header: header_label(id).to_string(),
            width: base_width(id, kind),
            min_width: 0.0,
            numeric: matches!(kind, Kind::Int | Kind::Real),
        })
        .collect()
}

/// Short header captions where the long column label would not fit.
fn header_label(id: &str) -> String {
    match id {
        "status_code" => i18n::COL_STATUS.to_string(),
        "content_type" => i18n::COL_CONTENT_TYPE.to_string(),
        "response_time" => i18n::COL_TIME.to_string(),
        "indexable" => i18n::COL_INDEXABLE.to_string(),
        _ => i18n::column_label(id).to_string(),
    }
}

/// Base width at 100% zoom; `None` = the column stretches. Dense by
/// default: seven-plus columns fit a 1440 px window beside the rail.
fn base_width(id: &str, kind: Kind) -> Option<f32> {
    match id {
        "url" => None,
        "status_code" => Some(58.0),
        "indexable" => Some(84.0),
        "content_type" | "meta_robots" | "x_robots" => Some(96.0),
        "title" | "h1" | "canonical" | "final_url" | "redirect_url" => Some(210.0),
        "crawl_depth" | "word_count" | "inlinks" | "outlinks" | "external_outlinks" => Some(66.0),
        "size_bytes" | "response_time" => Some(78.0),
        "page_ordinal" => Some(56.0),
        _ => match kind {
            Kind::Int | Kind::Real | Kind::Bool => Some(72.0),
            _ => Some(140.0),
        },
    }
}

/// Draw the grid, run its events and draw the overlays it asked for.
pub fn show(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette, filtered: i64, total: i64) {
    // The status strip owns a fixed bottom band; the grid takes the rest.
    // A bottom panel inside a Ui would not carve the parent's area, so both
    // halves are explicit child Uis.
    const STRIP_H: f32 = 30.0;
    let avail = ui.available_rect_before_wrap();
    let grid_rect =
        egui::Rect::from_min_max(avail.min, Pos2::new(avail.max.x, avail.max.y - STRIP_H));
    let strip_rect = egui::Rect::from_min_max(Pos2::new(avail.min.x, grid_rect.max.y), avail.max);
    if filtered == 0 {
        // ST-06: an empty filtered set shows the placeholder, not a grid.
        let mut empty_ui = ui.new_child(egui::UiBuilder::new().max_rect(grid_rect));
        empty_ui.set_clip_rect(grid_rect);
        if kit::empty_state(
            &mut empty_ui,
            p,
            crate::ui::kit::icons::FILTER_ALT,
            i18n::TABLE_NO_ROWS,
            i18n::TABLE_NO_ROWS_HINT,
            Some(crate::i18n::strs().clear),
        )
        .filter(|r| r.clicked())
        .into_iter()
        .next()
        .is_some()
        {
            reset_filters(app);
        }
        return;
    }
    let mut cols = grid_columns(&app.model.grid, &app.model.table.query.columns);
    // The address keeps its full text when the window has room: its floor is
    // the widest loaded URL (capped at 600, the column maximum).
    if let Some(c) = cols.iter_mut().find(|c| c.id == "url") {
        c.min_width = url_floor(ui, &app.model.table.cache_rows);
    }
    // The «compact» density is the default (the view setting scales rows).
    let density = match app.model.settings.get_str("view.density").as_str() {
        "comfortable" => 1.3,
        "standard" => 1.15,
        _ => 1.0,
    };
    let sort = (
        app.model.table.query.sort.clone(),
        app.model.table.query.direction == "desc",
    );
    let mut rows = UrlRows {
        table: &mut app.model.table,
        scan: app.model.scan.as_mut(),
    };
    let mut cell = |cui: &mut egui::Ui, p: &Palette, col: &GridColumn, v: Option<&Value>| {
        let kind = column(&col.id).map(|c| c.kind).unwrap_or(Kind::Text);
        cell_ui(cui, p, &col.id, kind, v);
    };
    let out: GridOut = {
        let mut grid_ui = ui.new_child(egui::UiBuilder::new().max_rect(grid_rect));
        grid_ui.set_clip_rect(grid_rect);
        grid::show(
            &mut grid_ui,
            p,
            &mut app.model.grid,
            grid::GridArgs {
                id: "urls-grid",
                columns: &cols,
                sort,
                density,
                source: &mut rows,
                cell: &mut cell,
            },
        )
    };
    // The strip draws over its own band only; the grid never runs under it.
    let mut strip_ui = ui.new_child(egui::UiBuilder::new().max_rect(strip_rect));
    strip_ui.set_clip_rect(strip_rect);
    status_strip(&mut strip_ui, app, p, filtered, total);
    // Any plain click on the grid closes a full-text popover (DC-01).
    if out.response.clicked() {
        app.model.grid.transients.popover = None;
    }
    let events = out.events;
    for ev in events {
        match ev {
            grid::Event::Sort { column, dir } => {
                let params = match dir {
                    Some(true) => json!({"column": column, "direction": "asc"}),
                    Some(false) => json!({"column": column, "direction": "desc"}),
                    None => json!({"column": null}),
                };
                app.act("table.sort", params);
                persist(app);
            }
            grid::Event::FilterMenu { column, pos } => {
                app.view.column_menu = Some((column, pos, ui.ctx().cumulative_frame_nr()));
            }
            grid::Event::Activate => {
                // The selection sync below already moved the details panel.
            }
            grid::Event::Copy => copy_selection(ui, app),
            grid::Event::ContextMenu { key, pos } => {
                app.view.row_menu = Some((key, pos, ui.ctx().cumulative_frame_nr()));
            }
            grid::Event::FocusFind => app.model.find_focus = true,
            grid::Event::ColumnsChanged => {
                app.model.sync_table_columns();
                persist(app);
            }
            grid::Event::Popover { key, column, pos } => {
                let text = cached_cell_text(app, &key, &column);
                app.model.grid.transients.popover = Some(grid::Popover {
                    row: key,
                    col: column,
                    text,
                    pos: [pos.x, pos.y],
                });
                ui.ctx().request_repaint();
            }
        }
    }
    sync_selection(ui, app);
    row_menu(ui, app, p);
    column_menu(ui, app, p);
    popover(ui, app, p);
    columns_dialog(ui, app, p);
}

fn reset_filters(app: &mut DesktopApp) {
    app.filter_text.clear();
    app.view.preset.clear();
    app.act(
        "table.filter",
        json!({"filters": [], "preset": null, "issue_check": null, "issue_severity": null}),
    );
}

/// Persist the layout after a widget-side change (drag, zoom).
fn persist(app: &mut DesktopApp) {
    app.model.persist_grid();
}

/// Move the details panel to the grid's active row; one `table.select` per
/// change, no matter which side (pointer, keyboard, ctl) moved it.
fn sync_selection(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let active = app.model.grid.selection.active.clone();
    let id = active.and_then(|k| k.parse::<i64>().ok());
    if id != app.model.table.selected {
        app.act("table.select", json!({"url_id": id}));
        ui.ctx().request_repaint();
    }
}

/// Ctrl+C / menu copy: the selection as TSV, with the marching border and a
/// toast afterwards. The `all` mode copies the whole filtered set.
fn copy_selection(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let params = if app.model.grid.selection.all {
        json!({"header": true})
    } else {
        let ids: Vec<Value> = app
            .model
            .grid
            .selection
            .keys
            .iter()
            .filter_map(|k| k.parse::<i64>().ok().map(Value::from))
            .collect();
        if ids.is_empty() {
            return;
        }
        json!({"selected": ids, "header": false})
    };
    let Some(v) = app.act("table.copy", params) else {
        return;
    };
    let Some(tsv) = v.get("tsv").and_then(Value::as_str) else {
        return;
    };
    let n = v.get("rows").and_then(Value::as_i64).unwrap_or(0).max(0) as usize;
    ui.ctx().copy_text(tsv.to_string());
    app.model.grid.transients.mark_copied();
    app.model.grid.transients.toast(i18n::copied_toast(n));
    app.model.status = i18n::TSV_COPIED.into();
    ui.ctx().request_repaint();
}

/// Full text of a cell from the scroll cache (the popover's data source).
fn cached_cell_text(app: &DesktopApp, key: &str, col: &str) -> String {
    let id = key.parse::<i64>().ok();
    app.model
        .table
        .cache_rows
        .iter()
        .find(|r| r.get("url_id").and_then(Value::as_i64) == id)
        .and_then(|r| r.get(col))
        .map(format_cell)
        .unwrap_or_default()
}

/// Draw one cell by column; numbers right-aligned, `—` faint.
fn cell_ui(ui: &mut egui::Ui, p: &Palette, id: &str, kind: Kind, cell: Option<&Value>) {
    let none = || RichText::new("—").color(p.faint);
    let Some(v) = cell else {
        // The row has not been fetched yet: a quiet placeholder keeps the
        // scroll fluid instead of a blank.
        ui.label(RichText::new("…").color(p.faint));
        return;
    };
    let null = v.is_null() || v.as_str() == Some("");
    let number = if matches!(kind, Kind::Int | Kind::Real) {
        v.as_f64()
    } else {
        None
    };
    // Figures and addresses render in the bundled monospace face.
    fn mono<S: Into<String>>(t: S) -> RichText {
        RichText::new(t).monospace()
    }
    match (id, kind) {
        ("status_code", _) => {
            let code = v.as_i64();
            let color = kit::status_color(p, code);
            ui.label(RichText::new("●").size(ty::OVERLINE).color(color));
            match code {
                Some(c) => ui.label(mono(c.to_string()).color(color)),
                None => ui.label(none()),
            };
        }
        ("indexable" | "canonical_self" | "canonical_other" | "robots_blocked", _) => {
            match v.as_bool() {
                Some(true) if id == "robots_blocked" => {
                    ui.label(
                        RichText::new(crate::ui::kit::icons::BLOCK)
                            .size(ty::TITLE)
                            .color(p.critical),
                    )
                    .on_hover_text(i18n::NO);
                }
                Some(true) => {
                    ui.label(
                        RichText::new(crate::ui::kit::icons::CHECK_CIRCLE)
                            .size(ty::TITLE)
                            .color(p.opportunity),
                    )
                    .on_hover_text(i18n::YES);
                }
                Some(false) => {
                    ui.label(
                        RichText::new(crate::ui::kit::icons::BLOCK)
                            .size(ty::TITLE)
                            .color(p.disabled),
                    )
                    .on_hover_text(i18n::NO);
                }
                None => {
                    ui.label(none());
                }
            }
        }
        ("response_time", _) if number.is_some() => {
            let s = number.unwrap_or(0.0);
            let text = if s < 1.0 {
                format!("{} {}", (s * 1000.0).round() as i64, i18n::UNIT_MS)
            } else {
                format!("{} с", format!("{s:.2}").replace('.', ","))
            };
            ui.label(mono(text));
        }
        ("size_bytes", _) if number.is_some() => {
            ui.label(mono(kit::human_bytes(number.unwrap_or(0.0) as i64)));
        }
        ("content_type", _) if !null => {
            let full = v.as_str().unwrap_or_default();
            let short = full.split(';').next().unwrap_or(full);
            ui.label(RichText::new(short).color(p.weak))
                .on_hover_text(full);
        }
        (_, Kind::Int) if number.is_some() => {
            ui.label(mono(kit::thousands(number.unwrap_or(0.0) as i64)));
        }
        (_, Kind::Real) if number.is_some() => {
            ui.label(mono(format!("{:.2}", number.unwrap_or(0.0))));
        }
        (_, Kind::Bool) => match v.as_bool() {
            Some(b) => {
                ui.label(RichText::new(if b { i18n::YES } else { i18n::NO }));
            }
            None => {
                ui.label(none());
            }
        },
        _ if null => {
            ui.label(none());
        }
        _ if col_is_url(id) => {
            let text = format_cell(v);
            ui.add(egui::Label::new(mono(text.clone())).truncate())
                .on_hover_text(&text);
        }
        _ => {
            let text = format_cell(v);
            ui.add(egui::Label::new(RichText::new(&text).size(ty::BODY)).truncate())
                .on_hover_text(&text);
        }
    }
}

/// Address-like columns that read best in the monospace face.
fn col_is_url(id: &str) -> bool {
    matches!(
        id,
        "url" | "final_url" | "redirect_url" | "canonical" | "final_canonical"
    )
}

/// One popup area anchored at `pos`; closes on a click elsewhere (the frame
/// that opened it does not count) or on Escape.
fn popup_area(
    ui: &egui::Ui,
    id: &str,
    pos: Pos2,
    open_frame: u64,
    add: impl FnOnce(&mut egui::Ui),
) -> bool {
    let mut close = false;
    egui::Area::new(egui::Id::new(id))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ui.ctx(), |ui| {
            Frame::popup(ui.style())
                .corner_radius(radius::MD as f32)
                .show(ui, |ui| {
                    add(ui);
                });
            let frame = ui.ctx().cumulative_frame_nr();
            let clicked_away = ui.input(|i| {
                i.pointer.any_click()
                    && i.pointer
                        .interact_pos()
                        .is_none_or(|m| !ui.clip_rect().contains(m))
            });
            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
            close = esc || (clicked_away && frame > open_frame);
        });
    close
}

fn menu_item(ui: &mut egui::Ui, p: &Palette, label: &str) -> bool {
    let r = ui.add(
        egui::Button::new(RichText::new(label).size(ty::BODY).color(p.text))
            .frame(false)
            .min_size(Vec2::new(230.0, 26.0)),
    );
    let hit = r.hovered();
    if hit {
        ui.painter()
            .rect_filled(r.rect, radius::SM as f32, p.menu_hover);
    }
    r.clicked()
}

fn menu_sep(ui: &mut egui::Ui, p: &Palette) {
    ui.add_space(space::XS);
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
    ui.painter().line_segment(
        [rect.min, Pos2::new(rect.max.x, rect.min.y)],
        egui::Stroke::new(1.0_f32, p.border_soft),
    );
    ui.add_space(space::XS);
}

/// The row context menu: copy actions and the browser hand-off (CM-01…05).
fn row_menu(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let Some((key, pos, frame)) = app.view.row_menu.clone() else {
        return;
    };
    let mut action: Option<u8> = None;
    let close = popup_area(ui, "urls-row-menu", pos, frame, |ui| {
        ui.set_min_width(240.0);
        if menu_item(ui, p, i18n::GRID_MENU_COPY) {
            action = Some(1);
        }
        ui.separator();
        if menu_item(ui, p, i18n::GRID_MENU_COPY_URL) {
            action = Some(2);
        }
        if menu_item(ui, p, i18n::GRID_MENU_COPY_PRETTY) {
            action = Some(3);
        }
        if menu_item(ui, p, i18n::GRID_MENU_COPY_UGLY) {
            action = Some(4);
        }
        ui.separator();
        if menu_item(ui, p, i18n::GRID_MENU_OPEN) {
            action = Some(5);
        }
    });
    if let Some(a) = action {
        run_row_action(ui, app, key.as_str(), a);
        app.view.row_menu = None;
    } else if close {
        app.view.row_menu = None;
    }
}

/// Run one row-menu action for the selection (the clicked row is in it).
fn run_row_action(ui: &mut egui::Ui, app: &mut DesktopApp, key: &str, action: u8) {
    match action {
        1 => copy_selection(ui, app),
        2..=4 => {
            let url = row_url(app, key);
            if let Some(url) = url {
                let text = match action {
                    2 => url,
                    3 => pretty_url(&url),
                    _ => ugly_url(&url),
                };
                ui.ctx().copy_text(text);
                app.model.grid.transients.toast(i18n::GRID_URL_COPIED);
                app.model.status = i18n::GRID_URL_COPIED.into();
            }
        }
        5 => {
            let mut urls: Vec<String> = app
                .model
                .grid
                .selection
                .keys
                .iter()
                .filter_map(|k| row_url(app, k))
                .collect();
            if urls.is_empty() {
                if let Some(u) = row_url(app, key) {
                    urls.push(u);
                }
            }
            let capped = urls.len() > 5;
            for u in urls.into_iter().take(5) {
                open_in_browser(&u);
            }
            if capped {
                app.model.grid.transients.toast(i18n::GRID_OPEN_CAP);
            }
        }
        _ => {}
    }
}

/// The address of one row key, straight from the scan (never from the cache).
fn row_url(app: &DesktopApp, key: &str) -> Option<String> {
    let id = key.parse::<i64>().ok()?;
    let scan = app.model.scan.as_ref()?;
    scan.con
        .query_row("SELECT url FROM urls WHERE url_id=?1", [id], |r| {
            r.get::<_, String>(0)
        })
        .ok()
}

/// The header menu: sort, autofit, freeze, hide and the column chooser.
fn column_menu(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let Some((col, pos, frame)) = app.view.column_menu.clone() else {
        return;
    };
    let mut action: Option<u8> = None;
    let close = popup_area(ui, "urls-col-menu", pos, frame, |ui| {
        ui.set_min_width(240.0);
        let sorted = app.model.table.query.sort.as_deref() == Some(col.as_str());
        if menu_item(ui, p, i18n::GRID_MENU_SORT_ASC) && !sorted {
            action = Some(1);
        }
        if menu_item(ui, p, i18n::GRID_MENU_SORT_DESC) && !sorted {
            action = Some(2);
        }
        if sorted && menu_item(ui, p, i18n::GRID_MENU_SORT_NONE) {
            action = Some(3);
        }
        menu_sep(ui, p);
        if menu_item(ui, p, i18n::GRID_MENU_AUTOFIT) {
            action = Some(4);
        }
        let names: Vec<&str> = COLUMNS.iter().map(|(n, _)| *n).collect();
        let frozen = app.model.grid.layout.frozen();
        let visible = app.model.grid.layout.visible(&names);
        let in_frozen = visible.iter().take(frozen).any(|c| c == &col);
        if !in_frozen && frozen == 0 && menu_item(ui, p, i18n::GRID_MENU_PIN) {
            action = Some(5);
        } else if in_frozen && menu_item(ui, p, i18n::GRID_MENU_UNPIN) {
            action = Some(6);
        }
        menu_sep(ui, p);
        if menu_item(ui, p, i18n::GRID_MENU_HIDE) {
            action = Some(7);
        }
        if menu_item(ui, p, i18n::GRID_MENU_CONFIGURE) {
            action = Some(8);
        }
    });
    if let Some(a) = action {
        app.view.column_menu = None;
        match a {
            1 => {
                app.act("table.sort", json!({"column": col, "direction": "asc"}));
            }
            2 => {
                app.act("table.sort", json!({"column": col, "direction": "desc"}));
            }
            3 => {
                app.act("table.sort", json!({"column": null}));
            }
            4 => autofit(ui, app, &col),
            5 => {
                app.act("table.column.pin", json!({"upto": col}));
            }
            6 => {
                app.act("table.column.pin", json!({"upto": null}));
            }
            7 => {
                app.act("table.column.hide", json!({"column": col}));
            }
            8 => {
                app.act("table.columns.configure", json!({"open": true}));
            }
            _ => {}
        }
        persist(app);
    } else if close {
        app.view.column_menu = None;
    }
}

/// Widest cached URL in the cell's monospace face plus cell padding,
/// clamped to 120..600.
fn url_floor(ui: &egui::Ui, rows: &[Value]) -> f32 {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let widest = rows
        .iter()
        .filter_map(|r| r.get("url").and_then(Value::as_str))
        .map(|u| {
            ui.painter()
                .layout_no_wrap(u.to_owned(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    (widest + 16.0).clamp(120.0, 600.0)
}

/// Fit a column to the widest visible cell (double-click on a border).
fn autofit(ui: &mut egui::Ui, app: &mut DesktopApp, col: &str) {
    let font = egui::FontId::proportional(ty::BODY);
    let mut widest = ui
        .painter()
        .layout_no_wrap(
            header_label(col).to_string(),
            font.clone(),
            egui::Color32::WHITE,
        )
        .size()
        .x;
    for row in &app.model.table.cache_rows {
        if let Some(v) = row.get(col) {
            let text = format_cell(v);
            let w = ui
                .painter()
                .layout_no_wrap(text, font.clone(), egui::Color32::WHITE)
                .size()
                .x;
            widest = widest.max(w);
        }
    }
    let id = col.to_string();
    app.model.grid.layout.set_width(&id, widest + 24.0);
    app.model.sync_table_columns();
}

/// The full-text popover of a double-clicked cell (DC-01).
fn popover(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let Some(pop) = app.model.grid.transients.popover.clone() else {
        return;
    };
    let pos = Pos2::new(pop.pos[0], pop.pos[1]);
    egui::Area::new(egui::Id::new("urls-cell-popover"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos + Vec2::new(10.0, 12.0))
        .show(ui.ctx(), |ui| {
            Frame::popup(ui.style())
                .corner_radius(radius::MD as f32)
                .show(ui, |ui| {
                    ui.set_max_width(420.0);
                    ui.set_min_width(180.0);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(header_label(&pop.col))
                                .size(ty::OVERLINE)
                                .color(p.weak),
                        );
                        ui.separator();
                        if pop.text.is_empty() {
                            ui.label(RichText::new("—").color(p.faint));
                        } else {
                            ui.label(RichText::new(&pop.text).size(ty::BODY));
                        }
                    });
                });
        });
    // Clicks anywhere else close it; Esc closes through the widget state.
    let clicked = ui.input(|i| i.pointer.any_click());
    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if clicked || esc {
        app.model.grid.transients.popover = None;
        ui.ctx().request_repaint();
    }
}

/// The column chooser dialog (CF-01…07): visibility, order, reset, cancel.
fn columns_dialog(ui: &mut egui::Ui, app: &mut DesktopApp, _p: &Palette) {
    if !app.model.windows.contains("columns") {
        app.view.cols_snapshot = None;
        return;
    }
    if app.view.cols_snapshot.is_none() {
        app.view.cols_snapshot = Some(app.model.grid.layout.clone());
    }
    let mut close: Option<bool> = None; // Some(revert)
    egui::Window::new(RichText::new(i18n::COLS_TITLE).size(ty::TITLE))
        .id(egui::Id::new("urls-cols-dialog"))
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(
            Frame::popup(ui.style())
                .corner_radius(radius::DIALOG as f32)
                .inner_margin(egui::Margin::same(16)),
        )
        .show(ui.ctx(), |ui| {
            ui.add_space(space::XS);
            ui.add(
                egui::TextEdit::singleline(&mut app.view.cols_search)
                    .desired_width(340.0)
                    .hint_text(i18n::COLS_SEARCH),
            );
            ui.add_space(space::SM);
            ScrollArea::vertical()
                .max_height(340.0)
                .id_salt("cols-list")
                .show(ui, |ui| {
                    let search = app.view.cols_search.to_lowercase();
                    let names: Vec<&str> = COLUMNS.iter().map(|(n, _)| *n).collect();
                    let visible = app.model.grid.layout.visible(&names);
                    for (name, _) in COLUMNS {
                        let label = i18n::column_label(name);
                        if !search.is_empty() && !label.to_lowercase().contains(&search) {
                            continue;
                        }
                        let mut on = visible.iter().any(|c| c == name);
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut on, label).changed() {
                                let method = if on {
                                    "table.column.show"
                                } else {
                                    "table.column.hide"
                                };
                                app.act(method, json!({"column": name}));
                                // url_id carries row identity and never shows.
                            }
                            if on {
                                let idx = visible.iter().position(|c| c == name).unwrap_or(0);
                                if idx > 0 && ui.small_button("↑").clicked() {
                                    let before = visible.get(idx - 1).map(|c| c.to_string());
                                    if let Some(prev) = before {
                                        // Move the previous one after this one.
                                        app.act(
                                            "table.column.move",
                                            json!({"column": prev, "before": name}),
                                        );
                                    }
                                }
                                if idx + 1 < visible.len() && ui.small_button("↓").clicked() {
                                    let next = visible[idx + 1].clone();
                                    app.act(
                                        "table.column.move",
                                        json!({"column": name, "before": next}),
                                    );
                                }
                            }
                        });
                    }
                });
            ui.add_space(space::SM);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(i18n::COLS_RESET).clicked() {
                    app.act("table.columns.reset", json!({}));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(i18n::COLS_OK).clicked() {
                        close = Some(false);
                    }
                    if ui.button(i18n::COLS_CANCEL).clicked() {
                        close = Some(true);
                    }
                });
            });
        });
    if let Some(revert) = close {
        if revert {
            if let Some(snapshot) = app.view.cols_snapshot.clone() {
                app.model.grid.layout = snapshot;
                app.model.sync_table_columns();
            }
        }
        persist(app);
        app.act("table.columns.configure", json!({"open": false}));
        app.view.cols_snapshot = None;
        ui.ctx().request_repaint();
    }
}

/// Status strip: row counters, the selection count, zoom and the copy hint.
fn status_strip(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette, filtered: i64, total: i64) {
    ui.add_space(space::XS);
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(space::MD as i8, space::XS as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let selected = app.model.grid.selection.len_total(filtered.max(0) as usize);
                ui.label(
                    RichText::new(i18n::rows_line(
                        filtered,
                        total,
                        app.model.table.cache_rows.len(),
                    ))
                    .size(ty::CAPTION)
                    .color(p.weak),
                );
                ui.label(
                    RichText::new(i18n::selected_count_line(selected))
                        .size(ty::CAPTION)
                        .color(p.weak),
                );
                if !app.model.status.is_empty() {
                    ui.label(
                        RichText::new(&app.model.status)
                            .size(ty::CAPTION)
                            .color(p.critical),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(i18n::GRID_COPY_HINT)
                            .size(ty::CAPTION)
                            .color(p.faint),
                    );
                    // Zoom stepper: the ladder, by steps (grid-1 §7–20).
                    ui.label(
                        RichText::new(format!("{}%", app.model.grid.zoom.percent() as i64))
                            .size(ty::CAPTION)
                            .color(p.weak),
                    );
                    if ui
                        .add(egui::Button::new(RichText::new("+").size(ty::CAPTION)).frame(false))
                        .on_hover_text(i18n::GRID_ZOOM)
                        .clicked()
                    {
                        app.act("table.zoom", json!({"step": "in"}));
                    }
                    if ui
                        .add(egui::Button::new(RichText::new("−").size(ty::CAPTION)).frame(false))
                        .on_hover_text(i18n::GRID_ZOOM)
                        .clicked()
                    {
                        app.act("table.zoom", json!({"step": "out"}));
                    }
                    if !app.model.details_open
                        && ui
                            .add(
                                egui::Button::new(
                                    RichText::new(crate::ui::kit::icons::KEYBOARD_ARROW_UP)
                                        .size(ty::BODY)
                                        .color(p.weak),
                                )
                                .frame(false),
                            )
                            .on_hover_text(i18n::DETAILS_SHOW)
                            .clicked()
                    {
                        app.act("details.toggle", json!({"open": true}));
                    }
                });
            });
        });
}

/// Percent-decode an address for the «pretty» copy variant.
fn pretty_url(url: &str) -> String {
    let bytes = url.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode the characters that do not survive round-trips, for the
/// «ugly» copy variant.
fn ugly_url(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for b in url.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => out.push(b as char),
            b'-' | b'.' | b'_' | b'~' | b'/' | b':' | b'?' | b'#' | b'[' | b']' | b'@' | b'!'
            | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Open one address in the system browser (CM-05).
fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_url_decodes_percent_escapes() {
        assert_eq!(
            pretty_url("https://a.ru/%D0%B4%D0%BE%D0%BC"),
            "https://a.ru/дом"
        );
        assert_eq!(pretty_url("https://a.ru/a%20b"), "https://a.ru/a b");
        // A stray percent stays as-is.
        assert_eq!(pretty_url("100%"), "100%");
        assert_eq!(pretty_url("a%zz"), "a%zz");
    }

    #[test]
    fn ugly_url_encodes_the_unsafe_characters() {
        assert_eq!(
            ugly_url("https://a.ru/дом"),
            "https://a.ru/%D0%B4%D0%BE%D0%BC"
        );
        assert_eq!(ugly_url("https://a.ru/a b?q=1"), "https://a.ru/a%20b?q=1");
        assert_eq!(ugly_url("https://a.ru/"), "https://a.ru/");
    }

    #[test]
    fn ugly_and_pretty_round_trip() {
        let raw = "https://a.ru/путь?a=1&b=два";
        assert_eq!(pretty_url(&ugly_url(raw)), raw);
    }
}
