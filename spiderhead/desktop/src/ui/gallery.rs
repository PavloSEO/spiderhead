//! Component gallery: every kit component in its states on the active theme.
//! Hidden from navigation; `ui.gallery` opens it and `ui.screenshot` captures it.

use egui::{vec2, Align, Frame, Id, RichText, Ui};
use serde_json::json;

use crate::i18n as t;
use crate::theme::tokens::Status;
use crate::theme::{space, ty, Palette};
use crate::ui::kit::chart::{
    bar_chart, line_chart, percent_bar_chart, sparkline, stacked_bar_chart, Series,
};
use crate::ui::kit::data::{
    avatar, avatar_chip, distribution_bar, flag_badge, indexability_badge, key_value, kpi_pill,
    page_bar, status_badge, tab_strip, Slice,
};
use crate::ui::kit::inputs::{
    checkbox, checkbox_mark, radio, segmented, select, switch, text_field, tip, tooltip_body,
    FieldOpts, Mark,
};
use crate::ui::kit::overlay::{
    banner, modal_frame, popover_menu, progress_bar, skeleton, toast, Entry, MenuItem, Tone,
};
use crate::ui::kit::{self, icons as ph, BtnKind};
use crate::ui::DesktopApp;

/// Section ids, in page order; `ui.gallery {"section": id}` scrolls to one.
pub const SECTIONS: [&str; 8] = [
    "buttons", "fields", "choice", "badges", "overlays", "feedback", "data", "charts",
];

/// Values the demo controls edit.
#[derive(Clone, Default)]
struct Demo {
    text: String,
    pick: usize,
    checked: bool,
    switch_on: bool,
    switch_off: bool,
    radio: usize,
    seg: usize,
    tab: usize,
}

/// Draw the gallery into the central panel; `section` scrolls to a block.
pub fn show(ui: &mut Ui, app: &mut DesktopApp, section: Option<&str>) {
    let p = app.model.theme.palette();
    let id = Id::new("gallery-demo");
    let mut demo: Demo = ui.data(|d| d.get_temp(id)).unwrap_or_else(|| Demo {
        switch_on: true,
        ..Demo::default()
    });
    let mut close = false;
    page_bar(ui, &p, t::GAL_TITLE, t::GAL_SUB, |ui| {
        close = kit::button(ui, &p, Some(ph::CLOSE), t::GAL_CLOSE, BtnKind::Outlined).clicked();
    });
    ui.add_space(space::SM);
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = space::MD;
        let block = |ui: &mut Ui, key: &str, title: &str, add: &mut dyn FnMut(&mut Ui)| {
            if section == Some(key) {
                ui.scroll_to_cursor_animation(
                    Some(Align::TOP),
                    egui::style::ScrollAnimation::none(),
                );
            }
            kit::card(ui, &p, |ui| {
                ui.set_width(ui.available_width());
                kit::caption(ui, &p, title);
                ui.add_space(space::XS);
                add(ui);
            });
        };
        block(ui, "buttons", t::GAL_BUTTONS, &mut |ui| buttons(ui, &p));
        block(ui, "fields", t::GAL_FIELDS, &mut |ui| {
            fields(ui, &p, &mut demo)
        });
        block(ui, "choice", t::GAL_CHOICE, &mut |ui| {
            choice(ui, &p, &mut demo)
        });
        block(ui, "badges", t::GAL_BADGES, &mut |ui| badges(ui, &p));
        block(ui, "overlays", t::GAL_OVERLAYS, &mut |ui| overlays(ui, &p));
        block(ui, "feedback", t::GAL_FEEDBACK, &mut |ui| feedback(ui, &p));
        block(ui, "data", t::GAL_DATA, &mut |ui| data(ui, &p, &mut demo));
        block(ui, "charts", t::GAL_CHARTS, &mut |ui| charts(ui, &p));
    });
    ui.data_mut(|d| d.insert_temp(id, demo));
    if close {
        app.act("ui.gallery", json!({"open": false}));
    }
}

