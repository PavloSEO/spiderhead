//! The settings dialog: a section rail on the left, the active page in a
//! scroll area and an always-visible apply footer. Every control dispatches
//! `settings.*`/`sources.*` commands through `DesktopApp::act`, so the control
//! channel sees exactly what the user did.

mod sources;

use std::collections::BTreeMap;

use eframe::egui::{self, vec2, RichText};
use serde_json::json;

use crate::i18n::settings as st;
use crate::theme::{row, space, ty, Palette};
use crate::ui::kit::{self, icons as ph};
use crate::ui::DesktopApp;

/// Widget-side state of the settings window. Secret buffers live only here —
/// the model and the file never see a typed key.
#[derive(Default)]
pub struct SettingsView {
    /// Text-field buffers keyed by setting key.
    pub texts: BTreeMap<String, String>,
    /// Last rejected value message per key (from `settings.set` errors).
    pub errors: BTreeMap<String, String>,
    /// Section-nav filter text.
    pub search: String,
    /// Secret input on the source-key page; cleared after `sources.credential`.
    pub secret: String,
    /// Credential component the source-key page edits (`token`, `login`…).
    pub component: String,
    /// Path buffer of the data-page export/import row.
    pub exchange_path: String,
    /// Cached `sources.list` reply for the sources pages.
    pub providers: Option<serde_json::Value>,
    /// Cached `sources.spend` reply for the spend page.
    pub spend: Option<serde_json::Value>,
}

/// The navigation sections (canvas order): (page id, icon, label).
fn sections() -> &'static [(&'static str, &'static str, &'static str)] {
    const SECTIONS: &[(&str, &str, &str)] = &[
        ("general", ph::SETTINGS, st::NAV_GENERAL),
        ("view", ph::PALETTE, st::NAV_VIEW),
        ("scan", ph::MANAGE_SEARCH, st::NAV_SCAN),
        ("core", ph::MEMORY, st::NAV_CORE),
        ("mcp", ph::HUB, st::NAV_MCP),
        ("sources", ph::DNS, st::NAV_SOURCES),
        ("agent", ph::SMART_TOY, st::NAV_AGENT),
        ("notify", ph::NOTIFICATIONS, st::NAV_NOTIFY),
        ("keys", ph::KEYBOARD, st::NAV_KEYS),
        ("data", ph::DATABASE, st::NAV_DATA),
        ("updates", ph::CACHED, st::NAV_UPDATES),
        ("about", ph::INFO, st::NAV_ABOUT),
    ];
    SECTIONS
}

/// Sub-pages that are not in the rail (opened from inside a section).
fn page_title(id: &str) -> &'static str {
    match id {
        "general" => st::NAV_GENERAL,
        "view" => st::NAV_VIEW,
        "scan" => st::NAV_SCAN,
        "core" => st::NAV_CORE,
        "mcp" => st::NAV_MCP,
        "sources" | "source_detail" | "source_key" | "sources_auth" | "spend" => st::NAV_SOURCES,
        "agent" | "agent_config" => st::NAV_AGENT,
        "notify" => st::NAV_NOTIFY,
        "keys" => st::NAV_KEYS,
        "data" => st::NAV_DATA,
        "updates" => st::NAV_UPDATES,
        _ => st::NAV_ABOUT,
    }
}

/// The settings window (`ui.window {id:"settings"}`).
pub fn window(ctx: &egui::Context, app: &mut DesktopApp, open: &mut bool) {
    let p = app.model.theme.palette();
    egui::Window::new(st::TITLE)
        .open(open)
        .collapsible(false)
        .default_size(vec2(1180.0, 780.0))
        .min_size(vec2(860.0, 560.0))
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(p.surface)
                .corner_radius(egui::CornerRadius::same(crate::theme::radius::DIALOG)),
        )
        .show(ctx, |ui| {
            let height = ui.available_height() - row::HEADER - space::SM;
            ui.horizontal(|ui| {
                nav_rail(ui, app, height);
                ui.add(egui::Separator::default().vertical().spacing(space::MD));
                page(ui, app, height);
            });
            footer(ui, app);
        });
}

