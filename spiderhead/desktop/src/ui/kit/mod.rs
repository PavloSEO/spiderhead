//! Reusable painted widgets: severity system, badges, KPI cards, nav items,
//! tabs, chips and empty states. Everything reads colours from the palette so
//! both themes stay consistent.

use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, Response, Sense, Stroke, Ui};
pub mod controls;
pub mod icons;
pub use controls::{button, icon_button, kbd, status_chip, BtnKind};
use icons as ph;

use crate::i18n::strs;
use crate::theme::{radius, semibold, space, ty, Palette};

/// The four severities of a finding, most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Breaks the page for users or crawlers.
    Critical,
    /// Worth fixing.
    Warning,
    /// Worth a look.
    Notice,
    /// Optimisation.
    Opportunity,
}

impl Severity {
    /// All severities in display order.
    pub const ALL: [Severity; 4] = [
        Severity::Critical,
        Severity::Warning,
        Severity::Notice,
        Severity::Opportunity,
    ];

    /// Parse a registry severity id.
    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }

    /// Registry id (`critical`, …).
    pub fn id(self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::Warning => "warning",
            Severity::Notice => "notice",
            Severity::Opportunity => "opportunity",
        }
    }

    /// Russian plural label for cards and column values.
    pub fn label(self) -> &'static str {
        let s = strs();
        match self {
            Severity::Critical => s.sev_critical,
            Severity::Warning => s.sev_warning,
            Severity::Notice => s.sev_notice,
            Severity::Opportunity => s.sev_opportunity,
        }
    }

    /// Phosphor glyph.
    pub fn icon(self) -> &'static str {
        match self {
            Severity::Critical => ph::ERROR,
            Severity::Warning => ph::WARNING,
            Severity::Notice => ph::INFO,
            Severity::Opportunity => ph::LIGHTBULB,
        }
    }

    /// Palette colour.
    pub fn color(self, p: &Palette) -> Color32 {
        match self {
            Severity::Critical => p.critical,
            Severity::Warning => p.warning,
            Severity::Notice => p.notice,
            Severity::Opportunity => p.opportunity,
        }
    }

    /// Badge background and foreground from the theme tokens.
    pub fn badge(self, p: &Palette) -> (Color32, Color32) {
        use crate::theme::tokens::Status;
        p.tone(match self {
            Severity::Critical => Status::Error,
            Severity::Warning => Status::Warning,
            Severity::Notice => Status::Running,
            Severity::Opportunity => Status::Done,
        })
    }
}

/// Colour of an HTTP status code: 2xx green, 3xx amber, 4xx/5xx red, none grey.
pub fn status_color(p: &Palette, code: Option<i64>) -> Color32 {
    match code {
        Some(200..=299) => p.status_codes[0],
        Some(300..=399) => p.status_codes[1],
        Some(400..=499) => p.status_codes[2],
        Some(500..=599) => p.status_codes[3],
        _ => p.faint,
    }
}

/// Semibold rich text.
pub fn bold(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).family(semibold())
}

/// A tinted pill: `text` on a soft fill of `color`.
pub fn badge(ui: &mut Ui, color: Color32, text: &str) -> Response {
    let font = FontId::new(ty::CAPTION, semibold());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let size = galley.size() + vec2(14.0, 6.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(radius::BADGE),
        Palette::tint(color, 36),
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    response
}

/// Severity pill: icon + label on the token badge colours.
pub fn severity_badge(ui: &mut Ui, p: &Palette, sev: Severity) -> Response {
    let (bg, fg) = sev.badge(p);
    badge_on(ui, bg, fg, &format!("{}  {}", sev.icon(), sev.label()))
}

/// A pill with explicit background and foreground; a border is drawn when
/// the background is white (high contrast).
pub fn badge_on(ui: &mut Ui, bg: Color32, fg: Color32, text: &str) -> Response {
    let font = FontId::new(ty::CAPTION, semibold());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, fg);
    let size = galley.size() + vec2(14.0, 6.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(radius::BADGE),
        bg,
        Stroke::new(if bg == Color32::WHITE { 1.0_f32 } else { 0.0 }, fg),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    response
}

/// A bordered card with the standard padding.
pub fn card<R>(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(radius::LG))
        .inner_margin(egui::Margin::same(space::LG as i8))
        .show(ui, add)
        .inner
}

