//! Краулер mode: open or run a scan, URL inspector, issues, HTML search and
//! scan comparison.

pub mod check;
pub mod compare;
pub mod compare_url;
pub mod dashboard;
mod detail_tabs;
mod details;
pub mod search;
pub mod sections;
pub mod url_grid;
pub mod urls;

use crate::ui::kit::icons as ph;
use eframe::egui;
use serde_json::json;

use crate::i18n;
use crate::theme::{space, Palette};
use crate::ui::kit;
use crate::ui::DesktopApp;

/// Quick-crawl page (profile menu target): bounded engine state + a way to
/// open a finished scan without the control channel.
pub fn home(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    ui.vertical_centered(|ui| {
        ui.add_space(space::XL * 2.5);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(64.0, 64.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 32.0, Palette::tint(p.accent, 30));
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            ph::TRAVEL_EXPLORE,
            egui::FontId::new(30.0, egui::FontFamily::Proportional),
            p.accent,
        );
        ui.add_space(space::MD);
        ui.label(kit::bold(i18n::CRAWLER_TITLE).size(crate::theme::ty::TITLE));
        ui.add_space(space::XS);
        ui.label(egui::RichText::new(i18n::CRAWLER_PENDING).color(p.weak));
        ui.add_space(space::XL);
        kit::card(ui, p, |ui| {
            ui.set_width(520.0);
            ui.label(egui::RichText::new(i18n::CRAWLER_OPEN_LABEL).color(p.weak));
            ui.horizontal(|ui| {
                let edit = egui::TextEdit::singleline(&mut app.view.crawl_path)
                    .desired_width(ui.available_width() - 130.0)
                    .margin(egui::vec2(10.0, 8.0))
                    .hint_text("…/scan.sqlite");
                let response = ui.add(edit);
                let open = ui.add(
                    egui::Button::new(
                        kit::bold(format!("{}  {}", ph::FOLDER_OPEN, i18n::CRAWLER_OPEN))
                            .color(p.on_accent),
                    )
                    .fill(p.accent),
                );
                let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (open.clicked() || enter) && !app.view.crawl_path.trim().is_empty() {
                    app.act(
                        "scan.open",
                        json!({"path": app.view.crawl_path.trim().to_string()}),
                    );
                    app.act("ui.navigate", json!({"id": "urls"}));
                }
            });
        });
        if !app.model.status.is_empty() {
            ui.add_space(space::SM);
            ui.label(egui::RichText::new(&app.model.status).color(p.critical));
        }
    });
}