/// The left rail: search box plus the section items.
fn nav_rail(ui: &mut egui::Ui, app: &mut DesktopApp, height: f32) {
    let p = app.model.theme.palette();
    ui.allocate_ui(vec2(row::NAV_WIDTH, height), |ui| {
        ui.vertical(|ui| {
            let search = &mut app.view.settings.search;
            let _ = ui.add(
                egui::TextEdit::singleline(search)
                    .hint_text(st::SEARCH)
                    .desired_width(row::NAV_WIDTH - space::MD),
            );
            ui.add_space(space::SM);
            let needle = app.view.settings.search.to_lowercase();
            for &(id, icon, label) in sections() {
                if !needle.is_empty() && !label.to_lowercase().contains(&needle) {
                    continue;
                }
                if kit::nav_item(ui, &p, icon, label, app.model.settings_page == id, None).clicked()
                {
                    app.act("settings.page", json!({"id": id}));
                }
            }
        });
    });
}

/// The active page: header plus the routed body in a scroll area.
fn page(ui: &mut egui::Ui, app: &mut DesktopApp, height: f32) {
    let p = app.model.theme.palette();
    ui.vertical(|ui| {
        kit::caption(ui, &p, page_title(&app.model.settings_page));
        ui.add_space(space::XS);
        egui::ScrollArea::vertical()
            .id_salt("settings-page")
            .max_height(height - ty::OVERLINE - space::SM)
            .show(ui, |ui| match app.model.settings_page.as_str() {
                "general" => page_general(ui, app),
                "view" => page_view(ui, app),
                "scan" => page_scan(ui, app),
                "core" => page_core(ui, app),
                "mcp" => page_mcp(ui, app),
                "sources" => sources::page_sources(ui, app),
                "source_detail" => sources::page_source_detail(ui, app),
                "source_key" => sources::page_source_key(ui, app),
                "sources_auth" => sources::page_sources_auth(ui, app),
                "spend" => sources::page_spend(ui, app),
                "agent" => page_agent(ui, app),
                "agent_config" => page_agent_config(ui, app),
                "notify" => page_notify(ui, app),
                "keys" => page_keys(ui, app),
                "data" => page_data(ui, app),
                "updates" => page_updates(ui, app),
                _ => page_about(ui, app),
            });
    });
}

/// Footer: the immediate-apply note, the last status and «Готово».
fn footer(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    ui.add_space(space::SM);
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(st::APPLY_NOTE)
                .size(ty::CAPTION)
                .color(p.faint),
        );
        if !app.model.status.is_empty() {
            ui.label(
                RichText::new(format!("·  {}", app.model.status))
                    .size(ty::CAPTION)
                    .color(p.warning),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if kit::button(ui, &p, None, st::DONE, kit::BtnKind::Primary).clicked() {
                app.act("ui.window", json!({"id": "settings", "open": false}));
            }
        });
    });
}

// ── Row widgets ─────────────────────────────────────────────────────

/// Store a setting through the control surface; keep the rejection text under
/// the row when the value does not validate.
fn set(app: &mut DesktopApp, key: &str, value: serde_json::Value) {
    match app.act("settings.set", json!({"key": key, "value": value})) {
        Some(_) => {
            app.view.settings.errors.remove(key);
        }
        None => {
            let message = app.model.status.clone();
            app.view.settings.errors.insert(key.to_string(), message);
        }
    }
}

/// One labelled row: title + hint on the left, the control on the right.
fn row_box(
    ui: &mut egui::Ui,
    p: &Palette,
    title: &str,
    hint: &str,
    body: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width() - 240.0);
            ui.label(RichText::new(title).size(ty::BODY));
            if !hint.is_empty() {
                ui.label(RichText::new(hint).size(ty::CAPTION).color(p.weak));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), body);
    });
    ui.add_space(space::XS);
    ui.separator();
    ui.add_space(space::XS);
}

/// A boolean row: label + switch.
fn switch(ui: &mut egui::Ui, app: &mut DesktopApp, key: &str, title: &str, hint: &str) {
    let p = app.model.theme.palette();
    let mut on = app.model.settings.get_bool(key);
    row_box(ui, &p, title, hint, |ui| {
        if ui.checkbox(&mut on, "").changed() {
            set(app, key, json!(on));
        }
        if app.model.settings.is_set(key) {
            ui.label(RichText::new(st::CHANGED).size(ty::CAPTION).color(p.faint));
        }
    });
}

