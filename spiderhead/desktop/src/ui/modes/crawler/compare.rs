//! Scan comparison (canvas «Сравнение»): two scan pickers with a swap button,
//! the summary KPI cards of `compare.run`, then the tab/metric row list.
//! Fewer than two saved scans renders the canvas empty state.

use crate::ui::kit::icons as ph;
use eframe::egui::{self, vec2, RichText};
use egui_extras::{Column, TableBuilder};
use serde_json::{json, Value};

use crate::i18n::{self, strs};
use crate::theme::{space, ty, Palette};
use crate::ui::kit::{self, bold};
use crate::ui::DesktopApp;

/// Rows of one comparison tab page.
const ROW_PAGE: i64 = 50;

/// Draw the two-scan comparison screen.
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
    if app.view.compare_scans.is_none() {
        app.view.compare_scans = app.act("compare.scans", json!({}));
    }
    let scans = app
        .view
        .compare_scans
        .as_ref()
        .and_then(|v| v.get("scans"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    kit::page_header(ui, p, i18n::NAV_COMPARE, "");
    ui.add_space(space::MD);
    if scans.len() < 2 {
        kit::card(ui, p, |ui| {
            kit::empty_state(
                ui,
                p,
                ph::COMPARE_ARROWS,
                i18n::COMPARE_EMPTY,
                i18n::COMPARE_EMPTY_HINT,
                None,
            );
        });
        return;
    }
    kit::card(ui, p, |ui| pickers(ui, app, p, &scans));
    ui.add_space(space::MD);
    // Re-run the comparison whenever the visible slice moved: tab, metric or
    // the page offset. `compare.run` is SQL — once per change, not per frame.
    if let Some(v) = app.model.compare.result.clone() {
        let page = v.get("page").cloned().unwrap_or_default();
        let stale = page.get("tab").and_then(Value::as_str) != Some(&app.model.compare.tab)
            || page.get("metric").and_then(Value::as_str) != Some(&app.model.compare.metric)
            || page.get("offset").and_then(Value::as_i64) != Some(app.view.compare_offset);
        if stale {
            app.act(
                "compare.run",
                json!({"offset": app.view.compare_offset, "limit": ROW_PAGE}),
            );
        }
    }
    if let Some(v) = app.model.compare.result.clone() {
        summary(ui, app, p, &v);
        ui.add_space(space::MD);
        tab_rows(ui, app, p, &v);
    } else {
        kit::card(ui, p, |ui| {
            kit::empty_state(
                ui,
                p,
                ph::COMPARE_ARROWS,
                i18n::COMPARE_EMPTY,
                i18n::COMPARE_PICK,
                None,
            );
        });
    }
}

/// Picker row: «Было» / «Стало» combos, the swap button and «Сравнить».
fn pickers(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette, scans: &[Value]) {
    let label_of = |path: Option<&String>| -> String {
        path.and_then(|p| {
            scans
                .iter()
                .find(|s| s.get("path").and_then(Value::as_str) == Some(p.as_str()))
        })
        .map(scan_label)
        .unwrap_or_else(|| i18n::COMPARE_PICK.to_string())
    };
    ui.horizontal(|ui| {
        for (side, caption, current) in [
            (
                "before",
                i18n::COMPARE_BEFORE,
                app.model.compare.before.clone(),
            ),
            (
                "after",
                i18n::COMPARE_AFTER,
                app.model.compare.after.clone(),
            ),
        ] {
            ui.vertical(|ui| {
                ui.label(RichText::new(caption).color(p.weak).size(ty::CAPTION));
                let selected = label_of(current.as_ref());
                egui::ComboBox::from_id_salt(format!("cmp-{side}"))
                    .selected_text(RichText::new(selected).size(ty::BODY).color(p.text))
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        for s in scans {
                            let path = s.get("path").and_then(Value::as_str).unwrap_or("");
                            if ui
                                .selectable_label(current.as_deref() == Some(path), scan_label(s))
                                .clicked()
                            {
                                app.act("compare.pick", json!({"side": side, "path": path}));
                                app.view.compare_offset = 0;
                            }
                        }
                    });
            });
            if side == "before" {
                ui.add_space(space::SM);
                ui.vertical(|ui| {
                    ui.add_space(ty::CAPTION + 4.0);
                    if ui
                        .add(
                            egui::Button::new(RichText::new(ph::COMPARE_ARROWS).size(ty::BODY))
                                .min_size(vec2(36.0, 30.0)),
                        )
                        .on_hover_text(i18n::COMPARE_SWAP)
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        app.act("compare.swap", json!({}));
                        app.view.compare_offset = 0;
                    }
                });
                ui.add_space(space::SM);
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.vertical(|ui| {
                ui.add_space(ty::CAPTION + 4.0);
                let ready = app.model.compare.before.is_some() && app.model.compare.after.is_some();
                if ui
                    .add_enabled(
                        ready,
                        egui::Button::new(
                            bold(format!("{}  {}", ph::PLAY_ARROW, i18n::COMPARE_RUN))
                                .color(p.on_accent),
                        )
                        .fill(p.accent)
                        .min_size(vec2(120.0, 30.0)),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    app.view.compare_offset = 0;
                    app.act("compare.run", json!({"limit": ROW_PAGE}));
                }
            });
        });
    });
}

