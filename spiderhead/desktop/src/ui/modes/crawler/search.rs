//! HTML content search (canvas «Поиск в HTML»): query field, scope chips and
//! options on top; a hit list with the selected document's occurrences on the
//! right. Unported/empty coverage renders the canvas unavailable state.

use crate::ui::kit::icons as ph;
use eframe::egui::{self, vec2, Align2, CornerRadius, FontId, Rect, RichText, Sense};
use serde_json::{json, Value};

use crate::i18n::{self, strs};
use crate::theme::{radius, semibold, space, ty, Palette};
use crate::ui::kit::{self, bold};
use crate::ui::DesktopApp;

/// Draw the search screen.
pub fn show(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let s = strs();
    if app.model.scan.is_none() {
        if kit::empty_state(
            ui,
            p,
            ph::FIND_IN_PAGE,
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
    kit::page_header(ui, p, i18n::NAV_SEARCH, "");
    ui.add_space(space::MD);
    kit::card(ui, p, |ui| query_bar(ui, app, p));
    ui.add_space(space::MD);
    results(ui, app, p);
}

/// Query row: the search field, scope chips and the option checkboxes.
fn query_bar(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    ui.label(
        RichText::new(i18n::SEARCH_QUERY)
            .color(p.weak)
            .size(ty::CAPTION),
    );
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let edit = egui::TextEdit::singleline(&mut app.view.search_text)
            .desired_width((ui.available_width() - 110.0).max(200.0))
            .margin(vec2(10.0, 8.0))
            .hint_text(format!("{}  {}", ph::SEARCH, i18n::SEARCH_QUERY));
        let field = ui.add(edit);
        let run = ui
            .add(
                egui::Button::new(
                    bold(format!("{}  {}", ph::MANAGE_SEARCH, i18n::SEARCH_RUN)).color(p.on_accent),
                )
                .fill(p.accent)
                .min_size(vec2(92.0, 34.0)),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (run.clicked() || enter) && !app.view.search_text.trim().is_empty() {
            app.act(
                "search.run",
                json!({"query": app.view.search_text.trim().to_string()}),
            );
        }
    });
    ui.add_space(space::SM);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for scope in crate::data::search::SCOPES {
            let active = app.model.search.scope == scope.id;
            if scope_chip(ui, p, i18n::search_scope_label(scope.id), active) {
                app.act("search.options", json!({"scope": scope.id}));
            }
        }
    });
    if app.model.search.scope == "css" {
        ui.add_space(space::SM);
        let response = ui.add(
            egui::TextEdit::singleline(&mut app.view.search_selector)
                .desired_width(f32::INFINITY)
                .margin(vec2(10.0, 6.0))
                .hint_text(format!("{}  {}", ph::CODE, i18n::SEARCH_SELECTOR)),
        );
        if response.changed() {
            app.act(
                "search.options",
                json!({"selector": app.view.search_selector.clone()}),
            );
        }
    }
    ui.add_space(space::SM);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::LG;
        let mut case = app.model.search.case_sensitive;
        if ui
            .checkbox(&mut case, RichText::new(i18n::SEARCH_CASE).size(ty::BODY))
            .changed()
        {
            app.act("search.options", json!({"case_sensitive": case}));
        }
        let mut only_200 = app.model.search.only_200;
        if ui
            .checkbox(
                &mut only_200,
                RichText::new(i18n::SEARCH_ONLY_200).size(ty::BODY),
            )
            .changed()
        {
            app.act("search.options", json!({"only_200": only_200}));
        }
    });
}