/// A free-text row committed on focus loss / Enter.
fn text(ui: &mut egui::Ui, app: &mut DesktopApp, key: &str, title: &str, hint: &str) {
    let p = app.model.theme.palette();
    let stored = app.model.settings.get_str(key);
    let mut edit = app
        .view
        .settings
        .texts
        .get(key)
        .cloned()
        .unwrap_or_else(|| stored.clone());
    row_box(ui, &p, title, hint, |ui| {
        let resp = ui.add(egui::TextEdit::singleline(&mut edit).desired_width(220.0));
        if resp.changed() {
            app.view
                .settings
                .texts
                .insert(key.to_string(), edit.clone());
        }
        if resp.lost_focus() && edit != stored {
            set(app, key, json!(edit));
            app.view.settings.texts.remove(key);
        }
    });
    error_line(ui, app, &p, key);
}

/// A numeric row (int/float/time share the text field; validation is the model's).
fn number(ui: &mut egui::Ui, app: &mut DesktopApp, key: &str, title: &str, hint: &str) {
    let p = app.model.theme.palette();
    let stored = match app.model.settings.get(key) {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    };
    let mut edit = app
        .view
        .settings
        .texts
        .get(key)
        .cloned()
        .unwrap_or_else(|| stored.clone());
    row_box(ui, &p, title, hint, |ui| {
        let resp = ui.add(egui::TextEdit::singleline(&mut edit).desired_width(90.0));
        if resp.changed() {
            app.view
                .settings
                .texts
                .insert(key.to_string(), edit.clone());
        }
        if resp.lost_focus() && edit != stored {
            set(app, key, json!(edit));
            app.view.settings.texts.remove(key);
        }
    });
    error_line(ui, app, &p, key);
}

/// The rejected-value message under a field.
fn error_line(ui: &mut egui::Ui, app: &DesktopApp, p: &Palette, key: &str) {
    if let Some(message) = app.view.settings.errors.get(key) {
        ui.label(RichText::new(message).size(ty::CAPTION).color(p.critical));
    }
}

/// A choices row: compact combo box.
fn choice(
    ui: &mut egui::Ui,
    app: &mut DesktopApp,
    key: &str,
    title: &str,
    hint: &str,
    options: &[(&str, &str)],
) {
    let p = app.model.theme.palette();
    let current = app.model.settings.get_str(key);
    row_box(ui, &p, title, hint, |ui| {
        let label = options
            .iter()
            .find(|(v, _)| *v == current)
            .map(|(_, l)| *l)
            .unwrap_or(current.as_str());
        egui::ComboBox::from_id_salt(key)
            .selected_text(label)
            .width(180.0)
            .show_ui(ui, |ui| {
                for &(value, label) in options {
                    if ui.selectable_label(current == value, label).clicked() {
                        set(app, key, json!(value));
                    }
                }
            });
    });
}

/// Segmented choice (few options, always visible). The row hosts the control
/// in a right-to-left region, so the options iterate reversed to keep the
/// declared left-to-right reading order.
fn segmented(
    ui: &mut egui::Ui,
    app: &mut DesktopApp,
    key: &str,
    title: &str,
    hint: &str,
    options: &[(&str, &str)],
) {
    let p = app.model.theme.palette();
    let current = app.model.settings.get_str(key);
    row_box(ui, &p, title, hint, |ui| {
        ui.horizontal(|ui| {
            for &(value, label) in options.iter().rev() {
                if ui.selectable_label(current == value, label).clicked() {
                    set(app, key, json!(value));
                }
            }
        });
    });
}

/// Uppercase group label.
fn group(ui: &mut egui::Ui, p: &Palette, text: &str) {
    ui.add_space(space::MD);
    kit::caption(ui, p, text);
    ui.add_space(space::XS);
}

// ── Pages ───────────────────────────────────────────────────────────

