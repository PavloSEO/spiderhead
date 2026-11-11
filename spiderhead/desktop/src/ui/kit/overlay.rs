//! Overlays and feedback: modal, popover menu, toast, banner, progress and
//! skeleton. Frames are drawn by plain functions so the gallery can show them
//! inline; `modal` and `toast_stack` are the live hosts.

use egui::{
    pos2, vec2, Align, Align2, Color32, Context, CornerRadius, FontId, Id, Layout, Order, Rect,
    RichText, Sense, Stroke, Ui,
};

use super::controls::{button, BtnKind};
use super::icons as ph;
use super::{bold, icon_button};
use crate::theme::tokens::Status;
use crate::theme::{control, radius, semibold, space, ty, Palette};

/// Severity of a toast or banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Neutral news.
    Info,
    /// Something finished.
    Success,
    /// Needs attention.
    Warning,
    /// Something failed.
    Error,
}

impl Tone {
    /// Every tone, in gallery order.
    pub const ALL: [Tone; 4] = [Tone::Info, Tone::Success, Tone::Warning, Tone::Error];

    fn status(self) -> Status {
        match self {
            Tone::Info => Status::Running,
            Tone::Success => Status::Done,
            Tone::Warning => Status::Warning,
            Tone::Error => Status::Error,
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Tone::Info => ph::INFO,
            Tone::Success => ph::CHECK_CIRCLE,
            Tone::Warning => ph::WARNING,
            Tone::Error => ph::ERROR,
        }
    }
}

/// One selectable row of a popover menu.
pub struct MenuItem<'a> {
    /// Row text.
    pub label: &'a str,
    /// Leading glyph; a checked row shows a tick in its place.
    pub icon: Option<&'a str>,
    /// Right-aligned shortcut or note.
    pub hint: Option<&'a str>,
    /// Marks the current choice.
    pub checked: bool,
    /// Paints the row in the error colour.
    pub danger: bool,
}

impl<'a> MenuItem<'a> {
    /// A plain row.
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            icon: None,
            hint: None,
            checked: false,
            danger: false,
        }
    }
}

/// A row or a separator of a popover menu.
pub enum Entry<'a> {
    /// A selectable row.
    Item(MenuItem<'a>),
    /// A hairline between groups.
    Sep,
}

/// Menu rows inside the current ui; returns the index of the clicked entry.
/// Wrap it in `egui::Popup::menu` for a live menu.
pub fn popover_menu(ui: &mut Ui, p: &Palette, entries: &[Entry<'_>]) -> Option<usize> {
    ui.vertical(|ui| {
        ui.set_min_width(200.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut picked = None;
        for (i, entry) in entries.iter().enumerate() {
            match entry {
                Entry::Sep => {
                    ui.add_space(space::XS);
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                    ui.painter().line_segment(
                        [rect.left_center(), rect.right_center()],
                        Stroke::new(1.0_f32, p.border_soft),
                    );
                    ui.add_space(space::XS);
                }
                Entry::Item(item) => {
                    let (rect, response) = ui.allocate_exact_size(
                        vec2(ui.available_width(), control::ITEM_H),
                        Sense::click(),
                    );
                    if response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(radius::SM),
                            p.menu_hover,
                        );
                    }
                    let ink = if item.danger { p.critical } else { p.text };
                    let lead = if item.checked {
                        Some(ph::CHECK)
                    } else {
                        item.icon
                    };
                    if let Some(g) = lead {
                        let tone = if item.checked { p.accent } else { p.weak };
                        ui.painter().text(
                            rect.left_center() + vec2(8.0, 0.0),
                            Align2::LEFT_CENTER,
                            g,
                            FontId::new(control::ICON, egui::FontFamily::Proportional),
                            tone,
                        );
                    }
                    ui.painter().text(
                        rect.left_center() + vec2(34.0, 0.0),
                        Align2::LEFT_CENTER,
                        item.label,
                        FontId::new(ty::BODY, egui::FontFamily::Proportional),
                        ink,
                    );
                    if let Some(hint) = item.hint {
                        ui.painter().text(
                            rect.right_center() - vec2(8.0, 0.0),
                            Align2::RIGHT_CENTER,
                            hint,
                            FontId::new(ty::CAPTION, egui::FontFamily::Proportional),
                            p.faint,
                        );
                    }
                    if response.clicked() {
                        picked = Some(i);
                    }
                    response.on_hover_cursor(egui::CursorIcon::PointingHand);
                }
            }
        }
        picked
    })
    .inner
}

