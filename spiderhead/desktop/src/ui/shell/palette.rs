//! ⌘K: one list of navigation targets and actions, filtered by typing. Every
//! entry is a control command, executed through `DesktopApp::act`.

use eframe::egui::{self, pos2, vec2, Align2, CornerRadius, FontId, Key, Rect, Sense};
use serde_json::{json, Value};

use crate::app::{Mode, Nav};
use crate::i18n::{self, strs};
use crate::theme::{radius, semibold, ty, Palette, Theme};
use crate::ui::kit;
use crate::ui::kit::icons as ph;
use crate::ui::DesktopApp;

struct Entry {
    group: &'static str,
    icon: &'static str,
    label: String,
    method: &'static str,
    params: Value,
}

fn entries(app: &DesktopApp) -> Vec<Entry> {
    let s = strs();
    let nav = |icon, label: &str, id: &str| Entry {
        group: i18n::PALETTE_NAV,
        icon,
        label: label.to_string(),
        method: "ui.navigate",
        params: json!({"id": id}),
    };
    let act = |icon, label: String, method, params| Entry {
        group: i18n::PALETTE_ACTIONS,
        icon,
        label,
        method,
        params,
    };
    // Navigation entries follow the product mode: each mode shows only its
    // own sections, matching the side navigation.
    let navs: &[(Nav, &str, &str)] = match app.model.mode {
        Some(Mode::Logs) => &[
            (Nav::Logs, ph::DESCRIPTION, i18n::NAV_LOGS),
            (Nav::LogsBots, ph::SMART_TOY, i18n::NAV_LOGS_BOTS),
            (Nav::LogsCompare, ph::DIFFERENCE, i18n::NAV_LOGS_X),
        ],
        Some(Mode::Agent) => &[
            (Nav::Work, ph::CHECKLIST, i18n::NAV_WORK),
            (Nav::Inbox, ph::INBOX, s.nav_inbox),
            (Nav::Projects, ph::WORKSPACES, s.nav_projects),
            (Nav::Urls, ph::TABLE_VIEW, s.nav_urls),
            (Nav::Dashboard, ph::RULE, i18n::NAV_ISSUES),
            (Nav::Compare, ph::COMPARE_ARROWS, i18n::NAV_COMPARE),
            (Nav::Search, ph::FIND_IN_PAGE, i18n::NAV_SEARCH),
        ],
        _ => &[
            (Nav::Crawler, ph::TRAVEL_EXPLORE, i18n::NAV_CRAWLER),
            (Nav::Urls, ph::TABLE_VIEW, s.nav_urls),
            (Nav::Dashboard, ph::RULE, i18n::NAV_ISSUES),
            (Nav::Compare, ph::COMPARE_ARROWS, i18n::NAV_COMPARE),
            (Nav::Search, ph::FIND_IN_PAGE, i18n::NAV_SEARCH),
            (Nav::Check, ph::FACT_CHECK, i18n::NAV_CHECK),
        ],
    };
    let mut list: Vec<Entry> = navs
        .iter()
        .map(|(id, icon, label)| nav(*icon, label, id.name()))
        .collect();
    for theme in Theme::ALL {
        list.push(act(
            ph::PALETTE,
            i18n::menu_theme(i18n::theme_label(theme)),
            "ui.theme",
            json!({"mode": theme.name()}),
        ));
    }
    // Mode switching is an action, like the Переход menu and the start cards.
    for mode in Mode::ALL {
        if app.model.mode == Some(mode) {
            continue;
        }
        list.push(act(
            ph::SWAP_HORIZ,
            format!("Перейти в режим «{}»", i18n::mode_label(mode.name())),
            "ui.mode",
            json!({"mode": mode.name()}),
        ));
    }
    if app.model.mode.is_some() {
        list.push(act(
            ph::APPS,
            i18n::MODE_START.to_string(),
            "ui.mode",
            json!({"mode": "start"}),
        ));
    }
    for (icon, label, id) in [
        (ph::SETTINGS, s.settings, "settings"),
        (ph::HUB, s.mcp, "mcp"),
        (ph::TERMINAL, s.cli_path, "cli"),
        (ph::HELP, s.help, "help"),
    ] {
        list.push(act(icon, label.to_string(), "ui.window", json!({"id": id})));
    }
    if app.model.scan.is_some() {
        list.push(act(
            ph::CLOSE,
            "Закрыть скан".into(),
            "scan.close",
            json!({}),
        ));
    }
    list
}

