//! Form controls of the kit: text field, select, checkbox, switch, segmented
//! control and the tooltip helper. Sizes and colours come from `theme` only.

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Rect, Response, RichText, Sense,
    Shape, Stroke, StrokeKind, Ui,
};

use super::icons as ph;
use super::overlay::{popover_menu, Entry, MenuItem};
use crate::theme::{control, radius, semibold, ty, Palette};

/// Options of [`text_field`].
pub struct FieldOpts<'a> {
    /// Leading glyph (search, link).
    pub icon: Option<&'a str>,
    /// Placeholder shown while the field is empty.
    pub hint: &'a str,
    /// Outer width in points.
    pub width: f32,
    /// Draws the error outline.
    pub invalid: bool,
    /// Shows the value but refuses edits.
    pub read_only: bool,
    /// Draws the focus outline without keyboard focus (gallery states).
    pub focused: bool,
}

impl Default for FieldOpts<'_> {
    fn default() -> Self {
        Self {
            icon: None,
            hint: "",
            width: 220.0,
            invalid: false,
            read_only: false,
            focused: false,
        }
    }
}

fn glyph(ui: &Ui, at: egui::Pos2, icon: &str, color: Color32) {
    ui.painter().text(
        at,
        Align2::LEFT_CENTER,
        icon,
        FontId::new(control::ICON, egui::FontFamily::Proportional),
        color,
    );
}

/// The outlined box of a field-like control: error, focus and fill states.
fn field_box(ui: &Ui, p: &Palette, rect: Rect, focused: bool, invalid: bool, flat: bool) -> Shape {
    let (fill, stroke) = if invalid {
        (p.surface, Stroke::new(1.5_f32, p.critical))
    } else if focused {
        (p.surface, Stroke::new(1.5_f32, p.accent))
    } else if flat || !ui.is_enabled() {
        (p.surface_hi, Stroke::new(1.0_f32, p.border))
    } else {
        (p.surface, Stroke::new(1.0_f32, p.outline))
    };
    egui::epaint::RectShape::new(
        rect,
        CornerRadius::same(radius::SM),
        fill,
        stroke,
        StrokeKind::Inside,
    )
    .into()
}

/// Single-line text field with an optional leading icon and placeholder.
pub fn text_field(ui: &mut Ui, p: &Palette, text: &mut String, o: &FieldOpts<'_>) -> Response {
    let (rect, _) = ui.allocate_exact_size(vec2(o.width, control::H), Sense::hover());
    let slot = ui.painter().add(Shape::Noop);
    let left = if o.icon.is_some() { 32.0 } else { 8.0 };
    let inner = Rect::from_min_max(
        pos2(rect.left() + left, rect.top() + 2.0),
        pos2(rect.right() - 8.0, rect.bottom() - 2.0),
    );
    let edit = egui::TextEdit::singleline(text)
        .frame(false)
        .margin(vec2(0.0, 0.0))
        .vertical_align(Align::Center)
        .desired_width(inner.width())
        .hint_text(RichText::new(o.hint).color(p.faint))
        .interactive(!o.read_only);
    let response = ui.put(inner, edit);
    let focused = o.focused || response.has_focus();
    ui.painter().set(
        slot,
        field_box(ui, p, rect, focused, o.invalid, o.read_only),
    );
    if let Some(icon) = o.icon {
        let tone = if focused { p.accent } else { p.weak };
        glyph(ui, rect.left_center() + vec2(8.0, 0.0), icon, tone);
    }
    response
}

