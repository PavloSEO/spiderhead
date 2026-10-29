//! Buttons, icon buttons, key caps and status chips of the canvas kit.

use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Response, Sense, Stroke, Ui};

use crate::theme::tokens::Status;
use crate::theme::{radius, semibold, ty, Palette};

/// Visual weight of a button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtnKind {
    /// Filled with the primary colour; one per screen.
    Primary,
    /// Outlined, neutral.
    Outlined,
    /// No frame.
    Text,
}

/// Dense button: 28 high, optional leading icon; an empty label leaves an
/// icon-only square-ish button.
pub fn button(
    ui: &mut Ui,
    p: &Palette,
    icon: Option<&str>,
    label: &str,
    kind: BtnKind,
) -> Response {
    let font = FontId::new(ty::CONTROL, semibold());
    let ink = match kind {
        BtnKind::Primary => p.on_accent,
        _ => p.text,
    };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, ink);
    let icon_w = if icon.is_some() { 26.0 } else { 0.0 };
    let pad = if label.is_empty() { 10.0 } else { 12.0 };
    let icon_w = if label.is_empty() {
        icon_w - 8.0
    } else {
        icon_w
    };
    let size = vec2(galley.size().x + icon_w + 2.0 * pad, 28.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let (fill, stroke) = match kind {
        BtnKind::Primary => (
            if response.hovered() {
                Palette::tint(p.accent, 230)
            } else {
                p.accent
            },
            Stroke::new(1.0_f32, p.accent),
        ),
        BtnKind::Outlined => (
            if response.hovered() {
                p.hover
            } else {
                p.surface
            },
            Stroke::new(1.0_f32, p.outline),
        ),
        BtnKind::Text => (
            if response.hovered() {
                p.hover
            } else {
                Color32::TRANSPARENT
            },
            Stroke::NONE,
        ),
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(radius::MD),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    let mut x = rect.left() + pad;
    if let Some(icon) = icon {
        ui.painter().text(
            pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            icon,
            FontId::new(18.0, egui::FontFamily::Proportional),
            ink,
        );
        x += icon_w;
    }
    ui.painter().galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        ink,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// 36-point square icon button with a tooltip.
pub fn icon_button(ui: &mut Ui, p: &Palette, icon: &str, tip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::MD), p.hover);
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon,
        FontId::new(20.0, egui::FontFamily::Proportional),
        p.weak,
    );
    response
        .on_hover_text(tip)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Key cap (`⌘K`).
pub fn kbd(ui: &mut Ui, p: &Palette, text: &str) {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        FontId::new(11.0, egui::FontFamily::Monospace),
        p.weak,
    );
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(12.0, 6.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(5), p.nav);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, p.weak);
}

/// Status pill of the canvas: tone background, icon and label (28 high).
pub fn status_chip(ui: &mut Ui, p: &Palette, status: Status, label: &str) -> Response {
    let (bg, fg) = p.tone(status);
    let icon = crate::ui::kit::icons::by_name(status.tone().icon).unwrap_or("");
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), FontId::new(ty::CAPTION, semibold()), fg);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 40.0, 28.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(14),
        bg,
        Stroke::new(if bg == Color32::WHITE { 1.0_f32 } else { 0.0 }, fg),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        pos2(rect.left() + 11.0, rect.center().y),
        Align2::LEFT_CENTER,
        icon,
        FontId::new(16.0, egui::FontFamily::Proportional),
        fg,
    );
    ui.painter().galley(
        pos2(rect.left() + 31.0, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
    response
}
