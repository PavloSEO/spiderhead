//! Top bar (52): fold button, project chip, target and state, ⌘K field and
//! the scan actions. Without a project the bar is the crawler variant of the
//! canon; the tab strip exists only in project mode.

use eframe::egui::{self, vec2, Align2, CornerRadius, FontId, Sense};
use serde_json::json;

use crate::app::Mode;
use crate::i18n;
use crate::theme::tokens::Status;
use crate::theme::{radius, ty, Palette};
use crate::ui::kit::icons as ph;
use crate::ui::kit::{self, BtnKind};
use crate::ui::DesktopApp;

/// Draw the bar above everything else.
pub fn top_bar(ctx: &egui::Context, app: &mut DesktopApp) {
    let p = app.model.theme.palette();
    egui::TopBottomPanel::top("topbar")
        .exact_height(36.0)
        .frame(
            egui::Frame::new()
                .fill(p.surface)
                .stroke(egui::Stroke::new(1.0_f32, p.border))
                // Under the transparent macOS titlebar the traffic lights
                // occupy the left ~78 px; the toolbar starts right of them.
                .inner_margin(egui::Margin {
                    left: if cfg!(target_os = "macos") { 84 } else { 12 },
                    right: 12,
                    top: 0,
                    bottom: 0,
                }),
        )
        .show(ctx, |ui| {
            // Right group first (Sides lays it out before the left one and
            // shrinks the left): the address field takes what is left, so
            // the ⌘K field never lands on top of it in a narrow window.
            let narrow = ui.available_width() < 1100.0;
            let url = start_url(app);
            let view: &DesktopApp = app;
            let (left, right) = egui::Sides::new().shrink_left().spacing(8.0).show(
                ui,
                |ui| left_group(ui, view, &p, url.as_deref()),
                |ui| right_group(ui, view, &p, narrow),
            );
            if let Some((method, params)) = left.or(right) {
                app.act(method, params);
            }
        });
}