/// Drop-down over `options`; returns true when the choice changed.
pub fn select(
    ui: &mut Ui,
    p: &Palette,
    selected: &mut usize,
    options: &[&str],
    width: f32,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(width, control::H), Sense::click());
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
    ui.painter().add(field_box(ui, p, rect, open, false, false));
    let label = options.get(*selected).copied().unwrap_or("");
    let clip = ui
        .painter()
        .with_clip_rect(rect.with_max_x(rect.right() - 30.0));
    clip.text(
        rect.left_center() + vec2(10.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::new(ty::BODY, egui::FontFamily::Proportional),
        p.text,
    );
    let chevron = if open {
        ph::KEYBOARD_ARROW_UP
    } else {
        ph::KEYBOARD_ARROW_DOWN
    };
    ui.painter().text(
        rect.right_center() - vec2(10.0, 0.0),
        Align2::RIGHT_CENTER,
        chevron,
        FontId::new(control::ICON, egui::FontFamily::Proportional),
        p.weak,
    );
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let mut picked = None;
    egui::Popup::menu(&response).width(width).show(|ui| {
        let entries: Vec<Entry<'_>> = options
            .iter()
            .enumerate()
            .map(|(i, o)| {
                Entry::Item(MenuItem {
                    checked: i == *selected,
                    ..MenuItem::new(o)
                })
            })
            .collect();
        picked = popover_menu(ui, p, &entries);
    });
    match picked {
        Some(i) if i != *selected => {
            *selected = i;
            true
        }
        _ => false,
    }
}

/// Checked state of a box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Empty box.
    Off,
    /// Ticked box.
    On,
    /// Some of a group: a dash.
    Part,
}

