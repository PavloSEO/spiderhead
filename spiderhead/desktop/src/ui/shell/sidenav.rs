//! Side navigation (200, or the 64 icon rail when compact) with the
//! sections of the canon and the profile button pinned bottom-left.

use eframe::egui::{self, pos2, vec2, Align2, CornerRadius, FontId, Rect, RichText, Sense};
use serde_json::json;

use crate::app::{Mode, Nav};
use crate::i18n::{self, strs};
use crate::theme::{radius, row, semibold, space, ty, Palette, Theme};
use crate::ui::kit::icons as ph;
use crate::ui::kit::{self, bold};
use crate::ui::DesktopApp;

/// Window width under which the navigation folds to the rail.
const COMPACT_BELOW: f32 = 1000.0;

/// What an item opens.
#[derive(Clone, Copy)]
enum Target {
    /// A screen of this build.
    Go(Nav),
    /// Needs a project (canon: shown disabled with a hint).
    NeedsProject,
    /// Screen not built yet.
    Soon,
}

struct Item {
    icon: &'static str,
    label: &'static str,
    target: Target,
    count: Option<String>,
}

fn item(icon: &'static str, label: &'static str, target: Target, count: Option<String>) -> Item {
    Item {
        icon,
        label,
        target,
        count,
    }
}

/// Draw the navigation panel.
pub fn navigation(ctx: &egui::Context, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    let compact = ctx.content_rect().width() < COMPACT_BELOW || app.model.nav_collapsed;
    let urls = app
        .model
        .table
        .last
        .as_ref()
        .and_then(|l| l.get("total"))
        .and_then(|v| v.as_i64())
        .map(kit::thousands);
    let issues = app
        .model
        .findings
        .as_ref()
        .and_then(|f| f.as_ref())
        .map(|f| kit::thousands(f.issues_total));
    use Target::{Go, NeedsProject as Need, Soon};
    // Each product mode lists only its own sections (canon §11.0).
    let (top, groups): (Vec<Item>, Vec<(&'static str, Vec<Item>)>) = match app.model.mode {
        Some(Mode::Crawler) | None => (
            vec![item(
                ph::TRAVEL_EXPLORE,
                i18n::NAV_CRAWLER,
                Go(Nav::Crawler),
                None,
            )],
            vec![(
                i18n::NAV_SEC_DATA,
                vec![
                    item(ph::TABLE_VIEW, strs().nav_urls, Go(Nav::Urls), urls),
                    item(ph::RULE, i18n::NAV_ISSUES, Go(Nav::Dashboard), issues),
                    item(
                        ph::COMPARE_ARROWS,
                        i18n::NAV_COMPARE,
                        Go(Nav::Compare),
                        None,
                    ),
                    item(ph::FIND_IN_PAGE, i18n::NAV_SEARCH, Go(Nav::Search), None),
                    item(ph::FACT_CHECK, i18n::NAV_CHECK, Go(Nav::Check), None),
                ],
            )],
        ),
        Some(Mode::Logs) => (
            Vec::new(),
            vec![(
                i18n::NAV_SEC_LOGS,
                vec![
                    item(ph::DESCRIPTION, i18n::NAV_LOGS, Go(Nav::Logs), None),
                    item(ph::SMART_TOY, i18n::NAV_LOGS_BOTS, Go(Nav::LogsBots), None),
                    item(ph::DIFFERENCE, i18n::NAV_LOGS_X, Go(Nav::LogsCompare), None),
                ],
            )],
        ),
        Some(Mode::Agent) => (
            vec![
                item(ph::CHECKLIST, i18n::NAV_WORK, Go(Nav::Work), None),
                item(ph::INBOX, strs().nav_inbox, Go(Nav::Inbox), None),
                item(ph::WORKSPACES, strs().nav_projects, Go(Nav::Projects), None),
            ],
            vec![
                (
                    i18n::NAV_SEC_DATA,
                    vec![
                        item(ph::HISTORY, i18n::NAV_SCANS, Need, None),
                        item(ph::TABLE_VIEW, strs().nav_urls, Go(Nav::Urls), urls),
                        item(ph::RULE, i18n::NAV_ISSUES, Go(Nav::Dashboard), issues),
                        item(
                            ph::COMPARE_ARROWS,
                            i18n::NAV_COMPARE,
                            Go(Nav::Compare),
                            None,
                        ),
                        item(ph::HUB, i18n::NAV_GRAPH, Soon, None),
                        item(ph::LINK, i18n::NAV_BACKLINKS, Need, None),
                        item(ph::DATASET, i18n::NAV_SEMANTICS, Need, None),
                        item(ph::FIND_IN_PAGE, i18n::NAV_SEARCH, Go(Nav::Search), None),
                    ],
                ),
                (
                    i18n::NAV_SEC_RESULT,
                    vec![
                        item(ph::MENU_BOOK, i18n::NAV_METHODS, Need, None),
                        item(ph::DESCRIPTION, i18n::NAV_REPORTS, Need, None),
                        item(ph::TERMINAL, i18n::NAV_LOG, Soon, None),
                    ],
                ),
            ],
        ),
    };
    egui::SidePanel::left("nav")
        .resizable(false)
        .exact_width(if compact { 48.0 } else { row::NAV_WIDTH })
        .frame(
            egui::Frame::new()
                .fill(p.nav)
                .inner_margin(egui::Margin::symmetric(8, 10)),
        )
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for it in &top {
                nav_row(ui, app, &p, it, compact);
            }
            for (title, items) in &groups {
                ui.add_space(space::SM);
                if compact {
                    ui.separator();
                } else {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        kit::caption(ui, &p, title);
                    });
                    ui.add_space(2.0);
                }
                for it in items {
                    nav_row(ui, app, &p, it, compact);
                }
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                profile_button(ui, app, &p, compact);
            });
        });
}

