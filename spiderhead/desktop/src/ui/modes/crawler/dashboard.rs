//! Issues screen (canvas «Проблемы»): a fixed-width check list on the left —
//! search box, severity chips and checks grouped by category — and the
//! selected check's detail plus its affected-URL table on the right.

use crate::ui::kit::icons as ph;
use eframe::egui::{self, vec2, Align2, CornerRadius, FontId, Rect, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use serde_json::{json, Value};

use crate::i18n::{self, strs};
use crate::theme::{radius, semibold, space, ty, Palette};
use crate::ui::checks;
use crate::ui::kit::{self, bold, Severity};
use crate::ui::DesktopApp;

/// Rows of the right-pane URL table at once (`issues.urls` limit).
const ISSUE_PAGE: i64 = 100;

/// One check of the audit, flattened from `dashboard.inspect`.
struct Row {
    id: String,
    severity: Severity,
    state: String,
    findings: i64,
    suppressed: i64,
    reason: String,
}

impl Row {
    fn from_json(v: &Value) -> Option<Self> {
        Some(Self {
            id: v.get("id")?.as_str()?.to_string(),
            severity: Severity::parse(v.get("severity")?.as_str()?)?,
            state: v.get("state").and_then(Value::as_str).unwrap_or("").into(),
            findings: v.get("findings").and_then(Value::as_i64).unwrap_or(0),
            suppressed: v
                .get("suppressed_findings")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            reason: v.get("reason").and_then(Value::as_str).unwrap_or("").into(),
        })
    }

    fn fired(&self) -> bool {
        self.state == "fired" && self.findings > 0
    }
}

/// Draw the issues screen.
pub fn show(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let s = strs();
    if app.model.scan.is_none() {
        if kit::empty_state(
            ui,
            p,
            ph::RULE,
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
    let Some(v) = app.act("dashboard.inspect", json!({})) else {
        return;
    };
    if v.get("state").and_then(Value::as_str) != Some("available") {
        kit::empty_state(
            ui,
            p,
            ph::WARNING,
            s.no_findings,
            i18n::DASH_NO_AUDIT_HINT,
            None,
        );
        return;
    }
    let rows: Vec<Row> = v
        .get("checks")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Row::from_json).collect())
        .unwrap_or_default();
    if rows.iter().all(|r| !r.fired()) {
        kit::empty_state(
            ui,
            p,
            ph::RULE,
            i18n::ISSUES_EMPTY,
            i18n::ISSUES_EMPTY_HINT,
            None,
        );
        return;
    }
    if app.model.dashboard_selected.is_none() {
        if let Some(first) = rows.iter().find(|r| r.fired()) {
            app.act("dashboard.select", json!({"check": first.id}));
            app.view.issues_offset = 0;
        }
    }
    let selected = app.model.dashboard_selected.clone();
    let row = selected
        .as_ref()
        .and_then(|id| rows.iter().find(|r| &r.id == id));
    // Keep the URL page of the selected check materialized; `issues.urls`
    // caches `(check, offset)` inside the model, so this runs once per change.
    if let Some(id) = &selected {
        let want = (id.clone(), app.view.issues_offset);
        let have = app
            .model
            .issue_page
            .as_ref()
            .map(|(c, o, _)| (c.clone(), *o));
        if have != Some(want) {
            app.act(
                "issues.urls",
                json!({"check": id, "offset": app.view.issues_offset, "limit": ISSUE_PAGE}),
            );
        }
    }
    let page = app.model.issue_page.clone().map(|(_, _, p)| p);

    egui::SidePanel::left("issues-list")
        .exact_width(340.0)
        .resizable(false)
        .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 0)))
        .show_inside(ui, |ui| {
            ui.add_space(space::SM);
            ui.horizontal(|ui| {
                ui.add_space(space::MD);
                ui.vertical(|ui| {
                    ui.set_width(308.0);
                    list(ui, app, p, &v, &rows, selected.as_deref());
                });
            });
        });
    ui.add_space(space::SM);
    ui.horizontal(|ui| {
        ui.add_space(space::MD);
        ui.vertical(|ui| {
            ui.set_width(ui.available_width() - space::MD);
            detail(ui, app, p, row);
            ui.add_space(space::MD);
            url_table(ui, app, p, row, page.as_ref());
        });
    });
}

