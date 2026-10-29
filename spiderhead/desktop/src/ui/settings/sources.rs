//! Settings → Источники данных: the provider list, per-provider detail and
//! key capture, the methods catalogue and the spend page. Credential values
//! never cross into the UI: rows show only the set/not-set state the
//! `sources.list` command reports, and saving goes through
//! `sources.credential` → `credentials::write_secret`.

use eframe::egui::{self, RichText};
use serde_json::json;

use crate::i18n::settings as st;
use crate::settings::Access;
use crate::theme::tokens::Status;
use crate::theme::{space, ty, Palette};
use crate::ui::kit::{self, icons as ph};
use crate::ui::DesktopApp;
/// Refresh the provider snapshot once per visit (an explicit, user-started read).
fn providers(app: &mut DesktopApp) -> serde_json::Value {
    if app.view.settings.providers.is_none() {
        app.view.settings.providers = app.act("sources.list", json!({}));
    }
    app.view.settings.providers.clone().unwrap_or(json!([]))
}

/// State → status-chip tone.
fn state_tone(state: &str) -> Status {
    match state {
        "configured_unverified" | "verified" | "ready" | "connected" => Status::Done,
        "invalid" | "failed" | "expired" | "revoked" => Status::Error,
        "not_required" => Status::Queued,
        _ => Status::Unavailable,
    }
}

/// One provider row: name, access kind, state chip, configure button.
fn provider_row(
    ui: &mut egui::Ui,
    app: &mut DesktopApp,
    p: &Palette,
    provider: &serde_json::Value,
) {
    let id = provider.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let title = provider.get("title").and_then(|v| v.as_str()).unwrap_or(id);
    let access = provider
        .get("access")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let state = provider.get("state").and_then(|v| v.as_str()).unwrap_or("");
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width() - 320.0);
            ui.label(kit::bold(title));
            ui.label(
                RichText::new(st::src_access(access))
                    .size(ty::CAPTION)
                    .color(p.weak),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if kit::button(ui, p, None, st::SRC_OPEN, kit::BtnKind::Text).clicked() {
                app.act(
                    "settings.page",
                    json!({"id": "source_detail", "provider": id}),
                );
            }
            kit::status_chip(ui, p, state_tone(state), st::src_state(state));
        });
    });
    ui.add_space(space::XS);
    ui.separator();
    ui.add_space(space::XS);
}

/// The sources list with the spend/methods header actions.
pub fn page_sources(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    let list = providers(app);
    ui.horizontal(|ui| {
        if kit::button(
            ui,
            &p,
            Some(ph::KEY),
            st::SRC_METHODS,
            kit::BtnKind::Outlined,
        )
        .clicked()
        {
            app.act("settings.page", json!({"id": "sources_auth"}));
        }
        if kit::button(
            ui,
            &p,
            Some(ph::ANALYTICS),
            st::SRC_SPEND,
            kit::BtnKind::Outlined,
        )
        .clicked()
        {
            app.act("settings.page", json!({"id": "spend"}));
        }
    });
    ui.add_space(space::SM);
    let providers = list.as_array().cloned().unwrap_or_default();
    let connected = providers
        .iter()
        .filter(|v| v.get("state").and_then(|s| s.as_str()) == Some("configured_unverified"))
        .count();
    ui.label(
        RichText::new(format!(
            "{} / {}  ·  {}",
            connected,
            providers.len(),
            st::SRC_SECRET_NOTE
        ))
        .size(ty::CAPTION)
        .color(p.weak),
    );
    ui.add_space(space::SM);
    for provider in &providers {
        provider_row(ui, app, &p, provider);
    }
}

/// One provider's detail page: the access flow and per-component state.
pub fn page_source_detail(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    if ui
        .button(RichText::new(st::SRC_BACK).color(p.accent))
        .clicked()
    {
        app.act("settings.page", json!({"id": "sources"}));
        return;
    }
    ui.add_space(space::SM);
    let id = app.model.settings_provider.clone().unwrap_or_default();
    let list = providers(app);
    let Some(provider) = list
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|v| v.get("id").and_then(|i| i.as_str()) == Some(&id))
        })
        .cloned()
    else {
        ui.label(RichText::new(id).color(p.weak));
        return;
    };
    let title = provider.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let access = provider
        .get("access")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    ui.label(kit::bold(title).size(ty::HEADING));
    let flow = match access {
        "oauth" => st::SRC_OAUTH_FLOW,
        "service_account" => st::SRC_SA_FLOW,
        _ => st::SRC_KEY_FLOW,
    };
    ui.add_space(space::XS);
    ui.add(egui::Label::new(RichText::new(flow).color(p.weak)).wrap());
    ui.add_space(space::SM);
    let components = provider
        .get("components")
        .and_then(|c| c.as_object())
        .cloned()
        .unwrap_or_default();
    for (name, is_set) in &components {
        let set = is_set.as_bool().unwrap_or(false);
        ui.horizontal(|ui| {
            ui.label(st::src_component(name));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if kit::button(ui, &p, None, st::SRC_CONNECT, kit::BtnKind::Outlined).clicked() {
                    app.view.settings.secret.clear();
                    app.view.settings.component = name.clone();
                    app.act("settings.page", json!({"id": "source_key", "provider": id}));
                }
                let tone = if set {
                    Status::Done
                } else {
                    Status::Unavailable
                };
                kit::status_chip(ui, &p, tone, if set { st::SRC_SET } else { st::SRC_UNSET });
            });
        });
        ui.add_space(space::XS);
    }
    if components.is_empty() {
        ui.label(
            RichText::new(st::SRC_SECRET_NOTE)
                .size(ty::CAPTION)
                .color(p.weak),
        );
    }
    ui.add_space(space::SM);
    info(ui, &p, st::SRC_SECRET_NOTE);
}

