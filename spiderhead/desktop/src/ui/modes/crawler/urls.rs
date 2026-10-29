//! The URL table: section tabs, a one-line toolbar (section filter, search,
//! condition builder, column picker, TSV export, «Обзор» toggle), the big
//! virtualized grid with its status strip, details below it and the
//! collapsible «Обзор» tree on the right.
//! The grid itself lives in `url_grid`; states follow `StatesMain`: no scan,
//! first load, read error, no rows.

use crate::ui::kit::icons as ph;
use eframe::egui::{self, vec2, Align, Layout, RichText};
use serde_json::{json, Value};

use super::{sections, url_grid};
use crate::data::columns::{column, Kind, COLUMNS};
use crate::data::filters::ops;
use crate::i18n::{self, strs};
use crate::theme::{space, ty, Palette};
use crate::ui::checks;
use crate::ui::kit::{self, bold};

/// Narrower than this the «Обзор» panel stays folded: the table needs the room.
const OVERVIEW_MIN_WINDOW: f32 = 1100.0;
use crate::ui::DesktopApp;

/// Draw the section: groups, filters, the big grid, status strip, details.
pub fn show(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let s = strs();
    if app.model.scan.is_none() {
        if kit::empty_state(
            ui,
            p,
            ph::LANGUAGE,
            s.no_scan,
            i18n::TABLE_EMPTY_HINT,
            Some(i18n::EMPTY_OPEN_SCAN),
        )
        .is_some_and(|r| r.clicked())
        {
            app.act("ui.navigate", json!({"id": "crawler"}));
        }
        return;
    }
    let last = app.model.table.last.clone();
    let count = |key: &str| {
        last.as_ref()
            .and_then(|l| l.get(key))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0)
    };
    let (filtered, total) = (count("filtered_total"), count("total"));

    let _ = (filtered, total);
    let counts = sections::crawl_counts(app);
    // Chrome above the table: the tab row and one toolbar row (≈ 60 px).
    sections::tab_strip(ui, app, p);
    ui.add_space(space::XS);
    let wide = ui.available_width() >= OVERVIEW_MIN_WINDOW;
    filter_bar(ui, app, p, counts.as_ref(), wide);
    ui.add_space(space::XS);

    // Panels carve their own rects out of this Ui before the table takes the
    // rest, so neither the tree nor the details ever paint over cells.
    if wide && !app.view.overview_folded {
        egui::SidePanel::right("overview")
            .resizable(true)
            .default_width(280.0)
            .width_range(200.0..=480.0)
            .frame(
                egui::Frame::new()
                    .fill(p.surface)
                    .stroke(egui::Stroke::new(1.0_f32, p.border))
                    .inner_margin(egui::Margin::symmetric(space::XS as i8, space::XS as i8)),
            )
            .show_inside(ui, |ui| sections::overview(ui, app, p, counts.as_ref()));
    }
    if app.model.details_open && app.model.table.selected.is_some() {
        egui::TopBottomPanel::bottom("details")
            .resizable(true)
            .default_height(240.0)
            .min_height(96.0)
            .frame(
                egui::Frame::new()
                    .fill(p.surface)
                    .stroke(egui::Stroke::new(1.0_f32, p.border))
                    .inner_margin(egui::Margin::symmetric(space::SM as i8, space::XS as i8)),
            )
            .show_inside(ui, |ui| super::details::show(ui, app, p));
    }

    let table_rect = ui.available_rect_before_wrap();
    let mut table_ui = ui.new_child(egui::UiBuilder::new().max_rect(table_rect));
    table_ui.set_clip_rect(table_rect.intersect(ui.clip_rect()));
    egui::Frame::new()
        .fill(p.surface)
        .stroke(egui::Stroke::new(1.0_f32, p.border))
        .inner_margin(egui::Margin::same(1))
        .show(&mut table_ui, |ui| {
            if app.model.table.last.is_none() && app.model.status.is_empty() {
                kit::empty_state(ui, p, ph::HOURGLASS_TOP, i18n::STATE_LOADING, "", None);
                return;
            }
            if app.model.table.last.is_none() && !app.model.status.is_empty() {
                let err = app.model.status.clone();
                if kit::empty_state(
                    ui,
                    p,
                    ph::ERROR,
                    i18n::STATE_ERR_TITLE,
                    &err,
                    Some(i18n::STATE_RETRY),
                )
                .is_some_and(|r| r.clicked())
                {
                    app.act("table.page", json!({}));
                }
                return;
            }
            url_grid::show(ui, app, p, filtered, total);
        });
}