/// Общие: startup, closing and behaviour in two columns.
fn page_general(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    ui.columns(2, |cols| {
        let (a, b) = cols.split_at_mut(1);
        let (left, right) = (&mut a[0], &mut b[0]);
        group(left, &p, st::G_STARTUP);
        segmented(
            left,
            app,
            "general.start",
            st::G_OPEN_AT_START,
            st::G_OPEN_HINT,
            &[("last", st::G_LAST_PROJECT), ("list", st::G_PROJECT_LIST)],
        );
        switch(
            left,
            app,
            "general.restore_tabs",
            st::G_RESTORE_TABS,
            st::G_RESTORE_TABS_HINT,
        );
        number(
            left,
            app,
            "general.recent_count",
            st::G_RECENT,
            st::G_RECENT_HINT,
        );
        text(
            left,
            app,
            "general.projects_folder",
            st::G_FOLDER,
            st::G_FOLDER_HINT,
        );
        group(left, &p, st::G_CLOSING);
        switch(
            left,
            app,
            "general.confirm_close_active_scan",
            st::G_CONFIRM_CLOSE,
            st::G_CONFIRM_CLOSE_HINT,
        );
        switch(
            left,
            app,
            "general.stay_in_menu_bar",
            st::G_STAY_MENU,
            st::G_STAY_MENU_HINT,
        );
        group(right, &p, st::G_BEHAVIOUR);
        segmented(
            right,
            app,
            "general.link_action",
            st::G_LINKS,
            st::G_LINKS_HINT,
            &[
                ("details", st::G_LINKS_DETAILS),
                ("browser", st::G_LINKS_BROWSER),
            ],
        );
        number(
            right,
            app,
            "general.autosave_seconds",
            st::G_AUTOSAVE,
            st::G_AUTOSAVE_HINT,
        );
        choice(
            right,
            app,
            "general.date_format",
            st::G_DATE_FORMAT,
            st::G_DATE_FORMAT_HINT,
            &[
                ("dd.MM.yyyy HH:mm", "09.10.2026 14:30"),
                ("yyyy-MM-dd HH:mm", "2026-10-09 14:30"),
                ("MM/dd/yyyy hh:mm a", "10/09/2026 02:30 PM"),
            ],
        );
        switch(
            right,
            app,
            "general.crash_stats",
            st::G_CRASH,
            st::G_CRASH_HINT,
        );
        ui_add_space(right);
        info_card(right, &p, st::G_NOTE);
    });
}

/// Вид: theme applies immediately via `settings.set` → `apply_setting`.
fn page_view(ui: &mut egui::Ui, app: &mut DesktopApp) {
    segmented(
        ui,
        app,
        "view.theme",
        st::V_THEME,
        "",
        &[
            ("light", crate::i18n::THEME_LIGHT),
            ("dark", crate::i18n::THEME_DARK),
            ("hc", crate::i18n::THEME_HC),
            ("system", crate::i18n::THEME_SYSTEM),
        ],
    );
    choice(
        ui,
        app,
        "view.language",
        st::V_LANGUAGE,
        "",
        &[("en", "English")],
    );
    choice(
        ui,
        app,
        "view.density",
        st::V_DENSITY,
        "",
        &[
            ("compact", st::V_DENSITIES[0]),
            ("standard", st::V_DENSITIES[1]),
            ("comfortable", st::V_DENSITIES[2]),
        ],
    );
    segmented(
        ui,
        app,
        "view.details_position",
        st::V_DETAILS_POS,
        "",
        &[
            ("bottom", st::V_DETAILS_POS_OPTS[0]),
            ("right", st::V_DETAILS_POS_OPTS[1]),
        ],
    );
    number(ui, app, "view.zoom", st::V_ZOOM, st::V_ZOOM_RANGE);
    switch(ui, app, "view.reduce_motion", st::V_REDUCE_MOTION, "");
    switch(ui, app, "view.mono_urls", st::V_MONO_URLS, "");
    switch(ui, app, "view.rail_when_narrow", st::V_RAIL, "");
    switch(ui, app, "view.focus_mode", st::V_FOCUS, "");
    switch(ui, app, "view.status_badges", st::V_BADGES, "");
}