/// One navigation row: icon, label, count; disabled items fade and explain why.
fn nav_row(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette, it: &Item, compact: bool) {
    let nav = match it.target {
        Target::Go(n) => Some(n),
        _ => None,
    };
    let selected = nav.is_some_and(|n| app.model.nav == n);
    let sense = if nav.is_some() {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, mut response) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), sense);
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::same(radius::MD), p.accent_soft);
    } else if response.hovered() && nav.is_some() {
        painter.rect_filled(rect, CornerRadius::same(radius::MD), p.menu_hover);
    }
    let (ink, icon_ink) = match (selected, nav.is_some()) {
        (true, _) => (p.on_selected, p.on_selected),
        (false, true) => (p.text, p.weak),
        // Disabled label stays readable (the grey reads as disabled through
        // the icon); the label itself carries information.
        (false, false) => (p.muted, p.disabled),
    };
    let icon_x = if compact {
        rect.center().x
    } else {
        rect.left() + 12.0 + 10.0
    };
    painter.text(
        pos2(icon_x, rect.center().y),
        Align2::CENTER_CENTER,
        it.icon,
        FontId::new(20.0, egui::FontFamily::Proportional),
        icon_ink,
    );
    if !compact {
        let font = if selected {
            semibold()
        } else {
            egui::FontFamily::Proportional
        };
        let label_w = painter
            .text(
                pos2(rect.left() + 44.0, rect.center().y),
                Align2::LEFT_CENTER,
                it.label,
                FontId::new(ty::CONTROL, font),
                ink,
            )
            .width();
        match (&it.count, it.target) {
            (Some(n), _) => {
                painter.text(
                    pos2(rect.right() - 12.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    n,
                    FontId::new(ty::CAPTION, egui::FontFamily::Proportional),
                    if selected { p.on_selected } else { p.weak },
                );
            }
            (None, Target::NeedsProject) if label_w + 44.0 + 84.0 < rect.width() => {
                painter.text(
                    pos2(rect.right() - 10.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    "нужен проект",
                    FontId::new(10.0, egui::FontFamily::Proportional),
                    p.muted,
                );
            }
            _ => {}
        }
    }
    // Attach a tooltip only when it adds information: the compact rail hides
    // the label, and disabled rows explain why they are disabled. On expanded
    // enabled rows the tip would just duplicate the label and cover the next
    // row. Tooltips also paint on egui's topmost layer, so they are suppressed
    // while any modal (palette, dialogs, profile menu) is open.
    if app.model.windows.is_empty() && (compact || !matches!(it.target, Target::Go(_))) {
        let tip = match it.target {
            Target::Go(_) => it.label,
            Target::NeedsProject => i18n::NEED_PROJECT,
            Target::Soon => i18n::NOT_IN_VERSION,
        };
        response = response.on_hover_text(tip);
    }
    if let Some(n) = nav {
        if response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            app.act("ui.navigate", json!({"id": n.name()}));
        }
    }
}