/// Draw the palette over the window.
pub fn show(ctx: &egui::Context, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    let screen = ctx.content_rect();
    // Scrim: a click outside the card closes the palette.
    let scrim = egui::Area::new(egui::Id::new("palette-scrim"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter().rect_filled(
                rect,
                0.0,
                egui::Color32::from_black_alpha(if p.theme.is_dark() { 150 } else { 80 }),
            );
            resp
        });
    let all = entries(app);
    let query = app.view.palette_query.to_lowercase();
    let shown: Vec<&Entry> = all
        .iter()
        .filter(|e| query.is_empty() || e.label.to_lowercase().contains(&query))
        .collect();
    if shown.is_empty() {
        app.view.palette_sel = 0;
    } else {
        app.view.palette_sel = app.view.palette_sel.min(shown.len() - 1);
    }
    ctx.input(|i| {
        if i.key_pressed(Key::ArrowDown) {
            app.view.palette_sel = (app.view.palette_sel + 1).min(shown.len().saturating_sub(1));
        }
        if i.key_pressed(Key::ArrowUp) {
            app.view.palette_sel = app.view.palette_sel.saturating_sub(1);
        }
    });
    let enter = ctx.input(|i| i.key_pressed(Key::Enter));
    let mut run: Option<usize> = None;
    let card = egui::Area::new(egui::Id::new("palette-card"))
        .order(egui::Order::Tooltip)
        .pivot(Align2::CENTER_TOP)
        .fixed_pos(pos2(screen.center().x, screen.top() + 96.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(p.surface)
                .stroke(egui::Stroke::new(1.0_f32, p.border))
                .corner_radius(CornerRadius::same(radius::DIALOG))
                .shadow(ctx.style().visuals.popup_shadow)
                .show(ui, |ui| {
                    ui.set_width(560.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.label(egui::RichText::new(ph::SEARCH).size(20.0).color(p.weak));
                        let edit = egui::TextEdit::singleline(&mut app.view.palette_query)
                            .frame(false)
                            .desired_width(ui.available_width() - 16.0)
                            .margin(vec2(4.0, 14.0))
                            .hint_text(i18n::PALETTE_PLACEHOLDER);
                        ui.add(edit).request_focus();
                    });
                    ui.painter().hline(
                        ui.min_rect().x_range(),
                        ui.min_rect().bottom(),
                        egui::Stroke::new(1.0_f32, p.border),
                    );
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            if shown.is_empty() {
                                ui.add_space(16.0);
                                ui.label(egui::RichText::new(i18n::PALETTE_EMPTY).color(p.weak));
                                ui.add_space(16.0);
                            }
                            let mut group = "";
                            for (i, e) in shown.iter().enumerate() {
                                if e.group != group {
                                    group = e.group;
                                    ui.add_space(8.0);
                                    ui.horizontal(|ui| {
                                        ui.add_space(16.0);
                                        kit::caption(ui, &p, group);
                                    });
                                    ui.add_space(4.0);
                                }
                                if entry_row(ui, &p, e, i == app.view.palette_sel).clicked() {
                                    run = Some(i);
                                }
                            }
                            ui.add_space(8.0);
                        });
                    ui.painter().hline(
                        ui.min_rect().x_range(),
                        ui.min_rect().bottom(),
                        egui::Stroke::new(1.0_f32, p.border),
                    );
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.label(
                            egui::RichText::new(i18n::PALETTE_HINT)
                                .size(ty::CAPTION)
                                .color(p.faint),
                        );
                        ui.add_space(0.0);
                    });
                    ui.add_space(8.0);
                });
        });
    if enter && !shown.is_empty() {
        run = Some(app.view.palette_sel);
    }
    let clicked_outside = scrim.inner.clicked()
        && ctx
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|pos| !card.response.rect.contains(pos));
    let escape = ctx.input(|i| i.key_pressed(Key::Escape));
    if let Some(i) = run {
        let (method, params) = (shown[i].method, shown[i].params.clone());
        app.act(method, params);
    }
    if run.is_some() || escape || clicked_outside {
        app.view.palette_query.clear();
        app.view.palette_sel = 0;
        app.act("ui.window", json!({"id": "palette", "open": false}));
    }
}

fn entry_row(ui: &mut egui::Ui, p: &Palette, e: &Entry, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
    let inner = Rect::from_min_max(rect.min + vec2(8.0, 1.0), rect.max - vec2(8.0, 1.0));
    if selected || response.hovered() {
        ui.painter().rect_filled(
            inner,
            CornerRadius::same(radius::MD),
            if selected {
                p.accent_soft
            } else {
                p.menu_hover
            },
        );
    }
    ui.painter().text(
        inner.left_center() + vec2(18.0, 0.0),
        Align2::CENTER_CENTER,
        e.icon,
        FontId::new(20.0, egui::FontFamily::Proportional),
        if selected { p.on_selected } else { p.weak },
    );
    ui.painter().text(
        inner.left_center() + vec2(40.0, 0.0),
        Align2::LEFT_CENTER,
        &e.label,
        FontId::new(
            ty::CONTROL,
            if selected {
                semibold()
            } else {
                egui::FontFamily::Proportional
            },
        ),
        if selected { p.on_selected } else { p.text },
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