/// Сканы по умолчанию: values new scans on this machine start with.
fn page_scan(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    let current = app.model.settings.get_str("scan.default_profile");
    row_box(ui, &p, st::S_PROFILE, st::s_profile_hint(&current), |ui| {
        egui::ComboBox::from_id_salt("scan.default_profile")
            .selected_text(st::s_profile_name(&current))
            .width(200.0)
            .show_ui(ui, |ui| {
                for id in ["quick", "full", "sitemap", "js"] {
                    if ui
                        .selectable_label(current == id, st::s_profile_name(id))
                        .clicked()
                    {
                        set(app, "scan.default_profile", json!(id));
                    }
                }
            });
    });
    number(ui, app, "scan.rate", st::S_RATE, st::S_RATE_RANGE);
    number(
        ui,
        app,
        "scan.url_limit",
        st::S_URL_LIMIT,
        st::S_LIMIT_RANGE,
    );
    number(ui, app, "scan.depth", st::S_DEPTH, st::S_DEPTH_RANGE);
    switch(ui, app, "scan.robots", st::S_ROBOTS, "");
    switch(ui, app, "scan.save_html", st::S_SAVE_HTML, "");
    number(
        ui,
        app,
        "scan.parallel",
        st::S_PARALLEL,
        st::S_PARALLEL_RANGE,
    );
    text(ui, app, "scan.sf_path", st::S_SF_PATH, "");
    // The license check is a core call this build does not expose: the row
    // stays and reports its «no data» state exactly like reference does.
    row_box(ui, &p, st::S_LICENSE, st::S_LICENSE_HINT, |ui| {
        let (bg, fg) = p.tone(crate::theme::tokens::Status::Unavailable);
        kit::badge_on(ui, bg, fg, st::S_LICENSE_UNKNOWN);
    });
    info_card(ui, &p, st::S_NOTE);
}

/// Ядро: shell-over-core status and the custom-CLI override.
fn page_core(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    choice(
        ui,
        app,
        "core.poll",
        st::C_POLL,
        "",
        &[("0.25", "0,25"), ("0.5", "0,5"), ("1", "1"), ("2", "2")],
    );
    choice(
        ui,
        app,
        "core.log_level",
        st::C_LOG,
        "",
        &[("INFO", "INFO"), ("DEBUG", "DEBUG")],
    );
    group(ui, &p, st::C_DIAG);
    kit::card(ui, &p, |ui| {
        ui.label(RichText::new(st::C_NOTE).size(ty::CAPTION).color(p.weak));
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(st::AB_VERSION)
                    .size(ty::CAPTION)
                    .color(p.faint),
            );
            ui.monospace(env!("CARGO_PKG_VERSION"));
        });
    });
}

/// MCP-сервер: the local agent-facing surface and its journal.
fn page_mcp(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    switch(ui, app, "mcp.enabled", st::M_ENABLED, "");
    choice(
        ui,
        app,
        "mcp.profile",
        st::M_PROFILE,
        st::M_PROFILE_HINT,
        &[
            ("full", "full"),
            ("audit", "audit"),
            ("quick-check", "quick-check"),
        ],
    );
    switch(ui, app, "mcp.journal", st::M_JOURNAL, "");
    info_card(ui, &p, st::M_NOTE);
}

/// Агент: what the agent may see and do.
fn page_agent(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    switch(ui, app, "agent.run_scans", st::A_RUN_SCANS, "");
    switch(ui, app, "agent.change_tasks", st::A_TASKS, "");
    switch(ui, app, "agent.show_actions", st::A_SHOW, "");
    text(
        ui,
        app,
        "agent.projects",
        st::A_PROJECTS,
        st::A_PROJECTS_HINT,
    );
    ui.add_space(space::SM);
    if kit::button(
        ui,
        &p,
        Some(ph::SMART_TOY),
        st::A_CONFIG,
        kit::BtnKind::Outlined,
    )
    .clicked()
    {
        app.act("settings.page", json!({"id": "agent_config"}));
    }
    info_card(ui, &p, st::A_NOTE);
}

/// Конфигурация агента: quotas, masked keys and the proposal queue.
fn page_agent_config(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    group(ui, &p, st::A_QUOTAS);
    kit::card(ui, &p, |ui| {
        ui.label(RichText::new(st::A_NO_PROPOSALS).color(p.weak));
        // Nothing to decide while the proposal queue is empty.
        ui.scope_builder(egui::UiBuilder::new().disabled(), |ui| {
            ui.horizontal(|ui| {
                let _ = kit::button(ui, &p, None, st::A_ACCEPT, kit::BtnKind::Outlined);
                let _ = kit::button(ui, &p, None, st::A_REJECT, kit::BtnKind::Text);
            });
        });
    });
    group(ui, &p, st::A_JOURNAL);
    info_card(ui, &p, st::A_KEYS);
}