/// One scope chip of the canvas.
fn scope_chip(ui: &mut egui::Ui, p: &Palette, label: &str, active: bool) -> bool {
    let font = FontId::new(
        ty::CAPTION + 0.5,
        if active {
            semibold()
        } else {
            egui::FontFamily::Proportional
        },
    );
    let ink = if active { p.accent } else { p.weak };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, ink);
    let (rect, response) = ui.allocate_exact_size(galley.size() + vec2(22.0, 11.0), Sense::click());
    let fill = if active {
        p.accent_soft
    } else if response.hovered() {
        p.surface_hi
    } else {
        p.surface
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(13),
        fill,
        egui::Stroke::new(1.0_f32, if active { p.accent } else { p.border }),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, ink);
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// Below the query bar: idle, unavailable, no-hit states or the hit panes.
fn results(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let Some(v) = app.model.search.result.clone() else {
        kit::card(ui, p, |ui| {
            kit::empty_state(
                ui,
                p,
                ph::MANAGE_SEARCH,
                i18n::SEARCH_IDLE_TITLE,
                i18n::SEARCH_IDLE_HINT,
                None,
            );
        });
        return;
    };
    let measured = v
        .get("coverage")
        .and_then(|c| c.get("documents_measured"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let state = v
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unavailable");
    if measured == 0 || state == "unavailable" {
        let reason = v
            .get("coverage")
            .and_then(|c| c.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let (title, hint) = if reason.contains("representations") || reason.contains("retained") {
            (i18n::SEARCH_NO_HTML, i18n::SEARCH_NO_HTML_HINT)
        } else {
            (i18n::SEARCH_UNAVAILABLE, i18n::SEARCH_NO_HTML_HINT)
        };
        kit::card(ui, p, |ui| {
            kit::empty_state(ui, p, ph::WARNING, title, hint, None);
        });
        return;
    }
    let hits = v
        .get("hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let hits_total = v.get("hits_total").and_then(Value::as_i64).unwrap_or(0);
    if hits_total == 0 {
        kit::card(ui, p, |ui| {
            kit::empty_state(
                ui,
                p,
                ph::MANAGE_SEARCH,
                i18n::SEARCH_NONE,
                i18n::SEARCH_NONE_HINT,
                None,
            );
        });
        return;
    }
    // Coverage strip: honest denominators even when hits are capped.
    let selected_docs = v
        .get("coverage")
        .and_then(|c| c.get("documents_selected"))
        .and_then(Value::as_i64)
        .unwrap_or(measured);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "{}{} · {}",
                if state == "partial" {
                    i18n::SEARCH_PARTIAL
                } else {
                    ""
                },
                i18n::search_coverage(measured, selected_docs),
                i18n::search_hits(hits_total),
            ))
            .size(ty::CAPTION)
            .color(p.weak),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(format!("{}  {}", ph::CONTENT_COPY, i18n::SEARCH_COPY))
                            .size(ty::CAPTION),
                    )
                    .small(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                if let Some(out) = app.act("search.copy", json!({})) {
                    if let Some(tsv) = out.get("tsv").and_then(Value::as_str) {
                        ui.ctx().copy_text(tsv.to_string());
                    }
                }
            }
        });
    });
    ui.add_space(space::SM);
    egui::SidePanel::left("search-hits")
        .exact_width(420.0)
        .resizable(true)
        .frame(egui::Frame::NONE)
        .show_inside(ui, |ui| {
            hit_list(ui, app, p, &hits, hits_total);
        });
    ui.add_space(space::SM);
    preview(ui, app, p, &hits);
}

/// Left column: one row per document that contains the marker.
fn hit_list(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette, hits: &[Value], total: i64) {
    let selected = app.model.search.selected.min(hits.len().saturating_sub(1));
    let mut pick = None;
    egui::ScrollArea::vertical()
        .id_salt("search-hits-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, hit) in hits.iter().enumerate() {
                if hit_row(ui, p, hit, i == selected).clicked() {
                    pick = Some(i);
                }
            }
            if (total as usize) > hits.len() {
                ui.add_space(space::SM);
                ui.label(
                    RichText::new(format!("+{}", total - hits.len() as i64))
                        .size(ty::CAPTION)
                        .color(p.faint),
                );
            }
        });
    if let Some(i) = pick {
        app.act("search.select", json!({"hit": i, "match": 0}));
    }
}