/// Left pane: search box, severity chips, fired checks grouped by category.
fn list(
    ui: &mut egui::Ui,
    app: &mut DesktopApp,
    p: &Palette,
    v: &Value,
    rows: &[Row],
    selected: Option<&str>,
) {
    ui.add(
        egui::TextEdit::singleline(&mut app.view.dash_search)
            .desired_width(f32::INFINITY)
            .margin(vec2(10.0, 6.0))
            .hint_text(format!("{}  {}", ph::SEARCH, i18n::DASH_SEARCH)),
    );
    ui.add_space(space::SM);
    severity_chips(ui, app, p, v, rows);
    ui.add_space(space::SM);
    let needle = app.view.dash_search.to_lowercase();
    let sev_filter = app.model.issues_severity.clone();
    let matches = |r: &Row| {
        (needle.is_empty()
            || r.id.to_lowercase().contains(&needle)
            || checks::info(&r.id).title.to_lowercase().contains(&needle))
            && (sev_filter.is_empty() || r.severity.id() == sev_filter)
    };
    let mut groups: Vec<(&'static str, Vec<&Row>)> = Vec::new();
    for r in rows.iter().filter(|r| r.fired() && matches(r)) {
        let cat = checks::category(&r.id);
        match groups.iter_mut().find(|(c, _)| *c == cat) {
            Some((_, list)) => list.push(r),
            None => groups.push((cat, vec![r])),
        }
    }
    let urgency = |l: &Vec<&Row>| l.iter().map(|r| r.severity as usize).min().unwrap_or(9);
    groups.sort_by_key(|(_, l)| {
        (
            urgency(l),
            std::cmp::Reverse(l.iter().map(|r| r.findings).sum::<i64>()),
        )
    });
    let mut pick = None;
    egui::ScrollArea::vertical()
        .id_salt("dashboard-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (cat, mut list) in groups {
                list.sort_by_key(|r| (r.severity as usize, std::cmp::Reverse(r.findings)));
                let total: i64 = list.iter().map(|r| r.findings).sum();
                ui.horizontal(|ui| {
                    kit::caption(ui, p, cat);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(kit::thousands(total))
                                .color(p.faint)
                                .size(ty::CAPTION),
                        );
                    });
                });
                for r in list {
                    if check_row(ui, p, r, selected == Some(r.id.as_str())).clicked() {
                        pick = Some(r.id.clone());
                    }
                }
                ui.add_space(space::MD);
            }
            let quiet: Vec<&Row> = rows.iter().filter(|r| !r.fired() && matches(r)).collect();
            if !quiet.is_empty() {
                egui::CollapsingHeader::new(
                    RichText::new(i18n::dash_quiet(quiet.len())).color(p.weak),
                )
                .id_salt("quiet-checks")
                .show(ui, |ui| {
                    for r in quiet {
                        if check_row(ui, p, r, selected == Some(r.id.as_str())).clicked() {
                            pick = Some(r.id.clone());
                        }
                    }
                });
            }
        });
    if let Some(id) = pick {
        app.act("dashboard.select", json!({"check": id}));
        app.view.issues_offset = 0;
    }
}

/// Severity chip row of the canvas list: «Все» plus one chip per severity,
/// counting only fired findings.
fn severity_chips(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette, v: &Value, rows: &[Row]) {
    let by = v.get("by_severity").and_then(Value::as_object);
    let active = app.model.issues_severity.clone();
    let mut click = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        let total: i64 = rows.iter().filter(|r| r.fired()).map(|r| r.findings).sum();
        if sev_chip(
            ui,
            p,
            ph::APPS,
            i18n::ISSUES_ALL,
            total,
            p.accent,
            active.is_empty(),
        ) {
            click = Some(String::new());
        }
        for sev in Severity::ALL {
            let count = by
                .and_then(|m| m.get(sev.id()))
                .and_then(Value::as_i64)
                .unwrap_or_else(|| {
                    rows.iter()
                        .filter(|r| r.severity == sev && r.fired())
                        .map(|r| r.findings)
                        .sum()
                });
            if sev_chip(
                ui,
                p,
                sev.icon(),
                sev.label(),
                count,
                sev.color(p),
                active == sev.id(),
            ) {
                click = Some(sev.id().to_string());
            }
        }
    });
    if let Some(id) = click {
        app.act("issues.severity", json!({"id": id}));
    }
}