/// Уведомления: the 7×3 channel matrix plus quiet hours.
fn page_notify(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for label in [st::N_SOUND, st::N_SYSTEM, st::N_WINDOW] {
                ui.add_space(space::XS);
                ui.label(RichText::new(label).size(ty::CAPTION).color(p.faint));
            }
        });
    });
    for &(event, _) in crate::settings::NOTIFY_EVENTS {
        let title = st::n_event(event);
        let hint = st::n_event_hint(event);
        row_box(ui, &p, title, hint, |ui| {
            // Right-to-left region: declare z→w so the columns read
            // «В окне · Системные · Звук» like the header row.
            for channel in ["z", "s", "w"] {
                let key = format!("notify.{event}_{channel}");
                let mut on = app.model.settings.get_bool(&key);
                if ui.checkbox(&mut on, "").changed() {
                    set(app, &key, json!(on));
                }
            }
        });
    }
    switch(ui, app, "notify.quiet", st::N_QUIET, st::N_QUIET_HINT);
    number(ui, app, "notify.quiet_from", st::N_FROM, st::N_TIME_FORMAT);
    number(ui, app, "notify.quiet_to", st::N_TO, st::N_TIME_FORMAT);
}

/// Горячие клавиши: every binding editable; equal chords flagged as conflicts.
fn page_keys(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if kit::button(
                ui,
                &p,
                Some(ph::RESTART_ALT),
                st::K_RESET_SECTION,
                kit::BtnKind::Outlined,
            )
            .clicked()
            {
                app.act("settings.reset", json!({"prefix": "keys."}));
            }
        });
    });
    ui.add_space(space::SM);
    let mut groups: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
    for &(action, chord) in crate::settings::SHORTCUTS {
        let group_name = st::k_group(action);
        match groups.iter_mut().find(|(g, _)| *g == group_name) {
            Some((_, list)) => list.push((action, chord)),
            None => groups.push((group_name, vec![(action, chord)])),
        }
    }
    let chords: Vec<String> = crate::settings::SHORTCUTS
        .iter()
        .map(|(a, _)| app.model.settings.get_str(&format!("keys.{a}")))
        .collect();
    for (group_name, actions) in groups {
        group(ui, &p, group_name);
        for (action, default) in actions {
            let key = format!("keys.{action}");
            let value = app.model.settings.get_str(&key);
            let conflict = !value.is_empty() && chords.iter().filter(|c| *c == &value).count() > 1;
            let title = st::k_action(action);
            let shown = if value.is_empty() {
                default
            } else {
                value.as_str()
            };
            row_box(ui, &p, title, "", |ui| {
                if conflict {
                    let (bg, fg) = p.tone(crate::theme::tokens::Status::Error);
                    kit::badge_on(ui, bg, fg, st::K_CONFLICT);
                }
                let mut edit = app
                    .view
                    .settings
                    .texts
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| shown.to_string());
                let resp = ui.add(egui::TextEdit::singleline(&mut edit).desired_width(150.0));
                if resp.changed() {
                    app.view.settings.texts.insert(key.clone(), edit.clone());
                }
                if resp.lost_focus() && edit != shown {
                    set(app, &key, json!(edit));
                }
                kit::kbd(ui, &p, shown);
            });
        }
    }
    group(ui, &p, st::K_EXCHANGE);
    ui.horizontal(|ui| {
        let path = &mut app.view.settings.exchange_path;
        if path.is_empty() {
            *path = "settings.json".to_string();
        }
        ui.add(egui::TextEdit::singleline(path).desired_width(200.0));
        if kit::button(
            ui,
            &p,
            Some(ph::DOWNLOAD),
            st::K_EXPORT,
            kit::BtnKind::Outlined,
        )
        .clicked()
        {
            let path = app.view.settings.exchange_path.clone();
            app.act("settings.export", json!({"path": path}));
        }
        if kit::button(
            ui,
            &p,
            Some(ph::UPLOAD_FILE),
            st::K_IMPORT,
            kit::BtnKind::Outlined,
        )
        .clicked()
        {
            let path = app.view.settings.exchange_path.clone();
            app.act("settings.import", json!({"path": path}));
        }
    });
}

