//! Data-display components: status and indexability badges, key-value rows,
//! distribution bar, page header with actions, tab strip with counters, KPI
//! pill and avatar chip.

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Layout, Painter,
    Response, RichText, Sense, Stroke, StrokeKind, Ui,
};

use super::icons as ph;
use super::{bold, thousands};
use crate::i18n;
use crate::theme::tokens::Status;
use crate::theme::{control, radius, semibold, space, ty, Palette};

/// A pill of `control::BADGE_H`: optional glyph, then the text.
fn pill(
    ui: &mut Ui,
    bg: Color32,
    fg: Color32,
    icon: Option<&str>,
    text: &str,
    family: FontFamily,
) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::new(ty::CAPTION, family), fg);
    let lead = if icon.is_some() { 18.0 } else { 0.0 };
    let size = vec2(galley.size().x + lead + 12.0, control::BADGE_H);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    // High contrast paints white pills; the outline keeps them visible.
    let outline = if bg == Color32::WHITE { 1.0_f32 } else { 0.0 };
    ui.painter().rect(
        rect,
        CornerRadius::same(control::BADGE_H as u8 / 2),
        bg,
        Stroke::new(outline, fg),
        StrokeKind::Inside,
    );
    if let Some(icon) = icon {
        ui.painter().text(
            pos2(rect.left() + 7.0, rect.center().y),
            Align2::LEFT_CENTER,
            icon,
            FontId::new(ty::BODY + 1.0, FontFamily::Proportional),
            fg,
        );
    }
    ui.painter().galley(
        pos2(
            rect.left() + 6.0 + lead,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        fg,
    );
    response
}

/// HTTP status pill tinted by class (2xx green … 5xx red); `None` is a dash.
pub fn status_badge(ui: &mut Ui, p: &Palette, code: Option<i64>) -> Response {
    let color = super::status_color(p, code);
    let text = code.map_or_else(|| "—".to_owned(), |c| c.to_string());
    pill(
        ui,
        Palette::tint(color, 36),
        color,
        None,
        &text,
        FontFamily::Monospace,
    )
}

/// A tone-coloured pill with a glyph and a label.
pub fn flag_badge(ui: &mut Ui, p: &Palette, status: Status, icon: &str, label: &str) -> Response {
    let (bg, fg) = p.tone(status);
    pill(ui, bg, fg, Some(icon), label, semibold())
}

/// Indexability pill: indexable, not indexable or unknown (a dash).
pub fn indexability_badge(ui: &mut Ui, p: &Palette, indexable: Option<bool>) -> Response {
    match indexable {
        Some(true) => flag_badge(ui, p, Status::Done, ph::CHECK_CIRCLE, i18n::YES),
        Some(false) => flag_badge(ui, p, Status::Unavailable, ph::BLOCK, i18n::NO),
        None => status_badge(ui, p, None),
    }
}

/// Label on the left, value on the right, hairline below.
pub fn key_value(ui: &mut Ui, p: &Palette, key: &str, value: &str, mono: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    ui.painter().text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        key,
        FontId::new(ty::BODY, FontFamily::Proportional),
        p.weak,
    );
    let family = if mono {
        FontFamily::Monospace
    } else {
        FontFamily::Proportional
    };
    ui.painter().text(
        rect.right_center(),
        Align2::RIGHT_CENTER,
        value,
        FontId::new(ty::BODY, family),
        p.text,
    );
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0_f32, p.border_soft),
    );
}

/// One segment of a [`distribution_bar`].
pub struct Slice<'a> {
    /// Legend text.
    pub label: &'a str,
    /// Size of the segment; only ratios matter.
    pub value: f64,
    /// Fill and legend dot.
    pub color: Color32,
}

/// Stacked horizontal bar with a legend of value and share per segment.
pub fn distribution_bar(ui: &mut Ui, p: &Palette, slices: &[Slice<'_>], width: f32) {
    let total: f64 = slices.iter().map(|s| s.value).sum();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 8.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(4), p.surface_hi);
    if total > 0.0 {
        let mut x = rect.left();
        let last = slices.iter().rposition(|s| s.value > 0.0);
        for (i, s) in slices.iter().enumerate() {
            let w = rect.width() * (s.value / total) as f32;
            let seg = egui::Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height()));
            let round = CornerRadius {
                nw: if x == rect.left() { 4 } else { 0 },
                sw: if x == rect.left() { 4 } else { 0 },
                ne: if Some(i) == last { 4 } else { 0 },
                se: if Some(i) == last { 4 } else { 0 },
            };
            ui.painter().rect_filled(seg, round, s.color);
            x += w;
        }
    }
    ui.add_space(space::XS);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        for s in slices {
            let share = if total > 0.0 {
                s.value / total * 100.0
            } else {
                0.0
            };
            ui.label(RichText::new("●").size(ty::OVERLINE).color(s.color));
            ui.label(RichText::new(s.label).size(ty::CAPTION).color(p.weak));
            ui.label(
                RichText::new(format!("{} · {share:.0}%", thousands(s.value as i64)))
                    .size(ty::CAPTION)
                    .color(p.text),
            );
            ui.add_space(space::SM);
        }
    });
}

/// Section or page header: title and subtitle left, `actions` right-aligned.
pub fn page_bar(
    ui: &mut Ui,
    p: &Palette,
    title: &str,
    subtitle: &str,
    actions: impl FnOnce(&mut Ui),
) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(bold(title).size(ty::HEADING));
            if !subtitle.is_empty() {
                ui.label(RichText::new(subtitle).color(p.weak).size(ty::BODY));
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), actions);
    });
}