/// The 44-point profile button; the menu opens above it through `ui.window`.
fn profile_button(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette, compact: bool) {
    let s = strs();
    ui.add_space(2.0);
    let open = app.model.windows.contains("profile");
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    if open || response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::MD), p.menu_hover);
    }
    let avatar = Rect::from_center_size(
        if compact {
            rect.center()
        } else {
            rect.left_center() + vec2(22.0, 0.0)
        },
        vec2(28.0, 28.0),
    );
    ui.painter()
        .circle_filled(avatar.center(), 14.0, p.tertiary);
    ui.painter().text(
        avatar.center(),
        Align2::CENTER_CENTER,
        "ПБ",
        FontId::new(12.0, semibold()),
        p.surface,
    );
    if !compact {
        let line = i18n::mode_label(app.model.mode.map(Mode::name).unwrap_or("start"));
        ui.painter().text(
            rect.left_center() + vec2(44.0, -8.0),
            Align2::LEFT_CENTER,
            s.profile,
            FontId::new(ty::BODY, semibold()),
            p.text,
        );
        ui.painter().text(
            rect.left_center() + vec2(44.0, 8.0),
            Align2::LEFT_CENTER,
            line,
            FontId::new(11.0, egui::FontFamily::Proportional),
            p.faint,
        );
        ui.painter().text(
            rect.right_center() - vec2(14.0, 0.0),
            Align2::CENTER_CENTER,
            ph::SETTINGS,
            FontId::new(18.0, egui::FontFamily::Proportional),
            p.muted,
        );
    }
    if response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        app.act("ui.window", json!({"id": "profile", "open": !open}));
    }
    if open {
        profile_menu(ui.ctx(), app, p, rect);
    }
}

/// Account/workspace menu, 264 wide, above the button: account header and
/// workspace, mode segment, theme, settings, usage, language, help, more,
/// sign-out. Every row goes through `DesktopApp::act`.
fn profile_menu(ctx: &egui::Context, app: &mut DesktopApp, p: &Palette, anchor: Rect) {
    let s = strs();
    let area = egui::Area::new(egui::Id::new("profile-menu"))
        .order(egui::Order::Foreground)
        .pivot(Align2::LEFT_BOTTOM)
        .fixed_pos(anchor.left_top() + vec2(0.0, -6.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(p.surface)
                .stroke(egui::Stroke::new(1.0_f32, p.border))
                .corner_radius(CornerRadius::same(radius::MD + 2))
                .shadow(ctx.style().visuals.popup_shadow)
                .inner_margin(egui::Margin::symmetric(4, 6))
                .show(ui, |ui| {
                    ui.set_width(264.0 - 8.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        ui.vertical(|ui| {
                            ui.label(bold(s.profile).size(ty::BODY));
                            ui.label(RichText::new(s.local_machine).size(11.0).color(p.faint));
                        });
                    });
                    ui.add_space(4.0);
                    menu_item(
                        ui,
                        p,
                        ph::CHECK,
                        p.accent,
                        i18n::MENU_WORKSPACE_LOCAL,
                        None,
                        false,
                    );
                    ui.separator();
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        kit::caption(ui, p, s.mode);
                    });
                    mode_segment(ui, app, p);
                    ui.add_space(4.0);
                    let mut close = false;
                    let mut go = |app: &mut DesktopApp, method: &str, params: serde_json::Value| {
                        app.act(method, params);
                        close = true;
                    };
                    let theme_row = menu_item(
                        ui,
                        p,
                        ph::PALETTE,
                        p.weak,
                        &i18n::menu_theme(i18n::theme_label(app.model.theme)),
                        None,
                        true,
                    );
                    if theme_row.clicked() {
                        app.view.theme_open = !app.view.theme_open;
                    }
                    if app.view.theme_open {
                        for theme in Theme::ALL {
                            let on = app.model.theme == theme;
                            let icon = if on { ph::CHECK } else { "" };
                            if menu_item(
                                ui,
                                p,
                                icon,
                                p.accent,
                                i18n::theme_label(theme),
                                None,
                                false,
                            )
                            .clicked()
                            {
                                go(app, "ui.theme", json!({"mode": theme.name()}));
                            }
                        }
                    }
                    ui.add_space(2.0);
                    ui.separator();
                    ui.add_space(2.0);
                    if menu_item(ui, p, ph::SETTINGS, p.weak, s.settings, Some("⌘,"), false)
                        .clicked()
                    {
                        go(app, "ui.window", json!({"id": "settings"}));
                    }
                    if menu_item(ui, p, ph::PAYMENTS, p.weak, i18n::MENU_USAGE, None, false)
                        .clicked()
                    {
                        go(app, "ui.window", json!({"id": "usage"}));
                    }
                    menu_item(
                        ui,
                        p,
                        ph::TRANSLATE,
                        p.weak,
                        i18n::MENU_LANGUAGE_ROW,
                        None,
                        true,
                    );
                    if menu_item(ui, p, ph::HELP, p.weak, s.help, Some("F1"), false).clicked() {
                        go(app, "ui.window", json!({"id": "help"}));
                    }
                    if menu_item(ui, p, ph::INFO, p.weak, i18n::MENU_MORE, None, true).clicked() {
                        go(app, "ui.window", json!({"id": "about"}));
                    }
                    ui.add_space(2.0);
                    ui.separator();
                    ui.add_space(2.0);
                    if menu_item(ui, p, ph::BLOCK, p.critical, i18n::MENU_LOGOUT, None, false)
                        .clicked()
                    {
                        go(app, "app.quit", json!({}));
                    }
                    close
                })
                .inner
        });
    let clicked_outside = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer
                .interact_pos()
                .is_some_and(|pos| !area.response.rect.contains(pos) && !anchor.contains(pos))
    });
    if area.inner || clicked_outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.view.theme_open = false;
        app.act("ui.window", json!({"id": "profile", "open": false}));
    }
}