/// One toggle chip: icon, label and finding count in a tinted pill.
fn sev_chip(
    ui: &mut egui::Ui,
    p: &Palette,
    icon: &str,
    label: &str,
    count: i64,
    color: egui::Color32,
    active: bool,
) -> bool {
    let font = FontId::new(ty::CAPTION + 0.5, egui::FontFamily::Proportional);
    let text = format!("{icon}  {label} · {}", kit::thousands(count));
    let ink = if active { color } else { p.weak };
    let galley = ui.painter().layout_no_wrap(text, font.clone(), ink);
    let (rect, response) = ui.allocate_exact_size(galley.size() + vec2(20.0, 10.0), Sense::click());
    let fill = if active {
        Palette::tint(color, 40)
    } else if response.hovered() {
        p.surface_hi
    } else {
        Palette::tint(color, 20)
    };
    let stroke = if active {
        egui::Stroke::new(1.0_f32, Palette::tint(color, 100))
    } else {
        egui::Stroke::new(1.0_f32, p.border)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(12),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, ink);
    let _ = font;
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// One check line: severity icon, readable title, code, finding count.
fn check_row(ui: &mut egui::Ui, p: &Palette, r: &Row, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
    let fired = r.fired();
    let color = if fired { r.severity.color(p) } else { p.faint };
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::MD), p.accent_soft);
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::MD), p.surface_hi);
    }
    ui.painter().text(
        rect.left_center() + vec2(14.0, 0.0),
        Align2::LEFT_CENTER,
        if fired {
            r.severity.icon()
        } else {
            ph::RADIO_BUTTON_UNCHECKED
        },
        FontId::new(17.0, egui::FontFamily::Proportional),
        color,
    );
    let right = if fired { 64.0 } else { 12.0 };
    let text_rect = Rect::from_min_max(
        rect.left_top() + vec2(40.0, 6.0),
        rect.right_bottom() - vec2(right, 6.0),
    );
    let info = checks::info(&r.id);
    let ink = if fired { p.text } else { p.weak };
    let clip = ui.painter().with_clip_rect(text_rect);
    clip.text(
        text_rect.left_top() + vec2(0.0, 1.0),
        Align2::LEFT_TOP,
        &info.title,
        FontId::new(
            ty::BODY,
            if selected {
                semibold()
            } else {
                egui::FontFamily::Proportional
            },
        ),
        ink,
    );
    clip.text(
        text_rect.left_top() + vec2(0.0, 21.0),
        Align2::LEFT_TOP,
        &r.id,
        FontId::new(ty::CAPTION - 1.0, egui::FontFamily::Monospace),
        p.faint,
    );
    if fired {
        let text = kit::thousands(r.findings);
        let galley =
            ui.painter()
                .layout_no_wrap(text, FontId::new(ty::CAPTION + 0.5, semibold()), color);
        let size = vec2(galley.size().x + 14.0, 20.0);
        let pill = Rect::from_min_size(
            egui::pos2(rect.right() - size.x - 12.0, rect.center().y - size.y / 2.0),
            size,
        );
        ui.painter()
            .rect_filled(pill, CornerRadius::same(10), Palette::tint(color, 38));
        ui.painter()
            .galley(pill.center() - galley.size() / 2.0, galley, color);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Right pane header: what the selected check says and the way into the table.
fn detail(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette, row: Option<&Row>) {
    let Some(r) = row else {
        kit::empty_state(ui, p, ph::INFO, i18n::DASH_PICK, "", None);
        return;
    };
    let info = checks::info(&r.id);
    ui.horizontal(|ui| {
        kit::severity_badge(ui, p, r.severity);
        kit::badge(ui, p.weak, checks::category(&r.id));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if r.fired() {
                let button = egui::Button::new(
                    bold(format!("{}  {}", ph::TABLE_VIEW, i18n::DASH_OPEN_URLS))
                        .color(p.on_accent),
                )
                .fill(p.accent)
                .min_size(vec2(0.0, 30.0));
                if ui
                    .add(button)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    app.act("dashboard.filter", json!({"issue_check": r.id}));
                }
            }
        });
    });
    ui.add_space(space::SM);
    ui.add(egui::Label::new(bold(&info.title).size(ty::TITLE)).wrap());
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(&r.id)
                .monospace()
                .size(ty::CAPTION)
                .color(p.faint),
        );
        let color = if r.fired() {
            r.severity.color(p)
        } else {
            p.faint
        };
        ui.label(
            bold(format!(
                "{} {}",
                kit::thousands(r.findings),
                i18n::findings_noun(r.findings)
            ))
            .size(ty::CAPTION)
            .color(color),
        );
        if r.suppressed > 0 {
            ui.label(
                RichText::new(i18n::dash_suppressed(r.suppressed))
                    .size(ty::CAPTION)
                    .color(p.faint),
            );
        }
    });
    if let Some(fix) = info.fix.as_deref().filter(|_| r.fired()) {
        ui.add_space(space::XS);
        ui.add(egui::Label::new(RichText::new(fix).color(p.weak).size(ty::CAPTION)).wrap());
    }
    if !r.fired() {
        ui.add_space(space::XS);
        let why = if r.reason.is_empty() {
            check_state(&r.state)
        } else {
            r.reason.as_str()
        };
        ui.add(egui::Label::new(
            RichText::new(format!("{}: {why}", i18n::DASH_WHY))
                .color(p.weak)
                .size(ty::CAPTION),
        ));
    }
}