/// A click in the bar: the command and its params, run after layout.
type Click = Option<(&'static str, serde_json::Value)>;

fn left_group(ui: &mut egui::Ui, app: &DesktopApp, p: &Palette, url: Option<&str>) -> Click {
    let mut click = None;
    ui.spacing_mut().item_spacing.x = 8.0;
    let tip = if app.model.nav_collapsed {
        i18n::EXPAND_NAV
    } else {
        i18n::COLLAPSE_NAV
    };
    if kit::icon_button(ui, p, ph::SIDE_NAVIGATION, tip).clicked() {
        click = Some(("ui.sidebar", json!({})));
    }
    // The left chip names the mode; the crawler keeps its
    // «без проекта» wording from the canon.
    let chip = match app.model.mode {
        Some(Mode::Logs) => i18n::MODE_LOGS,
        Some(Mode::Agent) => i18n::MODE_AGENT,
        _ => i18n::SHELL_NO_PROJECT,
    };
    kit::status_chip(ui, p, Status::NotAgreed, chip).on_hover_text(i18n::PROJECTS_TIP);
    match app.model.scan.is_some() {
        true => {
            let n = total(app);
            kit::status_chip(ui, p, Status::Done, &i18n::scan_ready(n));
        }
        false => {
            kit::status_chip(ui, p, Status::NotAgreed, i18n::SCAN_NONE);
        }
    }
    if let Some(url) = url {
        target_field(ui, p, url);
    }
    click
}

fn right_group(ui: &mut egui::Ui, app: &DesktopApp, p: &Palette, narrow: bool) -> Click {
    let mut click = None;
    ui.spacing_mut().item_spacing.x = 8.0;
    // Crawl actions live in the crawler mode (canon top bar).
    if app.model.mode == Some(Mode::Crawler) {
        let save = if narrow { "" } else { i18n::SAVE_AS_PROJECT };
        if kit::button(ui, p, Some(ph::CREATE_NEW_FOLDER), save, BtnKind::Primary)
            .on_hover_text(i18n::SAVE_AS_PROJECT)
            .clicked()
        {
            click = Some(("ui.mode", json!({"mode": "agent"})));
        }
        let crawl = if narrow { "" } else { i18n::NEW_CRAWL };
        if kit::button(ui, p, Some(ph::REFRESH), crawl, BtnKind::Outlined)
            .on_hover_text(i18n::NEW_CRAWL)
            .clicked()
        {
            click = Some(("ui.navigate", json!({"id": "crawler"})));
        }
    }
    if palette_field(ui, p, narrow).clicked() {
        click = Some(("ui.window", json!({"id": "palette"})));
    }
    click
}

fn total(app: &DesktopApp) -> i64 {
    app.model
        .table
        .last
        .as_ref()
        .and_then(|l| l.get("total"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

/// The crawl target of the open scan, read once per scan.
fn start_url(app: &mut DesktopApp) -> Option<String> {
    let scan = app.model.scan.as_ref()?;
    let key = scan.path.display().to_string();
    if let Some((k, v)) = &app.view.start_url {
        if *k == key {
            return v.clone();
        }
    }
    let url: Option<String> = scan
        .con
        .query_row("SELECT start_url FROM scan WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .ok();
    app.view.start_url = Some((key, url.clone()));
    url
}

/// Read-only target field: globe icon + monospace address.
fn target_field(ui: &mut egui::Ui, p: &Palette, url: &str) {
    // The address is the one stretching element (min 160): it shows the
    // whole URL whenever the bar has room for it.
    let font = FontId::new(ty::CAPTION, egui::FontFamily::Monospace);
    let galley = ui.painter().layout_no_wrap(url.to_owned(), font, p.text);
    let text_w = galley.size().x;
    let width = (text_w + 48.0).min(ui.available_width()).max(160.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
    if text_w + 48.0 > width {
        response.on_hover_text(url);
    }
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(radius::MD),
        egui::Stroke::new(1.0_f32, p.outline),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.left_center() + vec2(10.0, 0.0),
        Align2::LEFT_CENTER,
        ph::LANGUAGE,
        FontId::new(18.0, egui::FontFamily::Proportional),
        p.weak,
    );
    let clip = ui.painter().with_clip_rect(
        rect.with_min_x(rect.left() + 34.0)
            .with_max_x(rect.right() - 8.0),
    );
    clip.galley(
        egui::pos2(rect.left() + 34.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.text,
    );
}

/// The «Действия и переходы ⌘K» field: 220 wide, a bare search icon when
/// the window is narrow.
fn palette_field(ui: &mut egui::Ui, p: &Palette, narrow: bool) -> egui::Response {
    let w = if narrow { 28.0 } else { 220.0 };
    let (rect, response) = ui.allocate_exact_size(vec2(w, 28.0), Sense::click());
    let fill = if response.hovered() {
        p.hover
    } else {
        p.surface
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(radius::MD),
        fill,
        egui::Stroke::new(1.0_f32, p.outline),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.left_center() + vec2(if narrow { 5.0 } else { 10.0 }, 0.0),
        Align2::LEFT_CENTER,
        ph::SEARCH,
        FontId::new(18.0, egui::FontFamily::Proportional),
        p.weak,
    );
    if narrow {
        return response
            .on_hover_text(i18n::PALETTE_FIELD)
            .on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    ui.painter().text(
        rect.left_center() + vec2(36.0, 0.0),
        Align2::LEFT_CENTER,
        i18n::PALETTE_FIELD,
        FontId::new(ty::BODY, egui::FontFamily::Proportional),
        p.weak,
    );
    let galley = ui.painter().layout_no_wrap(
        "⌘K".to_owned(),
        FontId::new(11.0, egui::FontFamily::Monospace),
        p.weak,
    );
    let cap = egui::Rect::from_center_size(
        egui::pos2(
            rect.right() - 10.0 - galley.size().x / 2.0 - 6.0,
            rect.center().y,
        ),
        galley.size() + vec2(12.0, 6.0),
    );
    ui.painter().rect_filled(cap, CornerRadius::same(5), p.nav);
    ui.painter()
        .galley(cap.center() - galley.size() / 2.0, galley, p.weak);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