fn reset_filters(app: &mut DesktopApp) {
    app.filter_text.clear();
    app.view.preset.clear();
    app.act(
        "table.filter",
        json!({"filters": [], "preset": null, "issue_check": null, "issue_severity": null}),
    );
}

/// Section filter, search box, condition builder, the active-condition
/// summary and the right-side column/export/overview actions.
fn filter_bar(
    ui: &mut egui::Ui,
    app: &mut DesktopApp,
    p: &Palette,
    last: Option<&Value>,
    wide: bool,
) {
    let s = strs();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS + 2.0;
        sections::filter_combo(ui, app, last);
        let response = ui.add(
            egui::TextEdit::singleline(&mut app.filter_text)
                .desired_width(220.0)
                .margin(vec2(8.0, 3.0))
                .hint_text(format!("{}  {}", ph::SEARCH, s.url_contains)),
        );
        // Ctrl+F (the widget asks through `find_focus`) lands here once.
        if app.model.find_focus {
            response.request_focus();
            app.model.find_focus = false;
        }
        if (response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
            && !app.filter_text.is_empty()
        {
            app.act(
                "table.filter",
                json!({
                    "preset": null,
                    "filters": [{"column": "url", "op": "contains", "value": app.filter_text}],
                }),
            );
        }
        condition_dialog(ui, app, p);
        active_conditions(ui, app, p);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if wide {
                overview_toggle(ui, app, p);
            }
            export_button(ui, app, p);
            columns_menu(ui, app, p);
        });
    });
}

/// `+ Условие`: a small popup that appends one validated filter object.
fn condition_dialog(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let btn = ui.add(
        egui::Button::new(
            RichText::new(i18n::COND_ADD)
                .color(p.weak)
                .size(ty::CAPTION),
        )
        .fill(p.surface),
    );
    egui::Popup::menu(&btn).show(|ui| {
        ui.set_min_width(280.0);
        if app.view.cond_col.is_empty() {
            app.view.cond_col = "url".to_string();
            app.view.cond_op = "contains".to_string();
        }
        let kind = column(&app.view.cond_col)
            .map(|c| c.kind)
            .unwrap_or(Kind::Text);
        egui::Grid::new("cond-grid")
            .num_columns(2)
            .spacing(vec2(space::MD, space::SM))
            .show(ui, |ui| {
                ui.label(RichText::new(i18n::COND_COLUMN).color(p.weak));
                egui::ComboBox::from_id_salt("cond-col")
                    .selected_text(i18n::column_label(&app.view.cond_col))
                    .show_ui(ui, |ui| {
                        ui.set_min_width(180.0);
                        egui::ScrollArea::vertical()
                            .max_height(260.0)
                            .show(ui, |ui| {
                                let mut picked = None;
                                for (name, _) in COLUMNS {
                                    if ui
                                        .selectable_label(
                                            app.view.cond_col == *name,
                                            i18n::column_label(name),
                                        )
                                        .clicked()
                                    {
                                        picked = Some(*name);
                                    }
                                }
                                if let Some(name) = picked {
                                    app.view.cond_col = name.to_string();
                                    let kind = column(name).map(|c| c.kind).unwrap_or(Kind::Text);
                                    app.view.cond_op = ops(kind)[0].to_string();
                                    app.view.cond_val.clear();
                                }
                            });
                    });
                ui.end_row();
                ui.label(RichText::new(i18n::COND_OP).color(p.weak));
                egui::ComboBox::from_id_salt("cond-op")
                    .selected_text(i18n::op_label(&app.view.cond_op))
                    .show_ui(ui, |ui| {
                        for op in ops(kind) {
                            if ui
                                .selectable_label(app.view.cond_op == *op, i18n::op_label(op))
                                .clicked()
                            {
                                app.view.cond_op = op.to_string();
                            }
                        }
                    });
                ui.end_row();
                if needs_value(&app.view.cond_op) {
                    ui.label(RichText::new(i18n::COND_VALUE).color(p.weak));
                    ui.add(egui::TextEdit::singleline(&mut app.view.cond_val).desired_width(180.0));
                    ui.end_row();
                }
            });
        if ui
            .add(egui::Button::new(bold(i18n::COND_APPLY).color(p.on_accent)).fill(p.accent))
            .clicked()
        {
            if let Some(filter) = build_filter(app) {
                let mut list = app.model.table.query.filters.clone();
                list.push(filter);
                app.act(
                    "table.filter",
                    json!({"filters": Value::Array(list), "preset": null}),
                );
                app.view.cond_val.clear();
            }
        }
    });
}

