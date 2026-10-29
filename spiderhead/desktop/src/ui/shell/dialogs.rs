//! Dialog windows opened by `ui.window` (settings, MCP, command line, help).

use eframe::egui;
use serde_json::json;

use crate::i18n::{self, strs};
use crate::ui::DesktopApp;

/// Dialog windows the `ui.window` command opens (settings, MCP, CLI, help).
pub fn windows(ctx: &egui::Context, app: &mut DesktopApp) {
    for id in app.model.windows.clone() {
        let mut open = true;
        match id.as_str() {
            "settings" | "mcp" | "cli" | "help" | "about" | "usage" | "shortcuts" => {
                if scrim_clicked(ctx, app, &id) {
                    open = false;
                }
                match id.as_str() {
                    "settings" => settings_window(ctx, app, &mut open),
                    "mcp" => mcp_window(ctx, app, &mut open),
                    "cli" => cli_window(ctx, app, &mut open),
                    "help" => help_window(ctx, app, &mut open),
                    "about" => about_window(ctx, app, &mut open),
                    "usage" => usage_window(ctx, app, &mut open),
                    _ => shortcuts_window(ctx, app, &mut open),
                }
            }
            "palette" => {
                super::palette::show(ctx, app);
                continue;
            }
            // The profile menu is drawn by the side navigation.
            _ => continue,
        }
        if !open {
            app.act("ui.window", json!({"id": id, "open": false}));
        }
    }
}

/// Modal scrim under a dialog window — the same token the palette uses.
/// Returns `true` when the user clicked the dimmed area (close intent).
fn scrim_clicked(ctx: &egui::Context, app: &DesktopApp, id: &str) -> bool {
    let p = app.model.theme.palette();
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::new(format!("dialog-scrim-{id}")))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            ui.painter().rect_filled(
                rect,
                0.0,
                egui::Color32::from_black_alpha(if p.theme.is_dark() { 150 } else { 80 }),
            );
            resp
        })
        .inner
        .clicked()
}

/// Настройки: the section rail and pages live in `ui::settings`.
fn settings_window(ctx: &egui::Context, app: &mut DesktopApp, open: &mut bool) {
    crate::ui::settings::window(ctx, app, open);
}

/// MCP-сервер: bounded state — the desktop does not embed the server yet.
fn mcp_window(ctx: &egui::Context, app: &mut DesktopApp, open: &mut bool) {
    let s = strs();
    egui::Window::new(s.mcp)
        .open(open)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(egui::RichText::new(crate::i18n::MCP_UNAVAILABLE).weak());
            ui.add_space(6.0);
            ui.label(crate::i18n::MCP_HINT);
            ui.horizontal(|ui| {
                ui.monospace("spiderhead-mcp");
                if ui.button(crate::i18n::CLI_COPY).clicked() {
                    ui.ctx().copy_text("spiderhead-mcp".into());
                    app.model.status = crate::i18n::COPIED.into();
                }
            });
            ui.add_space(6.0);
            if ui.button("Подключить к Claude").clicked() {
                if let Some(v) = app.act("app.connect_claude", json!({})) {
                    if let Some(text) = v["clipboard"].as_str() {
                        ui.ctx().copy_text(text.to_owned());
                    }
                }
            }
        });
}

/// Командная строка: the ctl path of this window, ready to copy — like the
/// reference dialog showed the core-CLI path.
fn cli_window(ctx: &egui::Context, app: &mut DesktopApp, open: &mut bool) {
    let s = strs();
    let ctl = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("spiderhead-desktop-ctl")))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "spiderhead-desktop-ctl".into());
    egui::Window::new(s.cli_path)
        .open(open)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(egui::RichText::new(crate::i18n::CLI_NOTE).weak());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.monospace("spiderhead-desktop-ctl");
                if ui.button(crate::i18n::CLI_COPY).clicked() {
                    ui.ctx().copy_text(ctl.clone());
                    app.model.status = crate::i18n::COPIED.into();
                }
            });
            if ui.button("Установить команду в терминал").clicked() {
                app.act("app.install_cli", json!({}));
            }
            ui.label(
                egui::RichText::new(crate::i18n::CLI_COMMANDS_HINT)
                    .weak()
                    .small(),
            );
            // The resolved absolute path is dev detail: keep it, but collapsed.
            egui::CollapsingHeader::new(crate::i18n::CLI_DETAILS)
                .default_open(false)
                .show(ui, |ui| {
                    ui.monospace(egui::RichText::new(&ctl).small());
                });
        });
}

/// Справка: about + the command surface pointer.
fn help_window(ctx: &egui::Context, _app: &mut DesktopApp, open: &mut bool) {
    let s = strs();
    egui::Window::new(s.help)
        .open(open)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(crate::i18n::help_version(env!("CARGO_PKG_VERSION")));
            ui.add_space(6.0);
            ui.label(egui::RichText::new(crate::i18n::HELP_COMMANDS).weak());
            ui.label(egui::RichText::new(crate::i18n::HELP_HOTKEY).weak());
        });
}

/// О программе: name, version, tagline — the «Подробнее» of the profile menu.
fn about_window(ctx: &egui::Context, _app: &mut DesktopApp, open: &mut bool) {
    egui::Window::new(i18n::MENU_MORE)
        .open(open)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(crate::ui::kit::bold(i18n::APP_TITLE).size(18.0));
            ui.label(crate::i18n::help_version(env!("CARGO_PKG_VERSION")));
            ui.add_space(6.0);
            ui.label(egui::RichText::new(i18n::ABOUT_LINE).weak());
        });
}

/// Использование: bounded state — no paid sources are wired into the window.
fn usage_window(ctx: &egui::Context, _app: &mut DesktopApp, open: &mut bool) {
    egui::Window::new(i18n::MENU_USAGE)
        .open(open)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(egui::RichText::new(i18n::USAGE_EMPTY).weak());
        });
}

/// Горячие клавиши: the menu accelerators in one sheet.
fn shortcuts_window(ctx: &egui::Context, _app: &mut DesktopApp, open: &mut bool) {
    const GROUPS: &[(&str, &[(&str, &str)])] = &[
        (
            i18n::SHORTCUT_MODES,
            &[
                ("⌘1", i18n::MODE_CRAWLER),
                ("⌘2", i18n::MODE_LOGS),
                ("⌘3", i18n::MODE_AGENT),
            ],
        ),
        (
            i18n::SHORTCUT_APP,
            &[
                ("⌘K", i18n::PALETTE_FIELD),
                ("⌘,", i18n::MENU_SETTINGS),
                ("⌘N", "Новый краул"),
                ("⌘O", "Открыть скан"),
                ("F5", "Обновить"),
            ],
        ),
        (
            i18n::SHORTCUT_EDIT,
            &[
                ("⌘Z", "Отменить"),
                ("⌘X", "Вырезать"),
                ("⌘C", "Копировать"),
                ("⌘V", "Вставить"),
                ("⌘A", "Выбрать всё"),
                ("⌘F", "Найти в таблице"),
            ],
        ),
    ];
    egui::Window::new("Горячие клавиши")
        .open(open)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            for (title, rows) in GROUPS {
                ui.label(crate::ui::kit::bold(*title));
                ui.add_space(2.0);
                egui::Grid::new(format!("keys-{title}"))
                    .num_columns(2)
                    .spacing([24.0, 4.0])
                    .show(ui, |ui| {
                        for (key, label) in *rows {
                            ui.monospace(*key);
                            ui.label(egui::RichText::new(*label).weak());
                            ui.end_row();
                        }
                    });
                ui.add_space(10.0);
            }
        });
}
