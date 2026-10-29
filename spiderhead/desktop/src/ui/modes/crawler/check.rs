//! «Проверка страницы»: look one address up in the open scan (`simple.check`).
//! A crawler-mode section — drawn inside the shell, no separate chrome.

use crate::ui::kit::icons as ph;
use eframe::egui::{self, vec2, RichText};
use serde_json::{json, Value};

use crate::i18n::{self, strs};
use crate::theme::{space, Palette};
use crate::ui::kit::{self, bold};
use crate::ui::modes::format_cell;
use crate::ui::DesktopApp;

/// Draw the check page into the content panel.
pub fn show(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let s = strs();
    kit::page_header(ui, p, i18n::NAV_CHECK, "");
    ui.add_space(space::LG);
    ui.vertical_centered(|ui| {
        ui.set_max_width(640.0);
        ui.add_space(space::XL);
        if app.model.scan.is_none() {
            if let Some(r) = kit::empty_state(
                ui,
                p,
                ph::FACT_CHECK,
                i18n::TABLE_EMPTY_TITLE,
                i18n::SIMPLE_NO_SCAN,
                Some(i18n::EMPTY_OPEN_SCAN),
            ) {
                if r.clicked() {
                    app.act("scan.open", json!({"dialog": true}));
                }
            }
            return;
        }
        ui.label(RichText::new(s.simple_hint).color(p.weak));
        ui.add_space(space::LG);
        ui.horizontal(|ui| {
            let edit = egui::TextEdit::singleline(&mut app.view.simple_input)
                .desired_width(ui.available_width() - 120.0)
                .margin(vec2(12.0, 9.0))
                .hint_text(format!("{}  {}", ph::SEARCH, i18n::SIMPLE_URL_LABEL));
            let response = ui.add(edit);
            let go = ui.add(
                egui::Button::new(bold(i18n::SIMPLE_START).color(p.on_accent))
                    .fill(p.accent)
                    .min_size(vec2(108.0, 34.0)),
            );
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let url = app.view.simple_input.trim().to_string();
            if (go.clicked() || enter) && !url.is_empty() {
                app.act("simple.check", json!({"url": url}));
            }
        });
        ui.add_space(space::LG);
        if let Some(result) = app.model.simple_result.clone() {
            result_card(ui, app, p, &result);
        }
    });
}

/// The `simple.check` reply as a card.
fn result_card(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette, result: &Value) {
    match result.get("state").and_then(Value::as_str) {
        Some("found") => {
            let row = result.get("row").cloned().unwrap_or(Value::Null);
            kit::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                egui::Grid::new("simple-result")
                    .num_columns(2)
                    .spacing(vec2(space::LG, 7.0))
                    .show(ui, |ui| {
                        for key in [
                            "url",
                            "status_code",
                            "content_type",
                            "title",
                            "indexable",
                            "crawl_depth",
                            "word_count",
                            "size_bytes",
                            "response_time",
                        ] {
                            let Some(val) = row.get(key) else { continue };
                            ui.label(RichText::new(i18n::field_label(key)).color(p.weak));
                            if key == "status_code" {
                                let color = kit::status_color(p, val.as_i64());
                                ui.label(bold(format_cell(val)).color(color));
                            } else {
                                ui.add(egui::Label::new(format_cell(val)).wrap());
                            }
                            ui.end_row();
                        }
                    });
                if let Some(id) = row.get("url_id").and_then(Value::as_i64) {
                    ui.add_space(space::MD);
                    if ui
                        .button(format!("{}  {}", ph::TABLE_VIEW, i18n::SIMPLE_OPEN_TABLE))
                        .clicked()
                    {
                        app.act("ui.navigate", json!({"id": "urls"}));
                        app.act("table.select", json!({"url_id": id}));
                    }
                }
            });
        }
        Some("similar") => {
            ui.label(RichText::new(i18n::SIMPLE_SIMILAR).color(p.weak));
            if let Some(rows) = result.get("rows").and_then(Value::as_array) {
                for row in rows.iter().take(5) {
                    let url = row.get("url").and_then(Value::as_str).unwrap_or("");
                    if ui.link(url).clicked() {
                        app.view.simple_input = url.to_string();
                        app.act("simple.check", json!({"url": url}));
                    }
                }
            }
        }
        Some("not_found") => {
            ui.label(RichText::new(i18n::SIMPLE_NOT_FOUND).color(p.weak));
        }
        Some("no_scan") => {
            ui.label(RichText::new(i18n::SIMPLE_NO_SCAN).color(p.weak));
        }
        _ => {
            ui.label(RichText::new(&app.model.status).color(p.weak));
        }
    }
}