/// One hit row: URL, status code and the first snippet line.
fn hit_row(ui: &mut egui::Ui, p: &Palette, hit: &Value, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::MD), p.accent_soft);
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::MD), p.surface_hi);
    }
    let code = hit.get("status_code").and_then(Value::as_i64);
    let count = hit
        .get("occurrence_count")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let text_rect = Rect::from_min_max(
        rect.left_top() + vec2(12.0, 7.0),
        rect.right_bottom() - vec2(58.0, 7.0),
    );
    let clip = ui.painter().with_clip_rect(text_rect);
    clip.text(
        text_rect.left_top(),
        Align2::LEFT_TOP,
        hit.get("url").and_then(Value::as_str).unwrap_or(""),
        FontId::new(
            ty::BODY,
            if selected {
                semibold()
            } else {
                egui::FontFamily::Proportional
            },
        ),
        p.text,
    );
    clip.text(
        text_rect.left_top() + vec2(0.0, 21.0),
        Align2::LEFT_TOP,
        hit.get("snippet").and_then(Value::as_str).unwrap_or(""),
        FontId::new(ty::CAPTION, egui::FontFamily::Monospace),
        p.faint,
    );
    ui.painter().text(
        rect.right_top() + vec2(-12.0, 9.0),
        Align2::RIGHT_TOP,
        format!(
            "{} · {}",
            code.map(|c| c.to_string()).unwrap_or("—".into()),
            count
        ),
        FontId::new(ty::CAPTION, egui::FontFamily::Proportional),
        kit::status_color(p, code),
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Right column: occurrence navigation and the context window of the hit.
fn preview(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette, hits: &[Value]) {
    let Some(hit) = hits.get(app.model.search.selected.min(hits.len().saturating_sub(1))) else {
        return;
    };
    let url = hit
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let occurrences = hit
        .get("occurrences")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let count = hit
        .get("occurrence_count")
        .and_then(Value::as_i64)
        .unwrap_or(occurrences.len() as i64);
    let occurrence = app.model.search.occurrence;
    kit::card(ui, p, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(RichText::new(&url).size(ty::BODY).color(p.text)).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(format!("{}  {}", ph::TABLE_VIEW, i18n::SEARCH_OPEN))
                                .size(ty::CAPTION),
                        )
                        .small(),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    app.act(
                        "table.filter",
                        json!({"filters": [{"column": "url", "op": "eq", "value": url}]}),
                    );
                }
            });
        });
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            let next = ui
                .add_enabled(
                    occurrence + 1 < occurrences.len(),
                    egui::Button::new(ph::ARROW_FORWARD).small(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if next.clicked() {
                app.act(
                    "search.select",
                    json!({"hit": app.model.search.selected, "match": occurrence + 1}),
                );
            }
            ui.label(
                RichText::new(i18n::search_match(occurrence + 1, count))
                    .size(ty::CAPTION)
                    .color(p.weak),
            );
            let prev = ui
                .add_enabled(occurrence > 0, egui::Button::new(ph::ARROW_BACK).small())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if prev.clicked() {
                app.act(
                    "search.select",
                    json!({"hit": app.model.search.selected, "match": occurrence - 1}),
                );
            }
        });
        ui.add_space(space::SM);
        let occ = occurrences.get(occurrence);
        // `window` is the store's context text — lines around `line`,
        // numbered from `window_start_line`.
        let window = occ
            .and_then(|o| o.get("window"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let line_no = occ
            .and_then(|o| o.get("line"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let first_line = occ
            .and_then(|o| o.get("window_start_line"))
            .and_then(Value::as_i64)
            .unwrap_or(line_no);
        egui::ScrollArea::vertical()
            .id_salt("search-window")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_min_height(ui.available_height());
                for (i, text) in window.lines().enumerate() {
                    let n = first_line + i as i64;
                    let hot = n == line_no;
                    let color = if hot { p.text } else { p.weak };
                    let family = if hot {
                        semibold()
                    } else {
                        egui::FontFamily::Monospace
                    };
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(n.to_string())
                                .monospace()
                                .size(ty::CAPTION)
                                .color(p.faint),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(text)
                                    .font(FontId::new(ty::BODY, family))
                                    .color(color),
                            )
                            .wrap(),
                        );
                    });
                }
            });
    });
}