/// Right pane body: the affected-URL table of the selected check with pager.
fn url_table(
    ui: &mut egui::Ui,
    app: &mut DesktopApp,
    p: &Palette,
    row: Option<&Row>,
    page: Option<&Value>,
) {
    let Some(r) = row else { return };
    let state = page
        .and_then(|v| v.get("state"))
        .and_then(Value::as_str)
        .unwrap_or("");
    match state {
        "unmeasured" => {
            let reason = page
                .and_then(|v| v.get("reason"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let hint = if reason.is_empty() {
                i18n::ISSUES_UNMEASURED_HINT.to_string()
            } else {
                format!("{} ({reason})", i18n::ISSUES_UNMEASURED_HINT)
            };
            kit::card(ui, p, |ui| {
                kit::empty_state(ui, p, ph::INFO, i18n::ISSUES_UNMEASURED, &hint, None);
            });
        }
        "unavailable" => {
            kit::card(ui, p, |ui| {
                kit::empty_state(
                    ui,
                    p,
                    ph::WARNING,
                    i18n::ISSUES_NO_URLS,
                    i18n::DASH_NO_AUDIT_HINT,
                    None,
                );
            });
        }
        _ => {
            let total = page
                .and_then(|v| v.get("total"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if total == 0 {
                kit::card(ui, p, |ui| {
                    kit::empty_state(ui, p, ph::INFO, i18n::ISSUES_NO_URLS, "", None);
                });
                return;
            }
            let rows = page
                .and_then(|v| v.get("rows"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let offset = page
                .and_then(|v| v.get("offset"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let has_more = page
                .and_then(|v| v.get("has_more"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let check = r.id.clone();
            let mut open_url = None;
            kit::card(ui, p, |ui| {
                let height = (ui.available_height() - 34.0).max(120.0);
                TableBuilder::new(ui)
                    .striped(true)
                    .max_scroll_height(height)
                    .column(Column::remainder().at_least(280.0))
                    .column(Column::initial(72.0))
                    .column(Column::remainder().at_least(160.0))
                    .header(crate::theme::row::HEADER, |mut header| {
                        for label in [i18n::COL_URL, i18n::COL_STATUS, i18n::ISSUES_COL_EVIDENCE] {
                            header.col(|ui| {
                                ui.label(bold(label).size(ty::CAPTION).color(p.weak));
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(crate::theme::row::COMPACT, rows.len(), |mut row_ui| {
                            let v = &rows[row_ui.index()];
                            row_ui.col(|ui| {
                                let url = v
                                    .get("url")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                let r = ui.add(
                                    egui::Label::new(
                                        RichText::new(&url).color(p.text).size(ty::BODY),
                                    )
                                    .truncate(),
                                );
                                if r.double_clicked() {
                                    open_url = Some(url);
                                }
                                r.on_hover_text(i18n::DASH_OPEN_URLS);
                            });
                            row_ui.col(|ui| {
                                let code = v.get("status_code").and_then(Value::as_i64);
                                ui.label(
                                    RichText::new(
                                        code.map(|c| c.to_string()).unwrap_or_else(|| "—".into()),
                                    )
                                    .color(kit::status_color(p, code))
                                    .size(ty::BODY),
                                );
                            });
                            row_ui.col(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(
                                            v.get("evidence").and_then(Value::as_str).unwrap_or(""),
                                        )
                                        .color(p.weak)
                                        .size(ty::CAPTION),
                                    )
                                    .truncate(),
                                );
                            });
                        });
                    });
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(i18n::issues_footer(total, &check))
                            .size(ty::CAPTION)
                            .color(p.faint),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let next = ui
                            .add_enabled(has_more, egui::Button::new(ph::ARROW_FORWARD).small())
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if next.clicked() {
                            app.view.issues_offset = offset + ISSUE_PAGE;
                        }
                        ui.label(
                            RichText::new(i18n::issues_range(offset, rows.len() as i64, total))
                                .size(ty::CAPTION)
                                .color(p.weak),
                        );
                        let prev = ui
                            .add_enabled(offset > 0, egui::Button::new(ph::ARROW_BACK).small())
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if prev.clicked() {
                            app.view.issues_offset = (offset - ISSUE_PAGE).max(0);
                        }
                    });
                });
            });
            if let Some(url) = open_url {
                app.act(
                    "table.filter",
                    json!({"filters": [{"column": "url", "op": "eq", "value": url}]}),
                );
            }
        }
    }
}

/// Plain-words state of a check that has no findings.
fn check_state(state: &str) -> &str {
    match state {
        "silent" => i18n::DASH_STATE_SILENT,
        "skipped" => i18n::DASH_STATE_SKIPPED,
        "disabled" => i18n::DASH_STATE_DISABLED,
        other => other,
    }
}
