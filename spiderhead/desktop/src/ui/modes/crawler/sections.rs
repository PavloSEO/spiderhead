//! Section tabs above the URL table, the per-section filter drop-down and the
//! right «Обзор» tree. A section is a fixed list of filters; every filter is a
//! `table.filter` call, and its counter (where scan.v1 has one) is the facet
//! of the same id in the last page answer.

use eframe::egui::{self, vec2, Align2, FontId, RichText, Sense};
use serde_json::{json, Value};

use crate::data::query::Query;
use crate::i18n;
use crate::theme::{space, ty, Palette};
use crate::ui::DesktopApp;

/// One section tab: id, caption, filters as (key, caption). The first
/// filter of every section is «Все».
pub struct Section {
    pub id: &'static str,
    pub label: &'static str,
    pub filters: &'static [(&'static str, &'static str)],
}

pub const SECTIONS: &[Section] = &[
    Section {
        id: "internal",
        label: "Внутренние",
        filters: &[("all", "Все")],
    },
    Section {
        id: "codes",
        label: "Коды ответа",
        filters: &[
            ("all", "Все"),
            ("status_2xx", "2xx успех"),
            ("status_3xx", "3xx редирект"),
            ("status_4xx", "4xx ошибка клиента"),
            ("status_5xx", "5xx ошибка сервера"),
            ("no_response", "Нет ответа"),
        ],
    },
    Section {
        id: "title",
        label: "Title",
        filters: &[
            ("all", "Все"),
            ("title_missing", "Отсутствует"),
            ("title_duplicate", "Дубли"),
            ("title_over_60", "Длиннее 60"),
        ],
    },
    Section {
        id: "description",
        label: "Meta Description",
        filters: &[
            ("all", "Все"),
            ("meta_description_missing", "Отсутствует"),
            ("description_duplicate", "Дубли"),
            ("meta_description_over_160", "Длиннее 160"),
        ],
    },
    Section {
        id: "h1",
        label: "H1",
        filters: &[
            ("all", "Все"),
            ("h1_missing", "Отсутствует"),
            ("h1_multiple", "Несколько"),
        ],
    },
    Section {
        id: "canonical",
        label: "Canonical",
        filters: &[
            ("all", "Все"),
            ("canonical_missing", "Отсутствует"),
            ("canonical_other", "На другую страницу"),
            ("canonical_non_indexable", "На неиндексируемую"),
        ],
    },
    Section {
        id: "directives",
        label: "Директивы",
        filters: &[
            ("all", "Все"),
            ("noindex_meta", "Noindex в meta"),
            ("noindex_header", "Noindex в X-Robots-Tag"),
            ("robots_blocked", "Закрыты robots.txt"),
        ],
    },
    Section {
        id: "images",
        label: "Изображения",
        filters: &[("all", "Все"), ("images_missing_alt", "Без alt")],
    },
    Section {
        id: "links",
        label: "Ссылки",
        filters: &[
            ("all", "Все"),
            ("with_inlinks", "С входящими"),
            ("no_outlinks", "Без исходящих"),
            ("hreflang", "С hreflang"),
        ],
    },
    Section {
        id: "sitemap",
        label: "Sitemap",
        filters: &[("all", "Все"), ("in_sitemap", "В sitemap")],
    },
];

/// `table.filter` params of a filter key: facet keys map to the single
/// filter that reproduces their counter, the rest are presets.
pub fn filter_params(key: &str) -> Value {
    let f = |col: &str, op: &str, val: Value| json!({"filters": [{"column": col, "op": op, "value": val}], "preset": null});
    let clear = |mut v: Value| {
        v["issue_check"] = Value::Null;
        v["issue_severity"] = Value::Null;
        v
    };
    clear(match key {
        "all" => json!({"filters": [], "preset": null}),
        "status_2xx" => f("status_class", "eq", json!("2xx")),
        "status_3xx" => f("status_class", "eq", json!("3xx")),
        "status_4xx" => f("status_class", "eq", json!("4xx")),
        "status_5xx" => f("status_class", "eq", json!("5xx")),
        "title_duplicate" => json!({"preset": "duplicate_titles", "filters": []}),
        "canonical_other" => f("canonical_other", "eq", json!(true)),
        "with_inlinks" => json!({"preset": "inlinks", "filters": []}),
        preset => json!({"preset": preset, "filters": []}),
    })
}

