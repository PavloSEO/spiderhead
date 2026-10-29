//! Design tokens of the window: palette (light and dark), spacing scale, radii,
//! type scale and the bundled fonts (Inter, SIL OFL; Phosphor icons, MIT).
//! The model stores only the theme name; `apply` maps it to egui visuals.

pub mod tokens;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, TextStyle};

/// Spacing scale (points).
pub mod space {
    /// Hairline gap between tightly related items.
    pub const XS: f32 = 4.0;
    /// Default gap inside a control group.
    pub const SM: f32 = 8.0;
    /// Gap between groups inside a card.
    pub const MD: f32 = 12.0;
    /// Card padding and page gutter.
    pub const LG: f32 = 16.0;
    /// Gap between page sections.
    pub const XL: f32 = 24.0;
}

/// Corner radii (points).
pub mod radius {
    /// Fields and table cells (token `field`).
    pub const SM: u8 = 4;
    /// Badges (token `badge`).
    pub const BADGE: u8 = 6;
    /// Buttons, menus, nav items (token `control`).
    pub const MD: u8 = 10;
    /// Cards (token `card`).
    pub const LG: u8 = 14;
    /// Dialogs (token `dialog`).
    pub const DIALOG: u8 = 16;
}

/// Type scale (points).
pub mod ty {
    /// Overlines (uppercase captions), token `overline`.
    pub const OVERLINE: f32 = 11.0;
    /// Meta text, badges, status bar, token `meta`.
    pub const CAPTION: f32 = 12.0;
    /// Body text and table cells, token `body`.
    pub const BODY: f32 = 13.0;
    /// Controls, token `control`.
    pub const CONTROL: f32 = 14.0;
    /// Section titles, token `section`.
    pub const TITLE: f32 = 16.0;
    /// Page titles, token `canvas.page_title`.
    pub const HEADING: f32 = 22.0;
    /// Big numbers on KPI cards (token `kpi`, enlarged for cards).
    pub const KPI: f32 = 26.0;
}

/// Row heights (token `row`).
pub mod row {
    /// Dense table rows.
    pub const COMPACT: f32 = 28.0;
    /// Table header.
    pub const HEADER: f32 = 34.0;
    /// Navigation item.
    pub const NAV_ITEM: f32 = 36.0;
    /// Status bar.
    pub const STATUS_BAR: f32 = 22.0;
    /// Navigation panel width.
    pub const NAV_WIDTH: f32 = 200.0;
}

/// The Roboto Medium face (the tokens' emphasis weight), by name.
pub fn semibold() -> FontFamily {
    FontFamily::Name("medium".into())
}

/// The active theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Light visuals (the canon default, `default_theme` of the tokens).
    #[default]
    Light,
    /// Dark visuals.
    Dark,
    /// High-contrast visuals (black on white, yellow selection).
    HighContrast,
}

/// Background and foreground of a tinted badge.
pub type BadgePair = (Color32, Color32);

/// Every colour the shell paints with, resolved for one theme from the
/// generated tokens.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// The theme these colours belong to.
    pub theme: Theme,
    /// Page background behind the content (`surface`).
    pub bg: Color32,
    /// Top bar and cards (`base`).
    pub surface: Color32,
    /// Navigation panel, tab strip, mini cards (`raised`).
    pub nav: Color32,
    /// Raised fills: headers, inputs, kbd (alias of `nav`).
    pub surface_hi: Color32,
    /// Hovered row / item.
    pub hover: Color32,
    /// Hovered menu / nav item.
    pub menu_hover: Color32,
    /// Alternate table row.
    pub zebra: Color32,
    /// Dividers.
    pub border: Color32,
    /// Soft separators inside menus.
    pub border_soft: Color32,
    /// Control outline.
    pub outline: Color32,
    /// Strong outline (segments, fields).
    pub outline_strong: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text.
    pub weak: Color32,
    /// Tertiary text.
    pub faint: Color32,
    /// Muted icons and chevrons.
    pub muted: Color32,
    /// Disabled text and icons.
    pub disabled: Color32,
    /// Primary action colour.
    pub accent: Color32,
    /// Text on a primary fill.
    pub on_accent: Color32,
    /// Selected fill (`selected`).
    pub accent_soft: Color32,
    /// Text on a selected fill.
    pub on_selected: Color32,
    /// Agent accent (tertiary).
    pub tertiary: Color32,
    /// Agent container fill.
    pub tertiary_container: Color32,
    /// Text on the agent container.
    pub on_tertiary_container: Color32,
    /// Critical severity foreground.
    pub critical: Color32,
    /// Warning severity foreground.
    pub warning: Color32,
    /// Notice severity foreground.
    pub notice: Color32,
    /// Opportunity / success foreground.
    pub opportunity: Color32,
    /// Status code colours: 2xx, 3xx, 4xx, 5xx.
    pub status_codes: [Color32; 4],
}

fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    /// A colour with the given opacity over any background.
    pub fn tint(color: Color32, alpha: u8) -> Color32 {
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
    }

    /// `(background, foreground)` of a status in this theme.
    pub fn tone(&self, status: tokens::Status) -> BadgePair {
        let t = status.tone();
        let (bg, fg) = match self.theme {
            Theme::Light => t.light,
            Theme::Dark => t.dark,
            Theme::HighContrast => t.hc,
        };
        (rgb(bg), rgb(fg))
    }
}

impl Theme {
    /// Every theme, in menu order.
    pub const ALL: [Theme; 3] = [Theme::Light, Theme::Dark, Theme::HighContrast];

    /// Stable id used by commands and `ui.inspect`.
    pub fn name(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
            Theme::HighContrast => "hc",
        }
    }

    /// Parse a `ui.theme` mode value.
    pub fn parse(mode: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == mode)
    }

    /// True for the theme drawn on a dark background.
    pub fn is_dark(self) -> bool {
        self == Theme::Dark
    }

    /// The palette of this theme.
    pub fn palette(self) -> Palette {
        use tokens::Status as S;
        let r = match self {
            Theme::Light => tokens::LIGHT,
            Theme::Dark => tokens::DARK,
            Theme::HighContrast => tokens::HC,
        };
        let mut p = Palette {
            theme: self,
            bg: rgb(r.surface),
            surface: rgb(r.base),
            nav: rgb(r.raised),
            surface_hi: rgb(r.raised),
            hover: rgb(r.hover),
            menu_hover: rgb(r.menu_hover),
            zebra: rgb(r.zebra),
            border: rgb(r.divider),
            border_soft: rgb(r.divider_soft),
            outline: rgb(r.outline),
            outline_strong: rgb(r.outline_strong),
            text: rgb(r.text),
            weak: rgb(r.text_2),
            faint: rgb(r.text_3),
            muted: rgb(r.muted),
            disabled: rgb(r.disabled),
            accent: rgb(r.primary),
            on_accent: rgb(r.on_primary),
            accent_soft: rgb(r.selected),
            on_selected: rgb(r.on_selected),
            tertiary: rgb(r.tertiary),
            tertiary_container: rgb(r.tertiary_container),
            on_tertiary_container: rgb(r.on_tertiary_container),
            critical: Color32::TRANSPARENT,
            warning: Color32::TRANSPARENT,
            notice: Color32::TRANSPARENT,
            opportunity: Color32::TRANSPARENT,
            status_codes: [rgb(r.s2xx), rgb(r.s3xx), rgb(r.s4xx), rgb(r.s5xx)],
        };
        p.critical = p.tone(S::Error).1;
        p.warning = p.tone(S::Warning).1;
        p.notice = p.tone(S::Running).1;
        p.opportunity = p.tone(S::Done).1;
        p
    }
}