/// Operators that still need a typed value.
fn needs_value(op: &str) -> bool {
    !matches!(op, "empty" | "not_empty" | "is_null" | "not_null")
}

/// Compose a filter object from the dialog draft; `None` on bad input.
fn build_filter(app: &DesktopApp) -> Option<Value> {
    let col = app.view.cond_col.clone();
    let op = app.view.cond_op.clone();
    let kind = column(&col).map(|c| c.kind).unwrap_or(Kind::Text);
    if !needs_value(&op) {
        return Some(json!({"column": col, "op": op}));
    }
    let text = app.view.cond_val.trim().to_string();
    if text.is_empty() {
        return None;
    }
    let value = match op.as_str() {
        "in" | "not_in" => {
            let parts: Vec<Value> = text
                .split(',')
                .filter_map(|part| typed_value(kind, part.trim()))
                .collect();
            if parts.is_empty() {
                return None;
            }
            Value::Array(parts)
        }
        "between" => {
            let parts: Vec<Value> = text
                .split(',')
                .filter_map(|part| typed_value(kind, part.trim()))
                .collect();
            if parts.len() != 2 {
                return None;
            }
            Value::Array(parts)
        }
        _ => typed_value(kind, &text)?,
    };
    Some(json!({"column": col, "op": op, "value": value}))
}

/// Parse a typed value for the filter kind; `None` rejects bad input.
fn typed_value(kind: Kind, text: &str) -> Option<Value> {
    match kind {
        Kind::Int => text.parse::<i64>().ok().map(Value::from),
        Kind::Real => text.parse::<f64>().ok().map(Value::from),
        Kind::Bool => match text {
            "true" | "да" | "1" => Some(Value::Bool(true)),
            "false" | "нет" | "0" => Some(Value::Bool(false)),
            _ => None,
        },
        Kind::Class | Kind::Text => Some(Value::String(text.to_string())),
    }
}

/// Condition-builder filters and issue filters as one plain text line with
/// a reset cross: what is applied stays visible without a row of chips.
fn active_conditions(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let q = &app.model.table.query;
    let mut parts: Vec<String> = q
        .filters
        .iter()
        .map(|f| {
            let col = f.get("column").and_then(Value::as_str).unwrap_or("");
            let op = f.get("op").and_then(Value::as_str).unwrap_or("eq");
            let value = f
                .get("value")
                .map(crate::ui::modes::format_cell)
                .unwrap_or_default();
            i18n::filter_chip(col, op, &value)
        })
        .collect();
    // A section filter is shown by the drop-down already.
    let from_section = parts.len() == 1 && q.preset.is_none() && q.issue_check.is_null();
    let first = match &q.issue_check {
        Value::Array(a) => a.first().and_then(Value::as_str).map(str::to_string),
        v => v.as_str().map(str::to_string),
    };
    if let Some(check) = first {
        parts.push(checks::info(&check).title.to_string());
    }
    if let Some(sev) = q.issue_severity.as_str().and_then(kit::Severity::parse) {
        parts.push(format!("{}: {}", i18n::CHIP_SEVERITY, sev.label()));
    }
    if parts.is_empty() || (from_section && app.filter_text.is_empty() && is_section_filter(app)) {
        return;
    }
    let text = parts.join(" · ");
    ui.add(egui::Label::new(RichText::new(&text).size(ty::CAPTION).color(p.accent)).truncate())
        .on_hover_text(&text);
    if ui
        .add(egui::Button::new(RichText::new(ph::CLOSE).color(p.weak)).frame(false))
        .on_hover_text(strs().clear)
        .clicked()
    {
        reset_filters(app);
    }
}