/// Left-navigation entry: icon + label, optional count badge on the right.
pub fn nav_item(
    ui: &mut Ui,
    p: &Palette,
    icon: &str,
    label: &str,
    selected: bool,
    badge: Option<(String, Color32)>,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), crate::theme::row::NAV_ITEM),
        Sense::click(),
    );
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::same(radius::MD), p.accent_soft);
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(radius::MD), p.hover);
    }
    let ink = if selected { p.on_selected } else { p.text };
    let icon_color = if selected { p.on_selected } else { p.weak };
    painter.text(
        rect.left_center() + vec2(14.0, 0.0),
        Align2::LEFT_CENTER,
        icon,
        FontId::new(17.0, egui::FontFamily::Proportional),
        icon_color,
    );
    let font = if selected {
        FontId::new(ty::BODY, semibold())
    } else {
        FontId::new(ty::BODY, egui::FontFamily::Proportional)
    };
    painter.text(
        rect.left_center() + vec2(40.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        font,
        ink,
    );
    if let Some((text, color)) = badge {
        let galley =
            painter.layout_no_wrap(text, FontId::new(ty::CAPTION, semibold()), Color32::WHITE);
        let size = vec2(galley.size().x.max(10.0) + 10.0, 17.0);
        let pill = Rect::from_min_size(
            pos2(rect.right() - size.x - 8.0, rect.center().y - size.y / 2.0),
            size,
        );
        painter.rect_filled(pill, CornerRadius::same(9), color);
        painter.galley(pill.center() - galley.size() / 2.0, galley, Color32::WHITE);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Clickable KPI card: tinted icon, caption, big number.
#[allow(clippy::too_many_arguments)] // one painted widget; a params struct would only restate them
pub fn kpi_card(
    ui: &mut Ui,
    p: &Palette,
    width: f32,
    icon: &str,
    caption: &str,
    value: &str,
    color: Color32,
    active: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 84.0), Sense::click());
    let painter = ui.painter();
    let fill = if response.hovered() {
        p.hover
    } else {
        p.surface
    };
    let stroke = if active {
        Stroke::new(1.5_f32, color)
    } else {
        Stroke::new(1.0_f32, p.border)
    };
    painter.rect(
        rect,
        CornerRadius::same(radius::LG),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    if active {
        painter.rect_filled(
            rect.shrink(1.5),
            CornerRadius::same(radius::LG - 1),
            Palette::tint(color, 18),
        );
    }
    // Colour strip on the left edge ties the card to its severity.
    let strip = Rect::from_min_size(
        rect.left_top() + vec2(0.0, 16.0),
        vec2(3.0, rect.height() - 32.0),
    );
    painter.rect_filled(strip, CornerRadius::same(2), color);
    let chip = Rect::from_center_size(rect.left_top() + vec2(34.0, 30.0), vec2(30.0, 30.0));
    painter.rect_filled(
        chip,
        CornerRadius::same(radius::MD),
        Palette::tint(color, 40),
    );
    painter.text(
        chip.center(),
        Align2::CENTER_CENTER,
        icon,
        FontId::new(18.0, egui::FontFamily::Proportional),
        color,
    );
    painter.text(
        rect.left_top() + vec2(58.0, 30.0),
        Align2::LEFT_CENTER,
        caption,
        FontId::new(ty::BODY, egui::FontFamily::Proportional),
        p.weak,
    );
    painter.text(
        rect.left_bottom() + vec2(20.0, -12.0),
        Align2::LEFT_BOTTOM,
        value,
        FontId::new(ty::KPI, semibold()),
        p.text,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Underlined tab button.
pub fn tab(ui: &mut Ui, p: &Palette, label: &str, selected: bool) -> Response {
    let font = FontId::new(
        ty::BODY,
        if selected {
            semibold()
        } else {
            egui::FontFamily::Proportional
        },
    );
    let ink = if selected { p.text } else { p.weak };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, ink);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 20.0, 30.0), Sense::click());
    if response.hovered() && !selected {
        ui.painter().rect_filled(
            rect.shrink2(vec2(2.0, 3.0)),
            CornerRadius::same(radius::SM),
            p.surface_hi,
        );
    }
    ui.painter().galley(
        rect.center() - galley.size() / 2.0 - vec2(0.0, 1.0),
        galley,
        ink,
    );
    if selected {
        let line = Rect::from_min_size(
            pos2(rect.left() + 8.0, rect.bottom() - 2.0),
            vec2(rect.width() - 16.0, 2.0),
        );
        ui.painter()
            .rect_filled(line, CornerRadius::same(1), p.accent);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Removable filter chip; returns true when the × was clicked.
pub fn chip(ui: &mut Ui, p: &Palette, icon: &str, text: &str, color: Color32) -> bool {
    let font = FontId::new(ty::CAPTION + 0.5, egui::FontFamily::Proportional);
    let label = format!("{icon}  {text}   {}", ph::CLOSE);
    let galley = ui.painter().layout_no_wrap(label, font, color);
    let (rect, response) = ui.allocate_exact_size(galley.size() + vec2(18.0, 8.0), Sense::click());
    let fill = Palette::tint(color, if response.hovered() { 56 } else { 34 });
    ui.painter().rect(
        rect,
        CornerRadius::same(13),
        fill,
        Stroke::new(1.0_f32, Palette::tint(color, 90)),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    let _ = p;
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// Centered empty state: big icon, title, hint. Returns the action button's
/// response when `action` is given.
pub fn empty_state(
    ui: &mut Ui,
    p: &Palette,
    icon: &str,
    title: &str,
    hint: &str,
    action: Option<&str>,
) -> Option<Response> {
    let mut out = None;
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.28).max(space::XL));
        let (rect, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 32.0, Palette::tint(p.accent, 30));
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            icon,
            FontId::new(30.0, egui::FontFamily::Proportional),
            p.accent,
        );
        ui.add_space(space::MD);
        ui.label(bold(title).size(ty::TITLE));
        ui.add_space(space::XS);
        ui.add(egui::Label::new(egui::RichText::new(hint).color(p.weak)).wrap());
        if let Some(label) = action {
            ui.add_space(space::LG);
            let button = egui::Button::new(bold(label).color(p.on_accent))
                .fill(p.accent)
                .min_size(vec2(160.0, 32.0));
            out = Some(
                ui.add(button)
                    .on_hover_cursor(egui::CursorIcon::PointingHand),
            );
        }
    });
    out
}