/// A labelled row: a fixed caption column, then the samples.
fn row(ui: &mut Ui, p: &Palette, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::MD;
        ui.allocate_ui_with_layout(
            vec2(112.0, 28.0),
            egui::Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_size(vec2(112.0, 28.0));
                ui.label(RichText::new(label).size(ty::CAPTION).color(p.faint))
            },
        );
        add(ui);
    });
}

fn buttons(ui: &mut Ui, p: &Palette) {
    row(ui, p, t::GAL_KINDS, |ui| {
        kit::button(ui, p, Some(ph::PLAY_ARROW), t::GAL_RUN, BtnKind::Primary);
        kit::button(ui, p, None, t::GAL_CANCEL, BtnKind::Outlined);
        kit::button(ui, p, Some(ph::DOWNLOAD), t::GAL_EXPORT, BtnKind::Outlined);
        kit::button(ui, p, None, t::GAL_MORE, BtnKind::Text);
    });
    row(ui, p, t::GAL_DISABLED, |ui| {
        ui.add_enabled_ui(false, |ui| {
            kit::button(ui, p, Some(ph::PLAY_ARROW), t::GAL_RUN, BtnKind::Primary);
            kit::button(ui, p, None, t::GAL_CANCEL, BtnKind::Outlined);
        });
    });
    row(ui, p, t::GAL_ICONS, |ui| {
        kit::icon_button(ui, p, ph::REFRESH, t::GAL_REFRESH);
        kit::icon_button(ui, p, ph::TUNE, t::GAL_SETTINGS);
        kit::icon_button(ui, p, ph::DOWNLOAD, t::GAL_EXPORT);
        kit::kbd(ui, p, "⌘K");
    });
}

fn fields(ui: &mut Ui, p: &Palette, d: &mut Demo) {
    let mut empty = String::new();
    let mut filled = t::GAL_SAMPLE_URL.to_owned();
    let mut bad = "12".to_owned();
    let mut locked = t::GAL_SAMPLE_PATH.to_owned();
    row(ui, p, t::GAL_STATE_DEFAULT, |ui| {
        let o = FieldOpts {
            icon: Some(ph::LINK),
            hint: t::GAL_HINT_URL,
            ..FieldOpts::default()
        };
        text_field(ui, p, &mut empty, &o);
        text_field(ui, p, &mut filled, &FieldOpts::default());
    });
    row(ui, p, t::GAL_STATE_FOCUS, |ui| {
        let o = FieldOpts {
            icon: Some(ph::SEARCH),
            hint: t::GAL_HINT_SEARCH,
            focused: true,
            ..FieldOpts::default()
        };
        text_field(ui, p, &mut d.text, &o);
    });
    row(ui, p, t::GAL_STATE_ERROR, |ui| {
        let o = FieldOpts {
            invalid: true,
            ..FieldOpts::default()
        };
        text_field(ui, p, &mut bad, &o);
        ui.label(
            RichText::new(t::GAL_ERR_LIMIT)
                .size(ty::CAPTION)
                .color(p.critical),
        );
    });
    row(ui, p, t::GAL_STATE_LOCKED, |ui| {
        let o = FieldOpts {
            read_only: true,
            width: 260.0,
            ..FieldOpts::default()
        };
        text_field(ui, p, &mut locked, &o);
        ui.add_enabled_ui(false, |ui| {
            let mut off = String::new();
            let o = FieldOpts {
                hint: t::GAL_STATE_OFF,
                ..FieldOpts::default()
            };
            text_field(ui, p, &mut off, &o);
        });
    });
    row(ui, p, t::GAL_SELECT, |ui| {
        select(ui, p, &mut d.pick, &t::GAL_OPTIONS, 220.0);
        Frame::menu(ui.style()).show(ui, |ui| {
            ui.set_width(240.0);
            let items: Vec<Entry<'_>> = t::GAL_OPTIONS
                .iter()
                .enumerate()
                .map(|(i, o)| {
                    Entry::Item(MenuItem {
                        checked: i == d.pick,
                        hint: (i == 0).then_some(t::GAL_DEFAULT_HINT),
                        ..MenuItem::new(o)
                    })
                })
                .collect();
            popover_menu(ui, p, &items);
        });
    });
}