/// Register Roboto (regular + medium, Cyrillic included), Roboto Mono and the
/// Phosphor icon font. Called once at start-up; takes effect next frame.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for (name, bytes) in [
        (
            "roboto",
            &include_bytes!("../../assets/fonts/Roboto-Regular.ttf")[..],
        ),
        (
            "roboto-medium",
            &include_bytes!("../../assets/fonts/Roboto-Medium.ttf")[..],
        ),
        (
            "roboto-mono",
            &include_bytes!("../../assets/fonts/RobotoMono-Regular.ttf")[..],
        ),
    ] {
        fonts
            .font_data
            .insert(name.into(), FontData::from_static(bytes).into());
    }
    if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
        list.insert(0, "roboto".into());
    }
    if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
        list.insert(0, "roboto-mono".into());
        list.push("roboto".into());
    }
    fonts
        .families
        .insert(semibold(), vec!["roboto-medium".into(), "roboto".into()]);
    fonts.font_data.insert(
        "material".into(),
        FontData::from_static(include_bytes!(
            "../../assets/fonts/MaterialSymbolsOutlined.ttf"
        ))
        .into(),
    );
    for family in [FontFamily::Proportional, FontFamily::Monospace, semibold()] {
        if let Some(list) = fonts.families.get_mut(&family) {
            list.push("material".into());
        }
    }
    ctx.set_fonts(fonts);
}

/// Push the theme's visuals, spacing and text styles into egui. Call only
/// after `install_fonts` has taken effect (the styles name the medium face).
pub fn apply(ctx: &egui::Context, theme: Theme) {
    let p = theme.palette();
    let mut visuals = if theme.is_dark() {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.panel_fill = p.bg;
    visuals.window_fill = p.surface;
    visuals.extreme_bg_color = p.surface;
    visuals.faint_bg_color = p.zebra;
    visuals.code_bg_color = p.surface_hi;
    visuals.weak_text_color = Some(p.weak);
    visuals.hyperlink_color = p.accent;
    visuals.selection.bg_fill = p.accent_soft;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, p.on_selected);
    visuals.window_stroke = egui::Stroke::new(1.0_f32, p.border);
    visuals.window_corner_radius = radius::DIALOG.into();
    visuals.menu_corner_radius = radius::MD.into();
    visuals.popup_shadow = egui::Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(if theme.is_dark() { 120 } else { 36 }),
    };
    visuals.window_shadow = visuals.popup_shadow;
    let w = &mut visuals.widgets;
    for (state, fill, stroke) in [
        (&mut w.noninteractive, p.surface, p.border),
        (&mut w.inactive, p.surface, p.outline),
        (&mut w.hovered, p.hover, p.outline),
        (&mut w.active, p.accent_soft, p.accent),
        (&mut w.open, p.hover, p.outline),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = egui::Stroke::new(1.0_f32, stroke);
        state.corner_radius = radius::MD.into();
        state.fg_stroke.color = p.text;
    }
    ctx.set_visuals(visuals);

    ctx.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(space::SM, space::SM - 2.0);
        style.spacing.button_padding = egui::vec2(space::MD, 6.0);
        style.spacing.interact_size.y = 32.0;
        style.spacing.menu_margin = egui::Margin::same(6);
        style.spacing.window_margin = egui::Margin::same(space::LG as i8);
        style.interaction.tooltip_delay = 0.35;
        for (text_style, size, family) in [
            (TextStyle::Small, ty::CAPTION, FontFamily::Proportional),
            (TextStyle::Body, ty::BODY, FontFamily::Proportional),
            (TextStyle::Button, ty::CONTROL, semibold()),
            (TextStyle::Heading, ty::HEADING, semibold()),
            (TextStyle::Monospace, ty::BODY - 1.0, FontFamily::Monospace),
        ] {
            style
                .text_styles
                .insert(text_style, FontId::new(size, family));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_ids_roundtrip() {
        for t in Theme::ALL {
            assert_eq!(Theme::parse(t.name()), Some(t));
        }
        assert_eq!(Theme::parse("sepia"), None);
    }

    #[test]
    fn palettes_differ_by_theme() {
        assert_ne!(Theme::Dark.palette().bg, Theme::Light.palette().bg);
        assert_eq!(Theme::default(), Theme::Light);
    }
}