/// Хранилище и данные: retention, resets and the settings file exchange.
fn page_data(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    choice(
        ui,
        app,
        "data.keep_scans",
        st::D_KEEP,
        "",
        &[
            ("10", st::D_KEEP_OPTS[0]),
            ("20", st::D_KEEP_OPTS[1]),
            ("all", st::D_KEEP_OPTS[2]),
        ],
    );
    choice(
        ui,
        app,
        "data.html_age",
        st::D_HTML_AGE,
        "",
        &[
            ("30", st::D_HTML_OPTS[0]),
            ("90", st::D_HTML_OPTS[1]),
            ("never", st::D_HTML_OPTS[2]),
        ],
    );
    group(ui, &p, st::D_EXCHANGE);
    ui.horizontal(|ui| {
        let path = &mut app.view.settings.exchange_path;
        if path.is_empty() {
            *path = "settings.json".to_string();
        }
        ui.add(egui::TextEdit::singleline(path).desired_width(260.0));
        if kit::button(ui, &p, None, st::D_EXPORT, kit::BtnKind::Outlined).clicked() {
            let path = app.view.settings.exchange_path.clone();
            app.act("settings.export", json!({"path": path}));
        }
        if kit::button(ui, &p, None, st::D_IMPORT, kit::BtnKind::Outlined).clicked() {
            let path = app.view.settings.exchange_path.clone();
            app.act("settings.import", json!({"path": path}));
        }
    });
    info_card(ui, &p, st::D_EXPORT_NOTE);
    group(ui, &p, st::D_RESET);
    ui.horizontal(|ui| {
        if kit::button(ui, &p, None, st::D_RESET_VIEW, kit::BtnKind::Outlined).clicked() {
            app.act("settings.reset", json!({"prefix": "view."}));
        }
        if kit::button(ui, &p, None, st::D_RESET_ALL, kit::BtnKind::Outlined).clicked() {
            app.act("settings.reset", json!({"prefix": ""}));
        }
    });
    info_card(ui, &p, st::D_NOTE);
}

/// Обновления: channel and auto-check/download switches.
fn page_updates(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    kit::card(ui, &p, |ui| {
        for (label, value) in [
            (st::U_INSTALLED, env!("CARGO_PKG_VERSION").to_string()),
            (st::AB_CORE, env!("CARGO_PKG_VERSION").to_string()),
            (st::U_LAST_CHECK, st::U_NEVER.to_string()),
        ] {
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(ty::CAPTION).color(p.faint));
                ui.monospace(value);
            });
        }
    });
    segmented(
        ui,
        app,
        "updates.channel",
        st::U_CHANNEL,
        "",
        &[("stable", st::U_CHANNELS[0]), ("beta", st::U_CHANNELS[1])],
    );
    switch(ui, app, "updates.auto_check", st::U_AUTO, "");
    switch(ui, app, "updates.background_download", st::U_DOWNLOAD, "");
    ui.add_space(space::SM);
    if kit::button(
        ui,
        &p,
        Some(ph::CACHED),
        st::U_CHECK,
        kit::BtnKind::Outlined,
    )
    .clicked()
    {
        if let Some(reply) = app.act("settings.update_check", json!({})) {
            app.model.status = match reply.get("state").and_then(|s| s.as_str()) {
                Some("current") => st::U_CURRENT.into(),
                _ => st::U_UNAVAILABLE.into(),
            };
        }
    }
    group(ui, &p, st::U_CHANGELOG);
    info_card(ui, &p, st::U_NOTE);
}

/// О программе: versions and license notices.
fn page_about(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    kit::card(ui, &p, |ui| {
        ui.label(kit::bold(crate::i18n::APP_TITLE).size(ty::TITLE));
        ui.horizontal(|ui| {
            ui.label(RichText::new(st::AB_VERSION).color(p.faint));
            ui.monospace(env!("CARGO_PKG_VERSION"));
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new(st::AB_CORE).color(p.faint));
            ui.monospace(env!("CARGO_PKG_VERSION"));
        });
    });
    ui.add_space(space::SM);
    group(ui, &p, st::AB_LICENSES);
    info_card(ui, &p, st::AB_LICENSE_NOTE);
}

/// A muted note card.
fn info_card(ui: &mut egui::Ui, p: &Palette, text: &str) {
    ui.add_space(space::XS);
    kit::card(ui, p, |ui| {
        ui.add(egui::Label::new(RichText::new(text).size(ty::CAPTION).color(p.weak)).wrap());
    });
}

fn ui_add_space(ui: &mut egui::Ui) {
    ui.add_space(space::MD);
}