/// True when the live query is exactly this filter (no issue filter on top).
fn is_active(key: &str, q: &Query) -> bool {
    let params = filter_params(key);
    q.issue_check.is_null()
        && q.issue_severity.is_null()
        && json!(q.filters) == params["filters"]
        && q.preset.clone().map(Value::String).unwrap_or(Value::Null) == params["preset"]
}

/// The section shown: the remembered tab, unless the live query belongs to
/// another section only (a drill-down or a control-channel filter).
fn current(app: &DesktopApp) -> &'static Section {
    let q = &app.model.table.query;
    let remembered = SECTIONS
        .iter()
        .find(|s| s.id == app.view.section)
        .unwrap_or(&SECTIONS[0]);
    if remembered.filters.iter().any(|(k, _)| is_active(k, q)) {
        return remembered;
    }
    SECTIONS
        .iter()
        .find(|s| s.filters.iter().skip(1).any(|(k, _)| is_active(k, q)))
        .unwrap_or(remembered)
}

/// The answer whose facets count the whole crawl: the live one when no
/// filter is applied, else the one remembered for this scan. Facets of a
/// filtered answer count only the filtered rows.
pub fn crawl_counts(app: &mut DesktopApp) -> Option<Value> {
    let scan = app.model.scan.as_ref()?.path.display().to_string();
    let last = app.model.table.last.clone();
    if is_active("all", &app.model.table.query) && app.filter_text.is_empty() {
        if let Some(v) = &last {
            app.view.crawl_counts = Some((scan.clone(), v.clone()));
        }
    }
    match &app.view.crawl_counts {
        Some((k, v)) if *k == scan => Some(v.clone()),
        _ => last,
    }
}

/// Counter of a filter key: `Some(n)` exact, `None` when scan.v1 has no
/// counter for it. A capped counter keeps its `+`.
fn count(last: Option<&Value>, key: &str) -> Option<String> {
    let n = last?.get("facets")?.get(key)?.as_i64()?;
    let capped = last
        .and_then(|l| l.get("facet_states"))
        .and_then(|s| s.get(key))
        .and_then(Value::as_str)
        .is_some_and(|st| st != "exact");
    Some(format!("{n}{}", if capped { "+" } else { "" }))
}

fn apply(app: &mut DesktopApp, section: &str, key: &str) {
    app.view.section = section.to_string();
    app.filter_text.clear();
    app.act("table.filter", filter_params(key));
}

/// The tab row: one underlined tab per section, horizontally scrollable when
/// the window is narrower than the row.
pub fn tab_strip(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette) {
    let active = current(app).id;
    let mut pick = None;
    egui::ScrollArea::horizontal()
        .id_salt("section-tabs")
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for s in SECTIONS {
                    if tab(ui, p, s.label, s.id == active).clicked() {
                        pick = Some(s.id);
                    }
                }
            });
        });
    let rule = ui.min_rect();
    ui.painter().hline(
        rule.x_range(),
        rule.bottom(),
        egui::Stroke::new(1.0_f32, p.border),
    );
    if let Some(id) = pick {
        apply(app, id, "all");
    }
}

fn tab(ui: &mut egui::Ui, p: &Palette, label: &str, on: bool) -> egui::Response {
    let font = FontId::new(ty::CAPTION, egui::FontFamily::Proportional);
    let ink = if on { p.text } else { p.weak };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, ink);
    let size = vec2(galley.size().x + 2.0 * space::MD, 28.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if response.hovered() && !on {
        ui.painter().rect_filled(rect, 0.0, p.hover);
    }
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, ink);
    if on {
        let bar = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 4.0, rect.bottom() - 2.0),
            egui::pos2(rect.right() - 4.0, rect.bottom()),
        );
        ui.painter().rect_filled(bar, 1.0, p.accent);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The section's filter drop-down: «Все» first, counters where they exist.