/// A box (or round radio) with its label; the response clicks as a whole row.
fn mark_row(ui: &mut Ui, p: &Palette, mark: Mark, round: bool, label: &str) -> Response {
    let font = FontId::new(ty::BODY, egui::FontFamily::Proportional);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, p.text);
    let width = control::BOX + 8.0 + galley.size().x;
    let (rect, response) = ui.allocate_exact_size(vec2(width, control::BOX + 6.0), Sense::click());
    let enabled = ui.is_enabled();
    let on = mark != Mark::Off;
    let boxr = Rect::from_min_size(
        pos2(rect.left(), rect.center().y - control::BOX / 2.0),
        vec2(control::BOX, control::BOX),
    );
    let radius = CornerRadius::same(if round { 8 } else { radius::SM });
    let (fill, line) = if on && enabled {
        (p.accent, p.accent)
    } else if response.hovered() {
        (p.hover, p.outline_strong)
    } else {
        (p.surface, p.outline_strong)
    };
    if round {
        ui.painter().rect_stroke(
            boxr,
            radius,
            Stroke::new(1.5_f32, if on { p.accent } else { line }),
            StrokeKind::Inside,
        );
        if on {
            ui.painter()
                .circle_filled(boxr.center(), control::BOX / 4.0, p.accent);
        }
    } else {
        ui.painter().rect(
            boxr,
            radius,
            fill,
            Stroke::new(1.5_f32, line),
            StrokeKind::Inside,
        );
        let glyph = match mark {
            Mark::On => Some(ph::CHECK),
            Mark::Part => Some(ph::REMOVE),
            Mark::Off => None,
        };
        if let Some(g) = glyph {
            ui.painter().text(
                boxr.center(),
                Align2::CENTER_CENTER,
                g,
                FontId::new(control::BOX - 2.0, egui::FontFamily::Proportional),
                p.on_accent,
            );
        }
    }
    let ink = if enabled { p.text } else { p.disabled };
    ui.painter().galley(
        pos2(boxr.right() + 8.0, rect.center().y - galley.size().y / 2.0),
        galley,
        ink,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Checkbox with a label; flips `value` on click.
pub fn checkbox(ui: &mut Ui, p: &Palette, value: &mut bool, label: &str) -> Response {
    let mark = if *value { Mark::On } else { Mark::Off };
    let response = mark_row(ui, p, mark, false, label);
    if response.clicked() {
        *value = !*value;
    }
    response
}

/// Checkbox showing an explicit mark (including the partial dash); the
/// caller owns the state change.
pub fn checkbox_mark(ui: &mut Ui, p: &Palette, mark: Mark, label: &str) -> Response {
    mark_row(ui, p, mark, false, label)
}

/// Radio button; true marks the picked one of a group.
pub fn radio(ui: &mut Ui, p: &Palette, picked: bool, label: &str) -> Response {
    mark_row(
        ui,
        p,
        if picked { Mark::On } else { Mark::Off },
        true,
        label,
    )
}

/// Switch with a label; flips `value` on click.
pub fn switch(ui: &mut Ui, p: &Palette, value: &mut bool, label: &str) -> Response {
    let font = FontId::new(ty::BODY, egui::FontFamily::Proportional);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, p.text);
    let width = control::SWITCH_W + 8.0 + galley.size().x;
    let (rect, response) = ui.allocate_exact_size(vec2(width, control::H_SM), Sense::click());
    if response.clicked() {
        *value = !*value;
    }
    let enabled = ui.is_enabled();
    let t = ui.ctx().animate_bool(response.id, *value);
    let track = Rect::from_min_size(
        pos2(rect.left(), rect.center().y - control::SWITCH_H / 2.0),
        vec2(control::SWITCH_W, control::SWITCH_H),
    );
    let off = if enabled { p.outline_strong } else { p.border };
    let fill = off.lerp_to_gamma(p.accent, t);
    ui.painter()
        .rect_filled(track, CornerRadius::same(control::SWITCH_H as u8 / 2), fill);
    let knob_r = control::SWITCH_H / 2.0 - 3.0;
    let knob_x = egui::lerp(
        track.left() + knob_r + 3.0..=track.right() - knob_r - 3.0,
        t,
    );
    ui.painter()
        .circle_filled(pos2(knob_x, track.center().y), knob_r, p.on_accent);
    let ink = if enabled { p.text } else { p.disabled };
    ui.painter().galley(
        pos2(track.right() + 8.0, rect.center().y - galley.size().y / 2.0),
        galley,
        ink,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Segmented control: one outlined bar, the picked option filled; returns
/// true when the pick changed.
pub fn segmented(ui: &mut Ui, p: &Palette, picked: &mut usize, options: &[&str]) -> bool {
    let font = FontId::new(ty::BODY, semibold());
    let galleys: Vec<_> = options
        .iter()
        .map(|o| {
            ui.painter()
                .layout_no_wrap((*o).to_owned(), font.clone(), p.text)
        })
        .collect();
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 24.0).collect();
    let total: f32 = widths.iter().sum();
    let (rect, _) = ui.allocate_exact_size(vec2(total, control::H), Sense::hover());
    let shape = CornerRadius::same(radius::SM);
    ui.painter().rect(
        rect,
        shape,
        p.surface,
        Stroke::new(1.0_f32, p.outline),
        StrokeKind::Inside,
    );
    let mut changed = false;
    let mut x = rect.left();
    for (i, galley) in galleys.into_iter().enumerate() {
        let cell = Rect::from_min_size(pos2(x, rect.top()), vec2(widths[i], control::H));
        x += widths[i];
        let response = ui
            .interact(cell, ui.id().with(("seg", i)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let on = *picked == i;
        if on {
            ui.painter()
                .rect_filled(cell.shrink(2.0), shape, p.accent_soft);
        } else if response.hovered() {
            ui.painter().rect_filled(cell.shrink(2.0), shape, p.hover);
        }
        if i > 0 && !on && *picked + 1 != i {
            ui.painter().line_segment(
                [
                    pos2(cell.left(), cell.top() + 6.0),
                    pos2(cell.left(), cell.bottom() - 6.0),
                ],
                Stroke::new(1.0_f32, p.border),
            );
        }
        let ink = if on { p.on_selected } else { p.weak };
        ui.painter()
            .galley(cell.center() - galley.size() / 2.0, galley, ink);
        if response.clicked() && !on {
            *picked = i;
            changed = true;
        }
    }
    changed
}

/// Hover tooltip: a line of text and an optional shortcut hint.
pub fn tip(response: Response, p: &Palette, text: &str, shortcut: Option<&str>) -> Response {
    response.on_hover_ui(|ui| tooltip_body(ui, p, text, shortcut))
}

/// The content of a tooltip; the gallery draws it inline as a preview.
pub fn tooltip_body(ui: &mut Ui, p: &Palette, text: &str, shortcut: Option<&str>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(text).size(ty::CAPTION).color(p.text));
        if let Some(keys) = shortcut {
            super::controls::kbd(ui, p, keys);
        }
    });
}