/// Tab strip with optional counters under each label; returns true when the
/// picked tab changed.
pub fn tab_strip(
    ui: &mut Ui,
    p: &Palette,
    tabs: &[(&str, Option<i64>)],
    picked: &mut usize,
) -> bool {
    let mut changed = false;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        ui.horizontal(|ui| {
            for (i, (label, count)) in tabs.iter().enumerate() {
                let on = *picked == i;
                let ink = if on { p.text } else { p.weak };
                let family = if on {
                    semibold()
                } else {
                    FontFamily::Proportional
                };
                let name = ui.painter().layout_no_wrap(
                    (*label).to_owned(),
                    FontId::new(ty::BODY, family),
                    ink,
                );
                let num = count.map(|n| {
                    ui.painter().layout_no_wrap(
                        thousands(n),
                        FontId::new(ty::CAPTION, FontFamily::Proportional),
                        p.faint,
                    )
                });
                let num_w = num.as_ref().map_or(0.0, |g| g.size().x + 6.0);
                let (rect, response) = ui
                    .allocate_exact_size(vec2(name.size().x + num_w + 24.0, 32.0), Sense::click());
                if response.hovered() && !on {
                    ui.painter().rect_filled(
                        rect.shrink2(vec2(2.0, 4.0)),
                        CornerRadius::same(radius::SM),
                        p.hover,
                    );
                }
                let y = rect.center().y;
                ui.painter().galley(
                    pos2(rect.left() + 12.0, y - name.size().y / 2.0),
                    name.clone(),
                    ink,
                );
                if let Some(num) = num {
                    ui.painter().galley(
                        pos2(
                            rect.left() + 12.0 + name.size().x + 6.0,
                            y - num.size().y / 2.0 + 1.0,
                        ),
                        num,
                        p.faint,
                    );
                }
                if on {
                    let line = egui::Rect::from_min_size(
                        pos2(rect.left() + 8.0, rect.bottom() - 2.0),
                        vec2(rect.width() - 16.0, 2.0),
                    );
                    ui.painter()
                        .rect_filled(line, CornerRadius::same(1), p.accent);
                }
                if response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                    && !on
                {
                    *picked = i;
                    changed = true;
                }
            }
        });
        let (rule, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().rect_filled(rule, 0.0, p.border);
    });
    changed
}

/// KPI strip pill: bold value with a caption; `color` tints the value and
/// adds a dot.
pub fn kpi_pill(
    ui: &mut Ui,
    p: &Palette,
    value: &str,
    caption: &str,
    color: Option<Color32>,
) -> Response {
    let ink = color.unwrap_or(p.text);
    let v = ui
        .painter()
        .layout_no_wrap(value.to_owned(), FontId::new(ty::BODY, semibold()), ink);
    let c = ui.painter().layout_no_wrap(
        caption.to_owned(),
        FontId::new(ty::BODY, FontFamily::Proportional),
        p.weak,
    );
    let dot = if color.is_some() { 14.0 } else { 0.0 };
    let width = v.size().x + c.size().x + dot + 28.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(13),
        p.surface,
        Stroke::new(1.0_f32, p.border),
        StrokeKind::Inside,
    );
    let mut x = rect.left() + 12.0;
    if let Some(color) = color {
        ui.painter()
            .circle_filled(pos2(x + 3.0, rect.center().y), 3.0, color);
        x += dot;
    }
    ui.painter()
        .galley(pos2(x, rect.center().y - v.size().y / 2.0), v.clone(), ink);
    ui.painter().galley(
        pos2(x + v.size().x + 6.0, rect.center().y - c.size().y / 2.0),
        c,
        p.weak,
    );
    response
}

/// Round avatar with up to two initials.
fn paint_avatar(
    painter: &Painter,
    center: egui::Pos2,
    radius: f32,
    initials: &str,
    color: Color32,
) {
    painter.circle_filled(center, radius, color);
    painter.text(
        center,
        Align2::CENTER_CENTER,
        initials,
        FontId::new(radius * 1.0, semibold()),
        Color32::WHITE,
    );
}

/// Avatar alone, `size` points across.
pub fn avatar(ui: &mut Ui, initials: &str, color: Color32, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    paint_avatar(ui.painter(), rect.center(), size / 2.0, initials, color);
    response
}

/// Pill with an avatar, a label and an optional count; `active` marks the
/// picked filter.
pub fn avatar_chip(
    ui: &mut Ui,
    p: &Palette,
    initials: &str,
    label: &str,
    count: Option<&str>,
    color: Color32,
    active: bool,
) -> Response {
    let l =
        ui.painter()
            .layout_no_wrap(label.to_owned(), FontId::new(ty::BODY, semibold()), p.text);
    let n = count.map(|c| {
        ui.painter().layout_no_wrap(
            c.to_owned(),
            FontId::new(ty::BODY, FontFamily::Proportional),
            p.weak,
        )
    });
    let n_w = n.as_ref().map_or(0.0, |g| g.size().x + 6.0);
    let size = vec2(l.size().x + n_w + 46.0, control::H);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let (fill, line) = match (active, response.hovered()) {
        (true, _) => (p.accent_soft, p.accent),
        (false, true) => (p.hover, p.outline),
        (false, false) => (p.surface, p.border),
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(control::H as u8 / 2),
        fill,
        Stroke::new(1.0_f32, line),
        StrokeKind::Inside,
    );
    paint_avatar(
        ui.painter(),
        rect.left_center() + vec2(14.0, 0.0),
        9.0,
        initials,
        color,
    );
    let mut x = rect.left() + 29.0;
    ui.painter().galley(
        pos2(x, rect.center().y - l.size().y / 2.0),
        l.clone(),
        p.text,
    );
    x += l.size().x + 6.0;
    if let Some(n) = n {
        ui.painter()
            .galley(pos2(x, rect.center().y - n.size().y / 2.0), n, p.weak);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