/// Page header: title with an optional weak subtitle underneath.
pub fn page_header(ui: &mut Ui, p: &Palette, title: &str, subtitle: &str) {
    ui.label(bold(title).size(ty::HEADING));
    if !subtitle.is_empty() {
        ui.label(egui::RichText::new(subtitle).color(p.weak).size(ty::BODY));
    }
}

/// Uppercase caption above a group.
pub fn caption(ui: &mut Ui, p: &Palette, text: &str) {
    ui.label(bold(text.to_uppercase()).size(ty::OVERLINE).color(p.faint));
}

/// Human byte size: `812 Б`, `12,4 КБ`, `3,1 МБ`.
pub fn human_bytes(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    if b < 1024.0 {
        format!("{bytes} Б")
    } else if b < 1024.0 * 1024.0 {
        format!("{} КБ", format!("{:.1}", b / 1024.0).replace('.', ","))
    } else {
        format!("{} МБ", format!("{:.1}", b / 1_048_576.0).replace('.', ","))
    }
}

/// Integer with thin-space thousands: `12 345`.
pub fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{2009}');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_formats() {
        assert_eq!(thousands(1234567), "1\u{2009}234\u{2009}567");
        assert_eq!(thousands(12), "12");
        assert_eq!(human_bytes(512), "512 Б");
        assert_eq!(human_bytes(2048), "2,0 КБ");
    }

    #[test]
    fn severity_ids_roundtrip() {
        for s in Severity::ALL {
            assert_eq!(Severity::parse(s.id()), Some(s));
        }
    }
}