fn choice(ui: &mut Ui, p: &Palette, d: &mut Demo) {
    row(ui, p, t::GAL_CHECKBOX, |ui| {
        checkbox(ui, p, &mut d.checked, t::GAL_YES);
        checkbox_mark(ui, p, Mark::Off, t::GAL_NO);
        checkbox_mark(ui, p, Mark::Part, t::GAL_PART);
        ui.add_enabled_ui(false, |ui| {
            checkbox_mark(ui, p, Mark::Off, t::GAL_UNAVAILABLE);
            checkbox_mark(ui, p, Mark::On, t::GAL_UNAVAILABLE);
        });
    });
    row(ui, p, t::GAL_RADIO, |ui| {
        for (i, label) in t::GAL_RADIOS.iter().enumerate() {
            if radio(ui, p, d.radio == i, label).clicked() {
                d.radio = i;
            }
        }
    });
    row(ui, p, t::GAL_SWITCH, |ui| {
        switch(ui, p, &mut d.switch_on, t::GAL_SAVE_HTML);
        switch(ui, p, &mut d.switch_off, t::GAL_LIMIT_URL);
        ui.add_enabled_ui(false, |ui| {
            let mut off = false;
            switch(ui, p, &mut off, t::GAL_UNAVAILABLE);
        });
    });
    row(ui, p, t::GAL_SEGMENTED, |ui| {
        segmented(ui, p, &mut d.seg, &t::GAL_SEGMENTS);
    });
}

fn badges(ui: &mut Ui, p: &Palette) {
    row(ui, p, t::GAL_HTTP, |ui| {
        for code in [Some(200), Some(301), Some(404), Some(503), None] {
            status_badge(ui, p, code);
        }
    });
    row(ui, p, t::GAL_INDEX, |ui| {
        for state in [Some(true), Some(false), None] {
            indexability_badge(ui, p, state);
        }
        flag_badge(ui, p, Status::Error, ph::BLOCK, t::GAL_BLOCKED);
        flag_badge(ui, p, Status::Warning, ph::WARNING, t::GAL_NOTICE);
    });
    row(ui, p, t::GAL_STATUS, |ui| {
        for status in [
            Status::Running,
            Status::Done,
            Status::Warning,
            Status::Error,
        ] {
            kit::status_chip(ui, p, status, status.tone().label);
        }
    });
    row(ui, p, t::GAL_SEVERITY, |ui| {
        for sev in kit::Severity::ALL {
            kit::severity_badge(ui, p, sev);
        }
    });
}