/// Picker row label: scan number, folder name, date.
fn scan_label(s: &Value) -> String {
    let number = s.get("number").and_then(Value::as_i64).unwrap_or(0);
    let label = s.get("label").and_then(Value::as_str).unwrap_or("");
    let date = s
        .get("finished_at")
        .and_then(Value::as_str)
        .map(|d| d.chars().take(16).collect::<String>())
        .unwrap_or_default();
    format!("№{number} · {label} · {date}")
}

/// Summary strip: partition note, config warning and the six KPI cards.
fn summary(ui: &mut egui::Ui, _app: &mut DesktopApp, p: &Palette, v: &Value) {
    let common = v.get("common").and_then(Value::as_i64).unwrap_or(0);
    let only_b = v.get("only_before").and_then(Value::as_i64).unwrap_or(0);
    let only_a = v.get("only_after").and_then(Value::as_i64).unwrap_or(0);
    ui.label(
        RichText::new(i18n::compare_note(common, only_b, only_a))
            .size(ty::CAPTION)
            .color(p.weak),
    );
    if v.get("config_warning").and_then(Value::as_bool) == Some(true) {
        ui.add_space(2.0);
        ui.label(
            RichText::new(format!("{}  {}", ph::WARNING, i18n::COMPARE_CONFIG_WARN))
                .size(ty::CAPTION)
                .color(p.warning),
        );
    }
    ui.add_space(space::SM);
    let kb = v.get("kpi_before").cloned().unwrap_or_default();
    let ka = v.get("kpi_after").cloned().unwrap_or_default();
    let kpi = |key: &str| -> (&Value, &Value) {
        (
            kb.get(key).unwrap_or(&Value::Null),
            ka.get(key).unwrap_or(&Value::Null),
        )
    };
    let gap = space::MD;
    let width = (ui.available_width() - gap * 5.0) / 6.0;
    let cards: [(&str, &str, &str); 6] = [
        (ph::NOTES, i18n::COMPARE_KPI_WORDS, "words"),
        (ph::LINK, i18n::COMPARE_KPI_INLINKS, "inlinks"),
        (ph::SCHEDULE, i18n::COMPARE_KPI_RESPONSE, "response"),
        (ph::TITLE, i18n::COMPARE_KPI_TITLE60, "title_over_60"),
        (ph::HUB, i18n::COMPARE_KPI_CANONICAL, "canonical"),
        (ph::HTTP, i18n::COMPARE_KPI_STATUS, "status"),
    ];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (icon, label, key) in cards {
            let (before, after) = match key {
                "canonical" => (
                    &Value::Null,
                    v.get("canonical_changed").unwrap_or(&Value::Null),
                ),
                "status" => (
                    &Value::Null,
                    v.get("status_changed").unwrap_or(&Value::Null),
                ),
                _ => kpi(key),
            };
            let (before_s, after_s) = match after.as_f64() {
                Some(a) => match before.as_f64() {
                    Some(b) => (fmt_num(b), fmt_num(a)),
                    None => (String::new(), kit::thousands(a as i64)),
                },
                None => (String::new(), "—".into()),
            };
            kpi_card(ui, p, width, icon, label, &before_s, &after_s);
        }
    });
}

/// Compact KPI card: icon, label and a before→after value pair (the arrow is
/// the icon font — the text faces carry no `→` glyph).
fn kpi_card(
    ui: &mut egui::Ui,
    p: &Palette,
    width: f32,
    icon: &str,
    label: &str,
    before: &str,
    after: &str,
) {
    kit::card(ui, p, |ui| {
        ui.set_width(width - 2.0 * space::MD);
        ui.horizontal(|ui| {
            ui.label(RichText::new(icon).size(15.0).color(p.accent));
            ui.label(RichText::new(label).size(ty::CAPTION).color(p.weak));
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if !before.is_empty() {
                ui.label(bold(before).size(ty::TITLE));
                ui.label(
                    RichText::new(ph::ARROW_FORWARD)
                        .size(ty::BODY)
                        .color(p.faint),
                );
            }
            ui.label(bold(after).size(ty::TITLE));
        });
    });
}