/// True when the live filters are exactly one section filter.
fn is_section_filter(app: &DesktopApp) -> bool {
    let q = &app.model.table.query;
    sections::SECTIONS.iter().any(|s| {
        s.filters
            .iter()
            .any(|(k, _)| sections::filter_params(k)["filters"] == serde_json::json!(q.filters))
    })
}

/// Fold or unfold the right «Обзор» panel.
fn overview_toggle(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let (icon, tip) = if app.view.overview_folded {
        (ph::CHEVRON_LEFT, i18n::OVERVIEW_SHOW)
    } else {
        (ph::RIGHT_PANEL_CLOSE, i18n::OVERVIEW_HIDE)
    };
    if ui
        .add(egui::Button::new(RichText::new(icon).size(ty::BODY).color(p.weak)).frame(false))
        .on_hover_text(tip)
        .clicked()
    {
        app.view.overview_folded = !app.view.overview_folded;
    }
}

/// Column-picker menu: checkboxes over the allow-list; the change goes
/// through the grid's layout commands so order, widths and persistence stay
/// in one place (CL-06/CL-07/CL-08).
fn columns_menu(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let btn = ui
        .add(
            egui::Button::new(RichText::new(ph::VIEW_COLUMN).size(ty::BODY).color(p.weak))
                .frame(false),
        )
        .on_hover_text(i18n::COLS_MENU);
    egui::Popup::menu(&btn).show(|ui| {
        ui.set_min_width(240.0);
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                for (name, _) in COLUMNS {
                    if *name == "url_id" {
                        continue;
                    }
                    let mut on = !app.model.grid.layout.is_hidden(name);
                    if ui.checkbox(&mut on, i18n::column_label(name)).changed() {
                        let method = if on {
                            "table.column.show"
                        } else {
                            "table.column.hide"
                        };
                        app.act(method, json!({"column": name}));
                    }
                }
            });
        ui.separator();
        if ui
            .add(
                egui::Button::new(RichText::new(i18n::GRID_MENU_CONFIGURE).size(ty::BODY))
                    .frame(false),
            )
            .clicked()
        {
            app.act("table.columns.configure", json!({"open": true}));
        }
    });
}

/// TSV export of the filtered set through `table.copy` (the disk export
/// dialog comes with the shell slice).
fn export_button(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    if ui
        .add(
            egui::Button::new(RichText::new(ph::DOWNLOAD).size(ty::BODY).color(p.weak))
                .frame(false),
        )
        .on_hover_text(i18n::TSV_HINT)
        .clicked()
    {
        if let Some(v) = app.act("table.copy", json!({"header": true})) {
            if let Some(tsv) = v.get("tsv").and_then(Value::as_str) {
                ui.ctx().copy_text(tsv.to_string());
                app.model.status = i18n::TSV_COPIED.into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> DesktopApp {
        DesktopApp::new(
            crate::app::AppModel::new(),
            std::sync::Arc::new(std::sync::Mutex::new(crate::ui::Effects::default())),
        )
    }

    #[test]
    fn condition_dialog_accepts_only_typed_values() {
        assert_eq!(typed_value(Kind::Int, "42"), Some(json!(42)));
        assert_eq!(typed_value(Kind::Int, "4.2"), None);
        assert_eq!(typed_value(Kind::Real, "0.5"), Some(json!(0.5)));
        assert_eq!(typed_value(Kind::Bool, "да"), Some(json!(true)));
        assert_eq!(typed_value(Kind::Bool, "maybe"), None);
        assert_eq!(typed_value(Kind::Text, "x"), Some(json!("x")));
    }

    #[test]
    fn in_operator_splits_values_and_between_needs_two() {
        let mut app = app();
        app.view.cond_col = "status_code".into();
        app.view.cond_op = "in".into();
        app.view.cond_val = "200, 301".into();
        assert_eq!(
            build_filter(&app),
            Some(json!({"column": "status_code", "op": "in", "value": [200, 301]}))
        );
        app.view.cond_op = "between".into();
        app.view.cond_val = "1".into();
        assert_eq!(build_filter(&app), None);
    }

    #[test]
    fn valueless_operators_skip_the_value() {
        let mut app = app();
        app.view.cond_col = "title".into();
        app.view.cond_op = "empty".into();
        app.view.cond_val.clear();
        assert_eq!(
            build_filter(&app),
            Some(json!({"column": "title", "op": "empty"}))
        );
    }
}