fn overlays(ui: &mut Ui, p: &Palette) {
    row(ui, p, t::GAL_MENU, |ui| {
        Frame::menu(ui.style()).show(ui, |ui| {
            ui.set_width(200.0);
            let items = [
                Entry::Item(MenuItem {
                    icon: Some(ph::CONTENT_COPY),
                    hint: Some("⌘C"),
                    ..MenuItem::new(t::GAL_COPY)
                }),
                Entry::Item(MenuItem {
                    icon: Some(ph::OPEN_IN_NEW),
                    ..MenuItem::new(t::GAL_OPEN)
                }),
                Entry::Sep,
                Entry::Item(MenuItem {
                    icon: Some(ph::DELETE),
                    danger: true,
                    ..MenuItem::new(t::GAL_DELETE)
                }),
            ];
            popover_menu(ui, p, &items);
        });
        let mut name = t::GAL_SAMPLE_PATH.to_owned();
        modal_frame(
            ui,
            p,
            t::GAL_MODAL_TITLE,
            400.0,
            |ui| {
                ui.label(RichText::new(t::GAL_MODAL_BODY).color(p.weak));
                let o = FieldOpts {
                    width: 368.0,
                    ..FieldOpts::default()
                };
                text_field(ui, p, &mut name, &o);
            },
            |ui| {
                kit::button(ui, p, None, t::GAL_SAVE, BtnKind::Primary);
                kit::button(ui, p, None, t::GAL_CANCEL, BtnKind::Outlined);
            },
        );
    });
    row(ui, p, t::GAL_TOAST, |ui| {
        ui.vertical(|ui| {
            toast(ui, p, Tone::Success, t::GAL_TOAST_OK, t::GAL_TOAST_OK_D);
            toast(ui, p, Tone::Error, t::GAL_TOAST_ERR, t::GAL_TOAST_ERR_D);
        });
    });
    row(ui, p, t::GAL_BANNER, |ui| {
        ui.vertical(|ui| {
            banner(ui, p, Tone::Info, t::GAL_BANNER_INFO, &[t::GAL_UPDATE]);
            banner(ui, p, Tone::Warning, t::GAL_BANNER_WARN, &[t::GAL_DETAILS]);
            banner(
                ui,
                p,
                Tone::Error,
                t::GAL_BANNER_ERR,
                &[t::GAL_RETRY, t::GAL_DETAILS],
            );
        });
    });
}

fn feedback(ui: &mut Ui, p: &Palette) {
    row(ui, p, t::GAL_PROGRESS, |ui| {
        progress_bar(ui, p, Some(0.64), 260.0);
        progress_bar(ui, p, None, 260.0);
    });
    row(ui, p, t::GAL_SKELETON, |ui| {
        ui.vertical(|ui| {
            skeleton(ui, p, 360.0, 12.0);
            skeleton(ui, p, 260.0, 12.0);
        });
    });
    row(ui, p, t::GAL_TOOLTIP, |ui| {
        Frame::popup(ui.style()).show(ui, |ui| tooltip_body(ui, p, t::GAL_COLUMNS_TIP, Some("⌘J")));
        let r = kit::icon_button(ui, p, ph::VIEW_COLUMN, "");
        tip(r, p, t::GAL_HOVER_ME, None);
    });
}

fn data(ui: &mut Ui, p: &Palette, d: &mut Demo) {
    row(ui, p, t::GAL_TABS, |ui| {
        ui.vertical(|ui| {
            ui.set_width(ui.available_width().min(520.0));
            tab_strip(ui, p, &t::GAL_TAB_LIST, &mut d.tab);
        });
    });
    row(ui, p, t::GAL_KPI, |ui| {
        kpi_pill(ui, p, "612 480", t::GAL_VISITS, None);
        kpi_pill(ui, p, "81,1 %", "2xx", Some(p.status_codes[0]));
        kpi_pill(ui, p, "9,3 %", "3xx", Some(p.status_codes[1]));
        kpi_pill(ui, p, "1,6 %", "5xx", Some(p.status_codes[3]));
    });
    row(ui, p, t::GAL_AVATAR, |ui| {
        avatar(ui, "ПБ", p.accent, 28.0);
        avatar_chip(ui, p, "G", "Googlebot", Some("251 117"), p.series[0], true);
        avatar_chip(ui, p, "Я", "YandexBot", Some("202 118"), p.series[2], false);
        avatar_chip(ui, p, "b", "Bingbot", Some("42 874"), p.series[3], false);
    });
    row(ui, p, t::GAL_KEY_VALUE, |ui| {
        ui.vertical(|ui| {
            ui.set_width(300.0);
            key_value(ui, p, t::GAL_KV_URLS, "1 314", false);
            key_value(ui, p, t::GAL_KV_TIME, "284 мс", false);
            key_value(ui, p, t::GAL_KV_TYPE, "text/html", false);
        });
    });
    row(ui, p, t::GAL_DIST, |ui| {
        ui.vertical(|ui| {
            ui.set_width(360.0);
            let codes = ["2xx", "3xx", "4xx", "5xx"];
            let values = [1180.0, 96.0, 28.0, 10.0];
            let slices: Vec<Slice<'_>> = (0..4)
                .map(|i| Slice {
                    label: codes[i],
                    value: values[i],
                    color: p.status_codes[i],
                })
                .collect();
            distribution_bar(ui, p, &slices, 360.0);
        });
    });
    row(ui, p, t::GAL_HEADER, |ui| {
        ui.vertical(|ui| {
            ui.set_width(ui.available_width().min(760.0));
            page_bar(ui, p, t::GAL_PAGE_TITLE, t::GAL_PAGE_SUB, |ui| {
                kit::button(
                    ui,
                    p,
                    Some(ph::UPLOAD_FILE),
                    t::GAL_IMPORT,
                    BtnKind::Primary,
                );
                kit::button(ui, p, Some(ph::DOWNLOAD), t::GAL_EXPORT, BtnKind::Outlined);
            });
        });
    });
}