/// Number for a KPI card: integer when it is one, one decimal otherwise.
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        kit::thousands(v as i64)
    } else {
        format!("{v:.1}")
    }
}

/// Tab bar, metric chips of better/worse and the paged row table.
fn tab_rows(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette, v: &Value) {
    ui.horizontal(|ui| {
        for tab in crate::data::compare::TABS {
            let active = app.model.compare.tab == *tab;
            if kit::tab(ui, p, i18n::compare_tab_label(tab), active).clicked() {
                app.act("compare.tab", json!({"id": tab}));
                app.view.compare_offset = 0;
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ["better", "worse"].contains(&app.model.compare.tab.as_str()) {
                for metric in crate::data::compare::METRICS {
                    let active = app.model.compare.metric == *metric;
                    if kit::tab(ui, p, i18n::compare_metric_label(metric), active).clicked() {
                        app.act("compare.metric", json!({"id": metric}));
                        app.view.compare_offset = 0;
                    }
                }
                ui.label(
                    RichText::new(i18n::COMPARE_METRIC)
                        .size(ty::CAPTION)
                        .color(p.weak),
                );
            }
        });
    });
    ui.add_space(space::SM);
    let page = v.get("page").cloned().unwrap_or_default();
    let rows = page
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total = page.get("total").and_then(Value::as_i64).unwrap_or(0);
    let offset = page.get("offset").and_then(Value::as_i64).unwrap_or(0);
    let has_more = page
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tab = app.model.compare.tab.clone();
    let metric = app.model.compare.metric.clone();
    if rows.is_empty() {
        kit::card(ui, p, |ui| {
            kit::empty_state(ui, p, ph::DONE_ALL, i18n::compare_tab_label(&tab), "", None);
        });
        return;
    }
    let outcome = i18n::compare_outcome(&tab, &metric);
    let mut open_url = None;
    kit::card(ui, p, |ui| {
        let height = (ui.available_height() - 34.0).max(140.0);
        TableBuilder::new(ui)
            .striped(true)
            .max_scroll_height(height)
            .column(Column::remainder().at_least(300.0))
            .column(Column::initial(90.0))
            .column(Column::initial(90.0))
            .column(Column::initial(90.0))
            .column(Column::initial(130.0))
            .header(crate::theme::row::HEADER, |mut header| {
                for label in [
                    i18n::COL_URL,
                    i18n::COMPARE_COL_BEFORE,
                    i18n::COMPARE_COL_AFTER,
                    i18n::COMPARE_COL_DELTA,
                    i18n::COMPARE_COL_OUTCOME,
                ] {
                    header.col(|ui| {
                        ui.label(bold(label).size(ty::CAPTION).color(p.weak));
                    });
                }
            })
            .body(|body| {
                body.rows(crate::theme::row::COMPACT, rows.len(), |mut row_ui| {
                    let v = &rows[row_ui.index()];
                    row_ui.col(|ui| {
                        let url = v.get("url").and_then(Value::as_str).unwrap_or("");
                        let r = ui.add(
                            egui::Label::new(RichText::new(url).size(ty::BODY).color(p.text))
                                .truncate(),
                        );
                        if r.double_clicked() {
                            open_url = Some(url.to_string());
                        }
                        r.on_hover_text(i18n::CURL_TITLE);
                    });
                    for key in ["before", "after", "delta"] {
                        row_ui.col(|ui| {
                            let cell = v.get(key).cloned().unwrap_or(Value::Null);
                            let color = if key == "delta" { p.weak } else { p.text };
                            ui.label(
                                RichText::new(crate::ui::modes::format_cell(&cell))
                                    .size(ty::BODY)
                                    .color(color),
                            );
                        });
                    }
                    row_ui.col(|ui| {
                        ui.label(RichText::new(outcome).size(ty::CAPTION).color(p.weak));
                    });
                });
            });
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(i18n::issues_range(offset, rows.len() as i64, total))
                    .size(ty::CAPTION)
                    .color(p.weak),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let next = ui
                    .add_enabled(has_more, egui::Button::new(ph::ARROW_FORWARD).small())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if next.clicked() {
                    app.view.compare_offset = offset + ROW_PAGE;
                }
                let prev = ui
                    .add_enabled(offset > 0, egui::Button::new(ph::ARROW_BACK).small())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if prev.clicked() {
                    app.view.compare_offset = (offset - ROW_PAGE).max(0);
                }
            });
        });
    });
    if let Some(url) = open_url {
        app.act("compare.url", json!({"url": url}));
    }
}
