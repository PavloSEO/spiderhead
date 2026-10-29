//! The start screen («Что вы хотите сделать?»): three mode cards that all
//! dispatch `ui.mode`. Drawn instead of the shell while `model.mode` is None.

use eframe::egui::{self, vec2, Align2, CornerRadius, FontId, Rect, RichText, Sense};
use serde_json::json;

use crate::app::Mode;
use crate::i18n;
use crate::theme::{radius, semibold, space, ty, Palette};
use crate::ui::kit::{self, icons as ph};
use crate::ui::DesktopApp;

/// Where the test build of the modes that are not released yet can be downloaded.
/// Empty hides the download button.
pub const TEST_BUILD_URL: &str = "";

/// One chooser card: mode id, icon, title, description, available in this release.
const CARDS: [(Mode, &str, &str, &str, bool); 3] = [
    (
        Mode::Crawler,
        ph::TRAVEL_EXPLORE,
        i18n::MODE_CRAWLER,
        i18n::START_CRAWLER_D,
        false,
    ),
    (
        Mode::Logs,
        ph::DESCRIPTION,
        i18n::MODE_LOGS,
        i18n::START_LOGS_D,
        true,
    ),
    (
        Mode::Agent,
        ph::WORKSPACES,
        i18n::MODE_AGENT_CARD,
        i18n::START_AGENT_D,
        false,
    ),
];

/// Draw the chooser over the whole window (no shell chrome around it).
pub fn show(ctx: &egui::Context, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(p.bg))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                let top = (ui.available_height() - 420.0).max(48.0) * 0.5;
                ui.add_space(top);
                ui.label(
                    RichText::new(i18n::APP_TITLE.to_uppercase())
                        .size(ty::OVERLINE)
                        .family(semibold())
                        .color(p.accent),
                );
                ui.add_space(space::SM);
                ui.label(kit::bold(i18n::START_TITLE).size(ty::HEADING + 4.0));
                ui.add_space(space::XS);
                ui.label(RichText::new(i18n::START_SUB).color(p.weak));
                ui.add_space(space::XL);
                ui.horizontal(|ui| {
                    let width = 640.0_f32.min(ui.available_width());
                    let gap = space::MD;
                    let card_w = (width - gap * 2.0) / 3.0;
                    let left = (ui.available_width() - width).max(0.0) / 2.0;
                    ui.add_space(left);
                    for (mode, icon, title, desc, enabled) in CARDS {
                        if mode_card(ui, &p, icon, title, desc, card_w, enabled) {
                            app.act("ui.mode", json!({"mode": mode.name()}));
                        }
                        ui.add_space(gap);
                    }
                });
                ui.add_space(space::XL);
                ui.label(
                    RichText::new(i18n::START_HINT)
                        .size(ty::CAPTION)
                        .color(p.faint),
                );
            });
        });
}

/// A mode card: tinted icon, title, wrapped description; true when picked.
fn mode_card(
    ui: &mut egui::Ui,
    p: &Palette,
    icon: &str,
    title: &str,
    desc: &str,
    width: f32,
    enabled: bool,
) -> bool {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(width, 150.0), sense);
    let hovered = enabled && response.hovered();
    let fill = if hovered { p.hover } else { p.surface };
    let stroke = if hovered { p.accent } else { p.border };
    ui.painter().rect(
        rect,
        CornerRadius::same(radius::LG),
        fill,
        egui::Stroke::new(if hovered { 1.5_f32 } else { 1.0_f32 }, stroke),
        egui::StrokeKind::Inside,
    );
    let chip = Rect::from_center_size(rect.left_top() + vec2(30.0, 32.0), vec2(34.0, 34.0));
    ui.painter()
        .rect_filled(chip, CornerRadius::same(radius::MD), p.accent_soft);
    ui.painter().text(
        chip.center(),
        Align2::CENTER_CENTER,
        icon,
        FontId::new(20.0, egui::FontFamily::Proportional),
        p.on_selected,
    );
    ui.painter().text(
        rect.left_top() + vec2(52.0, 34.0),
        Align2::LEFT_CENTER,
        title,
        FontId::new(ty::BODY + 1.0, semibold()),
        p.text,
    );
    let text = ui.painter().layout(
        desc.to_owned(),
        FontId::new(ty::CAPTION + 0.5, egui::FontFamily::Proportional),
        p.weak,
        rect.width() - 32.0,
    );
    ui.painter()
        .galley(rect.left_top() + vec2(16.0, 64.0), text, p.weak);
    if !enabled {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(radius::LG),
            p.bg.gamma_multiply(0.55),
        );
        ui.painter().text(
            rect.right_top() + vec2(-14.0, 20.0),
            Align2::RIGHT_CENTER,
            i18n::START_SOON,
            FontId::new(ty::CAPTION, semibold()),
            p.weak,
        );
        if !TEST_BUILD_URL.is_empty() {
            let button = Rect::from_min_size(
                rect.left_bottom() + vec2(16.0, -38.0),
                vec2(rect.width() - 32.0, 26.0),
            );
            let label = RichText::new(i18n::START_DOWNLOAD).size(ty::CAPTION);
            if ui.put(button, egui::Button::new(label)).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(TEST_BUILD_URL));
            }
        }
        return false;
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}