/// Deterministic wave shaped like a day-by-day log series.
fn wave(days: usize, base: f64, swing: f64, phase: f64) -> Vec<f64> {
    (0..days)
        .map(|i| base + swing * (i as f64 * 0.9 + phase).sin().abs() + (i % 7) as f64 * base * 0.02)
        .collect()
}

fn charts(ui: &mut Ui, p: &Palette) {
    let days = 36;
    let labels: Vec<String> = (0..days).map(|i| format!("{:02}.09", i % 30 + 1)).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let bots: Vec<Vec<f64>> = (0..6)
        .map(|k| {
            wave(
                days,
                4000.0 - 500.0 * k as f64,
                1500.0 - 200.0 * k as f64,
                k as f64,
            )
        })
        .collect();
    let series: Vec<Series<'_>> = bots
        .iter()
        .zip(t::GAL_BOTS)
        .enumerate()
        .map(|(k, (v, name))| Series {
            name,
            color: p.series[k],
            values: v,
        })
        .collect();
    let codes: Vec<Vec<f64>> = [8000.0, 700.0, 600.0, 120.0]
        .iter()
        .enumerate()
        .map(|(k, b)| wave(days, *b, *b * 0.2, k as f64 * 2.0))
        .collect();
    let code_series: Vec<Series<'_>> = codes
        .iter()
        .zip(["2xx", "3xx", "4xx", "5xx"])
        .enumerate()
        .map(|(k, (v, name))| Series {
            name,
            color: p.status_codes[k],
            values: v,
        })
        .collect();
    let width = (ui.available_width() - space::MD) / 2.0;
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.label(kit::bold(t::GAL_CHART_VISITS));
            stacked_bar_chart(ui, p, &labels, &series, vec2(width, 190.0));
        });
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.label(kit::bold(t::GAL_CHART_CODES));
            percent_bar_chart(ui, p, &labels, &code_series, vec2(width, 190.0));
        });
    });
    ui.add_space(space::MD);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.label(kit::bold(t::GAL_CHART_BARS));
            bar_chart(ui, p, &labels, &series[0], vec2(width, 150.0));
        });
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.label(kit::bold(t::GAL_CHART_LINES));
            line_chart(ui, p, &labels, &series[..3], vec2(width, 150.0));
        });
    });
    ui.add_space(space::MD);
    row(ui, p, t::GAL_SPARK, |ui| {
        for (k, color) in [p.series[0], p.status_codes[0], p.status_codes[3]]
            .into_iter()
            .enumerate()
        {
            sparkline(ui, &bots[k][..20], color, vec2(110.0, 32.0));
        }
    });
}
