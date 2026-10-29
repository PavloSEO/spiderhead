//! Application shell: top bar, side navigation with the profile button, ⌘K
//! palette, dialogs and the status bar. Every widget action goes through
//! `AppModel::dispatch` so the control channel and the window never diverge.

mod dialogs;
mod palette;
mod sidenav;
mod topbar;

pub use dialogs::windows;
pub use sidenav::navigation;
pub use topbar::top_bar;

use eframe::egui::{self, RichText};

use crate::i18n;
use crate::theme::{row, space, ty, Theme};
use crate::ui::kit::icons as ph;
use crate::ui::DesktopApp;

/// Bottom status strip: read-only marker, scan name and URL count on the
/// left, display mode and core version on the right. Never shows a path.
pub fn status_bar(ctx: &egui::Context, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    let small =
        |text: String, color: egui::Color32| RichText::new(text).size(ty::CAPTION).color(color);
    egui::TopBottomPanel::bottom("status")
        .exact_height(row::STATUS_BAR)
        .frame(
            egui::Frame::new()
                .fill(p.nav)
                .stroke(egui::Stroke::new(1.0_f32, p.border))
                .inner_margin(egui::Margin::symmetric(space::MD as i8, 0)),
        )
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                match &app.model.scan {
                    Some(scan) => {
                        ui.label(small(
                            format!("{}  {}", ph::LOCK, i18n::STATUS_READONLY),
                            p.faint,
                        ));
                        ui.label(small(format!("·  {}", scan.display_name()), p.text));
                        let total = app
                            .model
                            .table
                            .last
                            .as_ref()
                            .and_then(|l| l.get("total"))
                            .and_then(|v| v.as_i64());
                        if let Some(n) = total {
                            ui.label(small(format!("·  {}", i18n::status_urls(n)), p.weak));
                        }
                    }
                    None => {
                        ui.label(small(i18n::STATUS_NO_SCAN.into(), p.faint));
                    }
                }
                if !app.model.status.is_empty() {
                    ui.label(small(format!("·  {}", app.model.status), p.warning));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(small(i18n::status_core(env!("CARGO_PKG_VERSION")), p.weak));
                    let mode =
                        i18n::mode_label(app.model.mode.map(|m| m.name()).unwrap_or("start"));
                    ui.label(small(mode.to_string(), p.weak));
                    let icon = match app.model.theme {
                        Theme::Dark => ph::DARK_MODE,
                        _ => ph::PALETTE,
                    };
                    ui.label(small(
                        format!("{icon}  {}", i18n::theme_label(app.model.theme)),
                        p.weak,
                    ));
                });
            });
        });
}