/// The key-capture page: a password field, save through the credential store,
/// revoke for a stored one. The typed value is wiped right after dispatch.
pub fn page_source_key(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    if ui
        .button(RichText::new(st::SRC_BACK).color(p.accent))
        .clicked()
    {
        let provider = app.model.settings_provider.clone().unwrap_or_default();
        app.act(
            "settings.page",
            json!({"id": "source_detail", "provider": provider}),
        );
        return;
    }
    ui.add_space(space::SM);
    let provider_id = app.model.settings_provider.clone().unwrap_or_default();
    // Landing on the page through the control channel names no component:
    // fall back to the provider's first credential part.
    let component = if app.view.settings.component.is_empty() {
        crate::settings::provider(&provider_id)
            .and_then(|p| p.components.first())
            .map(|c| c.name.to_string())
            .unwrap_or_default()
    } else {
        app.view.settings.component.clone()
    };
    let title = crate::settings::provider(&provider_id)
        .map(|p| p.title)
        .unwrap_or(provider_id.as_str());
    ui.label(kit::bold(format!("{title} · {}", st::src_component(&component))).size(ty::HEADING));
    ui.add_space(space::SM);
    ui.add(
        egui::TextEdit::singleline(&mut app.view.settings.secret)
            .password(true)
            .hint_text(st::SRC_KEY_PLACEHOLDER)
            .desired_width(320.0),
    );
    ui.add_space(space::SM);
    ui.horizontal(|ui| {
        let save =
            kit::button(ui, &p, Some(ph::CHECK), st::SRC_SAVE, kit::BtnKind::Primary).clicked();
        let revoke = kit::button(
            ui,
            &p,
            Some(ph::DELETE),
            st::SRC_REVOKE,
            kit::BtnKind::Outlined,
        )
        .clicked();
        if save && !app.view.settings.secret.is_empty() {
            let value = std::mem::take(&mut app.view.settings.secret);
            let out = app.act(
                "sources.credential",
                json!({
                    "provider": provider_id,
                    "component": component,
                    "action": "set",
                    "value": value,
                }),
            );
            if out.is_some() {
                app.view.settings.providers = None;
                app.act(
                    "settings.page",
                    json!({"id": "source_detail", "provider": provider_id}),
                );
            }
        }
        if revoke {
            let out = app.act(
                "sources.credential",
                json!({
                    "provider": provider_id,
                    "component": component,
                    "action": "revoke",
                }),
            );
            if out.is_some() {
                app.view.settings.providers = None;
            }
        }
    });
    ui.add_space(space::SM);
    info(ui, &p, st::SRC_SECRET_NOTE);
}

/// The methods catalogue: one row per provider with its access kind.
pub fn page_sources_auth(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    if ui
        .button(RichText::new(st::SRC_BACK).color(p.accent))
        .clicked()
    {
        app.act("settings.page", json!({"id": "sources"}));
        return;
    }
    ui.add_space(space::SM);
    for provider in crate::settings::providers() {
        let access = match provider.access {
            Access::OAuth => "oauth",
            Access::ApiKey => "api_key",
            Access::ServiceAccount => "service_account",
            Access::Public => "public",
        };
        ui.horizontal(|ui| {
            ui.label(provider.title);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if kit::button(ui, &p, None, st::SRC_OPEN, kit::BtnKind::Text).clicked() {
                    app.act(
                        "settings.page",
                        json!({"id": "source_detail", "provider": provider.id}),
                    );
                }
                ui.label(RichText::new(st::src_access(access)).color(p.weak));
            });
        });
        ui.add_space(space::XS);
        ui.separator();
        ui.add_space(space::XS);
    }
}

/// Расходы и лимиты: the spend report of paid providers.
pub fn page_spend(ui: &mut egui::Ui, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    if ui
        .button(RichText::new(st::SRC_BACK).color(p.accent))
        .clicked()
    {
        app.act("settings.page", json!({"id": "sources"}));
        return;
    }
    ui.add_space(space::SM);
    if app.view.settings.spend.is_none() {
        app.view.settings.spend = app.act("sources.spend", json!({}));
    }
    let spend = app.view.settings.spend.clone().unwrap_or(json!({}));
    kit::caption(ui, &p, st::SP_MONTH);
    ui.add_space(space::XS);
    let calls = spend.get("calls").and_then(|v| v.as_i64()).unwrap_or(0);
    ui.label(RichText::new(st::sp_calls(calls)).size(ty::BODY));
    ui.add_space(space::SM);
    kit::caption(ui, &p, st::SP_OPS);
    ui.add_space(space::XS);
    let by_source = spend
        .get("by_source")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    if by_source.is_empty() {
        ui.label(RichText::new(st::SP_EMPTY).color(p.weak));
    } else {
        for (name, units) in &by_source {
            ui.horizontal(|ui| {
                ui.label(kit::bold(name));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(obj) = units.as_object() {
                        let text = obj
                            .iter()
                            .map(|(unit, v)| format!("{unit}: {}", v))
                            .collect::<Vec<_>>()
                            .join(" · ");
                        ui.monospace(text);
                    }
                });
            });
            ui.add_space(space::XS);
        }
    }
}

/// A bordered note card.
fn info(ui: &mut egui::Ui, p: &Palette, text: &str) {
    kit::card(ui, p, |ui| {
        ui.add(egui::Label::new(RichText::new(text).size(ty::CAPTION).color(p.weak)).wrap());
    });
}