pub fn filter_combo(ui: &mut egui::Ui, app: &mut DesktopApp, last: Option<&Value>) {
    let section = current(app);
    let q = &app.model.table.query;
    let caption = |key: &str, label: &str| match count(last, key) {
        Some(n) => format!("{label}  {n}"),
        None => label.to_string(),
    };
    let shown = section
        .filters
        .iter()
        .find(|(k, _)| is_active(k, q))
        .map(|(k, l)| caption(k, l))
        .unwrap_or_else(|| i18n::FILTER_CUSTOM.to_string());
    let mut pick = None;
    egui::ComboBox::from_id_salt("section-filter")
        .width(200.0)
        .selected_text(RichText::new(shown).size(ty::CAPTION))
        .show_ui(ui, |ui| {
            for (key, label) in section.filters {
                let on = is_active(key, &app.model.table.query);
                if ui.selectable_label(on, caption(key, label)).clicked() {
                    pick = Some(*key);
                }
            }
        });
    if let Some(key) = pick {
        apply(app, section.id, key);
    }
}

/// The right «Обзор» tree: crawl summary, then every section with its
/// filters, counters and share of all URLs. A click opens that filter.
pub fn overview(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette, last: Option<&Value>) {
    let total = last
        .and_then(|l| l.get("total"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let mut pick = None;
    egui::ScrollArea::vertical()
        .id_salt("overview-tree")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            root(ui, p, i18n::OVERVIEW_SUMMARY);
            for key in ["all", "internal", "external"] {
                let label = i18n::facet_label(key);
                row(ui, p, label, count(last, key), total, false);
            }
            for s in SECTIONS {
                root(ui, p, s.label);
                for (key, label) in s.filters {
                    let on = current(app).id == s.id && is_active(key, &app.model.table.query);
                    if row(ui, p, label, count(last, key), total, on).clicked() {
                        pick = Some((s.id, *key));
                    }
                }
            }
        });
    if let Some((s, key)) = pick {
        apply(app, s, key);
    }
}

fn root(ui: &mut egui::Ui, p: &Palette, label: &str) {
    ui.add_space(space::XS);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    ui.painter().text(
        rect.left_center() + vec2(space::XS, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::new(ty::CAPTION, crate::theme::semibold()),
        p.text,
    );
}

/// One tree row: caption indented, counter and percent right-aligned.
fn row(
    ui: &mut egui::Ui,
    p: &Palette,
    label: &str,
    n: Option<String>,
    total: i64,
    on: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
    if on {
        ui.painter().rect_filled(rect, 0.0, p.accent_soft);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, p.hover);
    }
    let font = FontId::new(ty::CAPTION, egui::FontFamily::Proportional);
    let mono = FontId::new(ty::CAPTION, egui::FontFamily::Monospace);
    let pct_x = rect.right() - space::XS;
    let num_x = pct_x - 52.0;
    // The caption is clipped before the counters so the two never overlap.
    let clip = egui::Rect::from_min_max(rect.min, egui::pos2(num_x - 40.0, rect.max.y));
    ui.painter().with_clip_rect(clip).text(
        rect.left_center() + vec2(space::LG, 0.0),
        Align2::LEFT_CENTER,
        label,
        font,
        if on { p.accent } else { p.text },
    );
    if let Some(n) = n {
        let pct = n
            .trim_end_matches('+')
            .parse::<f64>()
            .ok()
            .filter(|_| total > 0)
            .map(|v| format!("{:.0}%", v * 100.0 / total as f64))
            .unwrap_or_default();
        ui.painter().text(
            egui::pos2(num_x, rect.center().y),
            Align2::RIGHT_CENTER,
            n,
            mono.clone(),
            p.text,
        );
        ui.painter().text(
            egui::pos2(pct_x, rect.center().y),
            Align2::RIGHT_CENTER,
            pct,
            mono,
            p.weak,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facet_keys_map_to_the_filter_behind_their_counter() {
        assert_eq!(
            filter_params("status_4xx")["filters"],
            json!([{"column": "status_class", "op": "eq", "value": "4xx"}])
        );
        assert_eq!(
            filter_params("title_duplicate")["preset"],
            json!("duplicate_titles")
        );
        assert_eq!(filter_params("h1_missing")["preset"], json!("h1_missing"));
    }

    #[test]
    fn every_filter_key_is_a_known_preset_or_facet_filter() {
        for s in SECTIONS {
            assert_eq!(s.filters[0].0, "all", "{}", s.id);
            for (key, _) in s.filters {
                let params = filter_params(key);
                if let Some(preset) = params["preset"].as_str() {
                    assert!(
                        crate::data::presets::PRESET_IDS.contains(&preset),
                        "{key}: unknown preset {preset}"
                    );
                }
            }
        }
    }
}