/// The three-mode segment of the profile menu (Краулер / Логи / Проект).
fn mode_segment(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        let width = ui.available_width() - 6.0;
        let (rect, _) = ui.allocate_exact_size(vec2(width, 32.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, CornerRadius::same(10), p.hover);
        let third = rect.width() / 3.0;
        for (i, mode) in Mode::ALL.into_iter().enumerate() {
            let on = app.model.mode == Some(mode);
            let cell = Rect::from_min_size(
                rect.left_top() + vec2(third * i as f32, 0.0),
                vec2(third, 32.0),
            )
            .shrink(2.0);
            let resp = ui.interact(cell, ui.id().with(mode.name()), Sense::click());
            if on {
                ui.painter()
                    .rect_filled(cell, CornerRadius::same(8), p.surface);
            }
            ui.painter().text(
                cell.center(),
                Align2::CENTER_CENTER,
                i18n::mode_label(mode.name()),
                FontId::new(12.0, semibold()),
                if on { p.on_selected } else { p.weak },
            );
            if resp
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                app.act("ui.mode", json!({"mode": mode.name()}));
            }
        }
    });
}

/// Menu row (34): icon, label, optional key hint or chevron.
fn menu_item(
    ui: &mut egui::Ui,
    p: &Palette,
    icon: &str,
    icon_color: egui::Color32,
    label: &str,
    hint: Option<&str>,
    chevron: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(
            rect.shrink2(vec2(2.0, 0.0)),
            CornerRadius::same(8),
            p.menu_hover,
        );
    }
    ui.painter().text(
        rect.left_center() + vec2(16.0, 0.0),
        Align2::CENTER_CENTER,
        icon,
        FontId::new(18.0, egui::FontFamily::Proportional),
        icon_color,
    );
    ui.painter().text(
        rect.left_center() + vec2(34.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::new(ty::BODY, egui::FontFamily::Proportional),
        p.text,
    );
    if let Some(h) = hint {
        ui.painter().text(
            rect.right_center() - vec2(12.0, 0.0),
            Align2::RIGHT_CENTER,
            h,
            FontId::new(11.0, egui::FontFamily::Monospace),
            p.faint,
        );
    }
    if chevron {
        ui.painter().text(
            rect.right_center() - vec2(14.0, 0.0),
            Align2::CENTER_CENTER,
            ph::CHEVRON_RIGHT,
            FontId::new(18.0, egui::FontFamily::Proportional),
            p.muted,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