/// Modal card: header with title and close, body, footer with right-aligned
/// buttons. Returns true when the close button was clicked.
pub fn modal_frame(
    ui: &mut Ui,
    p: &Palette,
    title: &str,
    width: f32,
    body: impl FnOnce(&mut Ui),
    footer: impl FnOnce(&mut Ui),
) -> bool {
    let mut closed = false;
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(radius::DIALOG))
        .shadow(ui.visuals().popup_shadow)
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing = vec2(space::SM, space::SM);
                let pad = egui::Margin::symmetric(space::LG as i8, space::SM as i8);
                egui::Frame::new().inner_margin(pad).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(bold(title).size(ty::TITLE));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            closed = icon_button(ui, p, ph::CLOSE, "").clicked();
                        });
                    });
                });
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(space::LG as i8, space::XS as i8))
                    .show(ui, |ui| {
                        ui.set_width(width - 2.0 * space::LG);
                        body(ui);
                    });
                ui.add_space(space::SM);
                let line = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover()).0;
                ui.painter().rect_filled(line, 0.0, p.border_soft);
                egui::Frame::new().inner_margin(pad).show(ui, |ui| {
                    ui.set_width(width - 2.0 * space::LG);
                    ui.with_layout(Layout::right_to_left(Align::Center), footer);
                });
            });
        });
    closed
}

/// Live modal: scrim over the window, card centred; Esc or × closes.
/// Returns true on the frame it was dismissed.
pub fn modal(
    ctx: &Context,
    p: &Palette,
    id: &str,
    title: &str,
    body: impl FnOnce(&mut Ui),
    footer: impl FnOnce(&mut Ui),
) -> bool {
    let screen = ctx.content_rect();
    egui::Area::new(Id::new(id).with("scrim"))
        .order(Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.allocate_rect(screen, Sense::click_and_drag());
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(control::SCRIM_ALPHA));
        });
    let mut closed = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    egui::Area::new(Id::new(id))
        .order(Order::Foreground)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            closed |= modal_frame(ui, p, title, control::MODAL_W, body, footer);
        });
    closed
}

/// One toast card; returns true when its × was clicked.
pub fn toast(ui: &mut Ui, p: &Palette, tone: Tone, title: &str, text: &str) -> bool {
    let (bg, fg) = p.tone(tone.status());
    let mut closed = false;
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(radius::MD))
        .shadow(ui.visuals().popup_shadow)
        .inner_margin(egui::Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.set_width(control::TOAST_W - 2.0 * space::MD);
            ui.horizontal_top(|ui| {
                let (chip, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
                ui.painter()
                    .rect_filled(chip, CornerRadius::same(radius::SM), bg);
                ui.painter().text(
                    chip.center(),
                    Align2::CENTER_CENTER,
                    tone.icon(),
                    FontId::new(control::ICON, egui::FontFamily::Proportional),
                    fg,
                );
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - 28.0);
                    ui.label(bold(title));
                    ui.add(
                        egui::Label::new(RichText::new(text).color(p.weak).size(ty::CAPTION))
                            .wrap(),
                    );
                });
                closed = icon_button(ui, p, ph::CLOSE, "").clicked();
            });
        });
    closed
}

/// Bottom-right corner host for toasts; `add` draws the cards.
pub fn toast_stack(ctx: &Context, add: impl FnOnce(&mut Ui)) {
    egui::Area::new(Id::new("toast-stack"))
        .order(Order::Tooltip)
        .anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -36.0))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = space::SM;
            add(ui);
        });
}

/// Inline banner with optional text actions; returns the clicked action.
pub fn banner(ui: &mut Ui, p: &Palette, tone: Tone, text: &str, actions: &[&str]) -> Option<usize> {
    let (bg, fg) = p.tone(tone.status());
    let mut clicked = None;
    egui::Frame::new()
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, Palette::tint(fg, 70)))
        .corner_radius(CornerRadius::same(radius::MD))
        .inner_margin(egui::Margin::symmetric(space::MD as i8, space::SM as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(tone.icon()).size(control::ICON).color(fg));
                ui.label(
                    RichText::new(text)
                        .color(fg)
                        .family(semibold())
                        .size(ty::BODY),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    for (i, label) in actions.iter().enumerate().rev() {
                        if button(ui, p, None, label, BtnKind::Text).clicked() {
                            clicked = Some(i);
                        }
                    }
                });
            });
        });
    clicked
}

/// Linear progress: `Some(0..=1)` is determinate, `None` slides.
pub fn progress_bar(ui: &mut Ui, p: &Palette, fraction: Option<f32>, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, control::BAR_H), Sense::hover());
    let round = CornerRadius::same(control::BAR_H as u8 / 2);
    ui.painter().rect_filled(rect, round, p.accent_soft);
    let fill = match fraction {
        Some(f) => Rect::from_min_size(
            rect.min,
            vec2(rect.width() * f.clamp(0.0, 1.0), rect.height()),
        ),
        None => {
            let t = (ui.input(|i| i.time) * 0.8).fract() as f32;
            let w = rect.width() * 0.3;
            let x = rect.left() + (rect.width() + w) * t - w;
            Rect::from_min_max(
                pos2(x.max(rect.left()), rect.top()),
                pos2((x + w).min(rect.right()), rect.bottom()),
            )
        }
    };
    ui.painter().rect_filled(fill, round, p.accent);
    if fraction.is_none() {
        ui.ctx().request_repaint();
    }
}

/// Placeholder block that pulses while data loads.
pub fn skeleton(ui: &mut Ui, p: &Palette, width: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let pulse = (ui.input(|i| i.time) * 2.0).sin() as f32 * 0.5 + 0.5;
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(radius::SM),
        p.hover.gamma_multiply(0.6 + 0.4 * pulse),
    );
    ui.ctx().request_repaint();
}
