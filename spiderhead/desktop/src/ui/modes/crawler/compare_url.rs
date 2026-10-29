//! One-URL comparison (canvas «Сравнение URL»): page fields of the same URL
//! in scans №1 and №2 plus the gained/lost outlinks. An absent URL gets the
//! explicit empty state — never fabricated fields.

use crate::ui::kit::icons as ph;
use eframe::egui::{self, vec2, RichText};
use egui_extras::{Column, TableBuilder};
use serde_json::{json, Value};

use crate::i18n::{self, strs};
use crate::theme::{space, ty, Palette};
use crate::ui::kit::{self, bold};
use crate::ui::DesktopApp;

/// Draw the URL comparison screen.
pub fn show(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let s = strs();
    if app.model.scan.is_none() {
        if kit::empty_state(
            ui,
            p,
            ph::COMPARE_ARROWS,
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
    ui.horizontal(|ui| {
        if ui
            .add(
                egui::Button::new(
                    RichText::new(format!("{}  {}", ph::ARROW_BACK, i18n::CURL_BACK))
                        .size(ty::BODY),
                )
                .min_size(vec2(0.0, 30.0)),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            app.act("ui.navigate", json!({"id": "compare"}));
        }
        ui.add_space(space::SM);
        ui.vertical(|ui| {
            ui.label(bold(i18n::CURL_TITLE).size(ty::HEADING));
            let url = app.model.compare.url.clone();
            if !url.is_empty() {
                ui.label(RichText::new(url).size(ty::CAPTION).color(p.weak));
            }
        });
    });
    ui.add_space(space::MD);
    let Some(v) = app.model.compare.url_result.clone() else {
        kit::card(ui, p, |ui| {
            kit::empty_state(
                ui,
                p,
                ph::COMPARE_ARROWS,
                i18n::CURL_TITLE,
                i18n::COMPARE_PICK,
                None,
            );
        });
        return;
    };
    if v.get("state").and_then(Value::as_str) == Some("absent") {
        kit::card(ui, p, |ui| {
            kit::empty_state(ui, p, ph::LINK_OFF, i18n::CURL_ABSENT, "", None);
        });
        return;
    }
    let fields = v
        .get("fields")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    kit::card(ui, p, |ui| {
        let width = ui.available_width();
        let height = (ui.available_height() - 8.0).max(200.0);
        TableBuilder::new(ui)
            .striped(true)
            .max_scroll_height(height)
            .column(Column::initial((width * 0.22).max(160.0)))
            .column(Column::remainder())
            .column(Column::remainder())
            .column(Column::initial(90.0))
            .column(Column::initial(110.0))
            .header(crate::theme::row::HEADER, |mut header| {
                for label in [
                    i18n::CURL_COL_FIELD,
                    i18n::COMPARE_BEFORE,
                    i18n::COMPARE_AFTER,
                    i18n::COMPARE_COL_DELTA,
                    "",
                ] {
                    header.col(|ui| {
                        ui.label(bold(label).size(ty::CAPTION).color(p.weak));
                    });
                }
            })
            .body(|body| {
                body.rows(crate::theme::row::COMPACT, fields.len(), |mut row_ui| {
                    let f = &fields[row_ui.index()];
                    let state = f.get("state").and_then(Value::as_str).unwrap_or("same");
                    row_ui.col(|ui| {
                        let field = f.get("field").and_then(Value::as_str).unwrap_or("");
                        ui.label(
                            RichText::new(i18n::compare_field_label(field))
                                .size(ty::BODY)
                                .color(p.weak),
                        );
                    });
                    for key in ["before", "after"] {
                        row_ui.col(|ui| {
                            let cell = f.get(key).cloned().unwrap_or(Value::Null);
                            let absent = cell.is_null()
                                && (state == "added" && key == "before"
                                    || state == "removed" && key == "after");
                            let ink = if absent { p.faint } else { p.text };
                            ui.add(
                                egui::Label::new(
                                    RichText::new(crate::ui::modes::format_cell(&cell))
                                        .size(ty::BODY)
                                        .color(ink),
                                )
                                .truncate(),
                            );
                        });
                    }
                    row_ui.col(|ui| {
                        ui.label(
                            RichText::new(crate::ui::modes::format_cell(
                                f.get("delta").unwrap_or(&Value::Null),
                            ))
                            .size(ty::BODY)
                            .color(p.weak),
                        );
                    });
                    row_ui.col(|ui| {
                        let ink = match state {
                            "changed" | "added" | "removed" => p.accent,
                            _ => p.faint,
                        };
                        ui.label(
                            RichText::new(i18n::curl_state(state))
                                .size(ty::CAPTION)
                                .color(ink),
                        );
                    });
                });
            });
    });
    ui.add_space(space::MD);
    links_diff(ui, p, &v);
}

/// Gained and lost outlinks of the URL, two columns under the field table.
fn links_diff(ui: &mut egui::Ui, p: &Palette, v: &Value) {
    let added = v
        .get("links_added")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let removed = v
        .get("links_removed")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if added.is_empty() && removed.is_empty() {
        return;
    }
    ui.columns(2, |cols| {
        for (col, (title, list, icon)) in cols.iter_mut().zip([
            (i18n::CURL_LINKS_ADDED, added, ph::ADD_LINK),
            (i18n::CURL_LINKS_REMOVED, removed, ph::LINK_OFF),
        ]) {
            kit::card(col, p, |ui| {
                ui.label(
                    bold(format!("{}  {title} ({})", icon, list.len()))
                        .size(ty::CAPTION)
                        .color(p.weak),
                );
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for link in &list {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(link.as_str().unwrap_or(""))
                                        .size(ty::CAPTION)
                                        .color(p.text),
                                )
                                .truncate(),
                            );
                        }
                    });
            });
        }
    });
}
