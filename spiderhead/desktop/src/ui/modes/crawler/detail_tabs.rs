//! Per-tab renderers of the details panel: headers+timing, links with the
//! in/out segment, retained HTML, resources, redirects+canonical, history and
//! structured data. Each reads one `details.load` payload; missing or absent
//! evidence renders as its own explicit state, never as a zero.

use crate::ui::kit::icons as ph;
use eframe::egui::{self, vec2, Align, Layout, RichText};
use serde_json::{json, Value};

use super::details::{field_value, summary};
use crate::i18n;
use crate::theme::{space, ty, Palette};
use crate::ui::kit::{self, bold};
use crate::ui::modes::format_cell;
use crate::ui::DesktopApp;

/// Link rows drawn per tab (the data cap stays at `LINK_CAP`).
const LINKS_DISPLAY_CAP: usize = 300;
/// HTML lines drawn per tab.
const HTML_LINE_CAP: usize = 400;
/// History rows drawn.
const HISTORY_CAP: usize = 100;

/// Route a details tab id to its renderer.
pub(super) fn render(
    ui: &mut egui::Ui,
    app: &mut DesktopApp,
    p: &Palette,
    tab: &str,
    v: &std::rc::Rc<Value>,
    counts: Option<&Value>,
    links_dir: &str,
) {
    match tab {
        "links_in" | "links_out" => links(ui, app, p, v, counts, links_dir),
        "headers" | "response" => headers(ui, p, v),
        "html" | "body" => html(ui, app, p, v),
        "resources" => resources(ui, p, v),
        "redirects" => redirects(ui, p, v),
        "history" => history(ui, p, v),
        "structured" => structured(ui, app, p, v),
        _ => summary(ui, p, v),
    }
}

/// Headers + timing: request/response header dictionaries, the timing rail
/// and derived observations (x-robots, caching, compression).
fn headers(ui: &mut egui::Ui, p: &Palette, v: &Value) {
    let get = |k: &str| v.get(k).cloned().unwrap_or(Value::Null);
    // Header payloads are `[[name, value], …]` arrays, preserving order.
    let req = v.get("request_headers").and_then(Value::as_array);
    let resp = v.get("response_headers").and_then(Value::as_array);
    if req.is_none() && resp.is_none() {
        ui.label(RichText::new(i18n::HDR_NO_HEADERS).color(p.weak));
        return;
    }
    let timing_rows = [
        ("requested_at", get("requested_at")),
        ("received_at", get("received_at")),
        ("response_time", get("response_time")),
        ("reported_size_bytes", get("reported_size_bytes")),
        ("method", get("method")),
    ];
    ui.columns(3, |cols| {
        header_card(&mut cols[0], p, i18n::HDR_REQUEST, req);
        header_card(&mut cols[1], p, i18n::HDR_RESPONSE, resp);
        let col = &mut cols[2];
        kit::caption(col, p, i18n::HDR_TIMING);
        col.add_space(space::XS);
        egui::Grid::new("details-timing")
            .num_columns(2)
            .spacing(vec2(space::MD, space::XS))
            .show(col, |ui| {
                for (name, val) in timing_rows {
                    if val.is_null() || val.as_str() == Some("") {
                        continue;
                    }
                    ui.label(RichText::new(i18n::field_label(name)).color(p.weak));
                    field_value(ui, p, name, &val);
                    ui.end_row();
                }
            });
        let hops = v
            .get("redirect_chain")
            .and_then(Value::as_array)
            .map(|c| c.len() as i64)
            .unwrap_or(0);
        if hops > 0 {
            col.label(RichText::new(i18n::timing_hops(hops)).color(p.weak));
        }
        col.add_space(space::MD);
        kit::caption(col, p, i18n::HDR_INSIGHTS);
        col.add_space(space::XS);
        for name in ["x_robots", "cache-control", "content_encoding"] {
            let val = match name {
                "cache-control" => resp
                    .and_then(|pairs| {
                        pairs.iter().find_map(|pair| {
                            let kv = pair.as_array()?;
                            (kv.first()?.as_str()? == "cache-control")
                                .then(|| kv.get(1).cloned())
                                .flatten()
                        })
                    })
                    .unwrap_or(Value::Null),
                _ => get(name),
            };
            if val.is_null() || val.as_str() == Some("") {
                continue;
            }
            col.horizontal(|ui| {
                ui.label(RichText::new(i18n::field_label(name)).color(p.weak));
                ui.label(RichText::new(format_cell(&val)).monospace());
            });
        }
        let effective = get("effective_status_code");
        let status = get("status_code");
        if !effective.is_null() && effective != status {
            col.horizontal(|ui| {
                ui.label(RichText::new(i18n::field_label("effective_status_code")).color(p.weak));
                field_value(ui, p, "effective_status_code", &effective);
            });
        }
    });
}

/// One header list card (`[name, value]` pair rows, monospace).
fn header_card(ui: &mut egui::Ui, p: &Palette, title: &str, headers: Option<&Vec<Value>>) {
    kit::caption(ui, p, title);
    ui.add_space(space::XS);
    let Some(headers) = headers else {
        ui.label(RichText::new("—").color(p.faint));
        return;
    };
    if headers.is_empty() {
        ui.label(RichText::new("—").color(p.faint));
        return;
    }
    egui::Grid::new(format!("details-hdr-{title}"))
        .num_columns(2)
        .striped(true)
        .spacing(vec2(space::MD, space::XS))
        .show(ui, |ui| {
            for pair in headers {
                let kv = pair.as_array();
                let name = kv
                    .and_then(|kv| kv.first())
                    .map(format_cell)
                    .unwrap_or_default();
                let val = kv.and_then(|kv| kv.get(1)).cloned().unwrap_or(Value::Null);
                ui.label(RichText::new(name).monospace().color(p.weak));
                ui.label(RichText::new(format_cell(&val)).monospace());
                ui.end_row();
            }
        });
}

/// Links tab: inbound/outbound segment with badges and the attribute column.
fn links(
    ui: &mut egui::Ui,
    app: &mut DesktopApp,
    p: &Palette,
    v: &Value,
    counts: Option<&Value>,
    links_dir: &str,
) {
    let n_in = counts
        .and_then(|c| c.get("links_in"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let n_out = counts
        .and_then(|c| c.get("links_out"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    ui.horizontal(|ui| {
        for (dir, label, n) in [("in", i18n::SEG_IN, n_in), ("out", i18n::SEG_OUT, n_out)] {
            let text = format!("{label} {n}");
            if kit::tab(ui, p, &text, links_dir == dir).clicked() {
                app.view.links_dir = dir.to_string();
                if let Some(id) = app.model.table.selected {
                    app.act(
                        "details.load",
                        json!({"url_id": id, "tab": format!("links_{dir}")}),
                    );
                }
            }
        }
    });
    ui.add_space(space::SM);
    let empty = Vec::new();
    let items = v.get("items").and_then(Value::as_array).unwrap_or(&empty);
    if items.is_empty() {
        let text = if links_dir == "out" {
            i18n::LINKS_EMPTY_OUT
        } else {
            i18n::LINKS_EMPTY_IN
        };
        ui.label(RichText::new(text).color(p.weak));
        return;
    }
    if v.get("capped").and_then(Value::as_bool).unwrap_or(false) {
        let total = v
            .get("total")
            .and_then(Value::as_i64)
            .unwrap_or(items.len() as i64);
        ui.label(RichText::new(i18n::links_shown(items.len(), total)).color(p.weak));
    }
    egui::Grid::new("details-links")
        .num_columns(5)
        .striped(true)
        .spacing(vec2(space::LG, space::XS))
        .show(ui, |ui| {
            for head in [
                i18n::LINKS_COL_URL,
                i18n::LINKS_COL_ANCHOR,
                i18n::LINKS_COL_ATTR,
                i18n::LINKS_COL_POSITION,
                i18n::COL_STATUS,
            ] {
                ui.label(bold(head).size(ty::CAPTION).color(p.faint));
            }
            ui.end_row();
            for row in items.iter().take(LINKS_DISPLAY_CAP) {
                let url = row.get("url").cloned().unwrap_or(Value::Null);
                ui.add(egui::Label::new(format_cell(&url)).truncate())
                    .on_hover_text(format_cell(&url));
                ui.label(format_cell(row.get("anchor").unwrap_or(&Value::Null)));
                ui.label(RichText::new(link_attr(row)).color(p.weak).monospace());
                ui.label(format_cell(row.get("position").unwrap_or(&Value::Null)));
                let status = row.get("status_code").cloned().unwrap_or(Value::Null);
                field_value(ui, p, "status_code", &status);
                ui.end_row();
            }
        });
    if items.len() > LINKS_DISPLAY_CAP {
        ui.label(RichText::new(i18n::details_more(items.len() - LINKS_DISPLAY_CAP)).color(p.weak));
    }
}

/// `Атрибут` cell: rel keywords, `nofollow` when set, target when not self.
fn link_attr(row: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(rel) = row.get("rel").and_then(Value::as_array) {
        for r in rel {
            if let Some(s) = r.as_str() {
                if !s.is_empty() {
                    parts.push(s.to_string());
                }
            }
        }
    }
    if row
        .get("nofollow")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && !parts.iter().any(|r| r.eq_ignore_ascii_case("nofollow"))
    {
        parts.push("nofollow".to_string());
    }
    if let Some(t) = row.get("target").and_then(Value::as_str) {
        if !t.is_empty() {
            parts.push(t.to_string());
        }
    }
    if parts.is_empty() {
        "—".to_string()
    } else {
        parts.join(" ")
    }
}

/// HTML tab: meta line, in-source search and the source with line numbers.
/// Line-splitting and lowering run once per payload (`HtmlCache`) — the frame
/// loop would otherwise rescan up to 512 KiB of source text every repaint.
fn html(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette, v: &std::rc::Rc<Value>) {
    let bytes = v.get("bytes").and_then(Value::as_u64).unwrap_or(0) as usize;
    let repr = v
        .get("representation")
        .and_then(Value::as_str)
        .unwrap_or("—");
    let capped = v.get("capped").and_then(Value::as_bool).unwrap_or(false);
    let Some(text) = v.get("text").and_then(Value::as_str) else {
        ui.label(RichText::new(i18n::DETAILS_NO_BODY).color(p.weak));
        return;
    };
    let find = app.view.html_find.to_lowercase();
    let cache = &mut app.view.html_cache;
    if cache
        .payload
        .as_ref()
        .is_none_or(|p| !std::rc::Rc::ptr_eq(p, v))
    {
        cache.lines.clear();
        let mut at = 0;
        for piece in text.split_inclusive('\n') {
            // Same terminator handling as `str::lines`: `\n` and `\r\n`.
            let len = piece.strip_suffix('\n').map_or(piece.len(), |l| {
                l.strip_suffix('\r').map_or(l.len(), |r| r.len())
            });
            cache.lines.push(at..at + len);
            at += piece.len();
        }
        cache.lowered = None;
        cache.payload = Some(v.clone());
    }
    if !find.is_empty() && cache.lowered.is_none() {
        cache.lowered = Some(
            cache
                .lines
                .iter()
                .map(|r| text[r.clone()].to_lowercase())
                .collect(),
        );
    }
    let lines = &cache.lines;
    let lowered = cache.lowered.as_deref().unwrap_or(&[]);
    let matches = if find.is_empty() {
        0
    } else {
        lowered.iter().filter(|l| l.contains(&find)).count()
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(i18n::body_meta(bytes, repr, capped)).color(p.weak));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(i18n::html_stats(lines.len(), matches))
                    .size(ty::CAPTION)
                    .color(p.weak),
            );
            ui.add(
                egui::TextEdit::singleline(&mut app.view.html_find)
                    .desired_width(180.0)
                    .hint_text(format!("{}  {}", ph::SEARCH, i18n::HTML_FIND)),
            );
        });
    });
    ui.add_space(space::XS);
    egui::Frame::new()
        .fill(p.surface_hi)
        .corner_radius(crate::theme::radius::MD)
        .inner_margin(egui::Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (i, range) in lines.iter().take(HTML_LINE_CAP).enumerate() {
                let hit = !find.is_empty() && lowered.get(i).is_some_and(|l| l.contains(&find));
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{:>4}", i + 1))
                            .monospace()
                            .size(ty::CAPTION)
                            .color(p.faint),
                    );
                    let rt = RichText::new(&text[range.clone()])
                        .monospace()
                        .size(ty::CAPTION);
                    ui.label(if hit { rt.color(p.accent) } else { rt });
                });
            }
            if lines.len() > HTML_LINE_CAP {
                ui.label(RichText::new(i18n::html_shown(HTML_LINE_CAP, lines.len())).color(p.weak));
            }
        });
}

/// Resources tab: measured image counters and the declared resource refs.
fn resources(ui: &mut egui::Ui, p: &Palette, v: &Value) {
    let images = v.get("images").cloned().unwrap_or(Value::Null);
    let (total, missing) = (
        images.get("total").and_then(Value::as_i64).unwrap_or(0),
        images
            .get("missing_alt")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    );
    if total > 0 || missing > 0 {
        kit::caption(ui, p, i18n::RES_IMAGES_TITLE);
        ui.add_space(space::XS);
        ui.label(RichText::new(i18n::res_images(total, missing)));
        ui.add_space(space::MD);
    }
    kit::caption(ui, p, i18n::RES_DECLARED);
    ui.add_space(space::XS);
    let empty = Vec::new();
    let items = v.get("items").and_then(Value::as_array).unwrap_or(&empty);
    if items.is_empty() {
        ui.label(RichText::new(i18n::RES_EMPTY).color(p.weak));
        let reason = v
            .get("capability")
            .and_then(|c| c.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !reason.is_empty() {
            ui.label(RichText::new(reason).color(p.faint));
        }
        return;
    }
    egui::Grid::new("details-resources")
        .num_columns(5)
        .striped(true)
        .spacing(vec2(space::LG, space::XS))
        .show(ui, |ui| {
            for head in [
                i18n::RES_COL_KIND,
                i18n::RES_COL_URL,
                i18n::COL_STATUS,
                i18n::COL_SIZE,
                i18n::RES_COL_STATE,
            ] {
                ui.label(bold(head).size(ty::CAPTION).color(p.faint));
            }
            ui.end_row();
            for row in items {
                let kind = row.get("kind").and_then(Value::as_str).unwrap_or("");
                ui.label(match kind {
                    "script" => "JS",
                    "stylesheet" => "CSS",
                    other => other,
                });
                let url = row.get("url").cloned().unwrap_or(Value::Null);
                ui.add(egui::Label::new(format_cell(&url)).truncate())
                    .on_hover_text(format_cell(&url));
                field_value(
                    ui,
                    p,
                    "status_code",
                    row.get("status_code").unwrap_or(&Value::Null),
                );
                field_value(
                    ui,
                    p,
                    "size_bytes",
                    row.get("size_bytes").unwrap_or(&Value::Null),
                );
                let state = row
                    .get("capture_state")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let reason = row.get("reason").and_then(Value::as_str).unwrap_or("");
                ui.label(RichText::new(i18n::res_capture(state, reason)).color(p.weak));
                ui.end_row();
            }
        });
}

/// Redirects + canonical: hop cards of the retained chain and the canonical
/// fields of the page.
fn redirects(ui: &mut egui::Ui, p: &Palette, v: &Value) {
    let empty = Vec::new();
    let chain = v.get("chain").and_then(Value::as_array).unwrap_or(&empty);
    if chain.is_empty() {
        ui.label(RichText::new(i18n::REDIRECT_NONE).color(p.weak));
    } else {
        kit::caption(ui, p, &i18n::redirect_hops(chain.len()));
        ui.add_space(space::XS);
        for hop in chain {
            ui.horizontal(|ui| {
                let status = hop.get("status_code").and_then(Value::as_i64);
                let color = kit::status_color(p, status);
                ui.label(RichText::new("●").size(ty::OVERLINE).color(color));
                match status {
                    Some(c) => {
                        ui.label(bold(c.to_string()).color(color));
                    }
                    None => {
                        ui.label(RichText::new("—").color(p.faint));
                    }
                }
                let url = format_cell(hop.get("url").unwrap_or(&Value::Null));
                ui.add(egui::Label::new(url.as_str()).truncate())
                    .on_hover_text(&url);
                ui.label(RichText::new(ph::SUBDIRECTORY_ARROW_RIGHT).color(p.faint));
                let next = format_cell(hop.get("next_url").unwrap_or(&Value::Null));
                ui.add(egui::Label::new(next.as_str()).truncate())
                    .on_hover_text(&next);
            });
        }
    }
    ui.add_space(space::MD);
    kit::caption(ui, p, i18n::CANON_TITLE);
    ui.add_space(space::XS);
    let canonical = v.get("canonical").and_then(Value::as_str).unwrap_or("");
    let note = if canonical.is_empty() {
        i18n::CANON_MISSING
    } else if v
        .get("canonical_self")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        i18n::CANON_SELF
    } else {
        i18n::CANON_OTHER
    };
    ui.label(RichText::new(note).color(p.weak));
    egui::Grid::new("details-canonical")
        .num_columns(2)
        .spacing(vec2(space::MD, space::XS))
        .show(ui, |ui| {
            for name in ["canonical", "final_canonical", "redirect_url", "final_url"] {
                let Some(val) = v.get(name) else { continue };
                if val.is_null() || val.as_str() == Some("") {
                    continue;
                }
                ui.label(RichText::new(i18n::field_label(name)).color(p.weak));
                ui.label(RichText::new(format_cell(val)).monospace());
                ui.end_row();
            }
        });
}

/// History tab: per-scan rows from `url_history` plus the change list.
fn history(ui: &mut egui::Ui, p: &Palette, v: &Value) {
    let total = v.get("scan_total").and_then(Value::as_i64).unwrap_or(0);
    ui.label(RichText::new(i18n::hist_files(total)).color(p.weak));
    let errors = v
        .get("errors")
        .and_then(Value::as_array)
        .map(|e| e.len())
        .unwrap_or(0);
    if errors > 0 {
        ui.label(RichText::new(i18n::hist_errors(errors)).color(p.critical));
    }
    ui.add_space(space::SM);
    let empty = Vec::new();
    let scans = v.get("scans").and_then(Value::as_array).unwrap_or(&empty);
    if scans.is_empty() {
        ui.label(RichText::new("—").color(p.faint));
        return;
    }
    egui::Grid::new("details-history")
        .num_columns(7)
        .striped(true)
        .spacing(vec2(space::LG, space::XS))
        .show(ui, |ui| {
            for head in [
                i18n::HIST_COL_SCAN,
                i18n::HIST_COL_DATE,
                i18n::HIST_COL_STATE,
                i18n::COL_STATUS,
                i18n::COL_TITLE,
                i18n::COL_WORDS,
                i18n::COL_INLINKS,
            ] {
                ui.label(bold(head).size(ty::CAPTION).color(p.faint));
            }
            ui.end_row();
            for scan in scans.iter().take(HISTORY_CAP) {
                field_value(ui, p, "number", scan.get("number").unwrap_or(&Value::Null));
                let date = scan
                    .get("finished_at")
                    .and_then(Value::as_str)
                    .unwrap_or("—");
                ui.label(RichText::new(date).color(p.weak));
                let state = scan.get("state").and_then(Value::as_str).unwrap_or("");
                ui.label(RichText::new(i18n::hist_state(state)).color(p.weak));
                for name in ["status_code", "title", "word_count", "inlinks"] {
                    field_value(ui, p, name, scan.get(name).unwrap_or(&Value::Null));
                }
                ui.end_row();
            }
        });
    let transitions = v
        .get("transitions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let changed: Vec<String> = transitions
        .iter()
        .flat_map(|t| {
            t.get("changed")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|c| c.as_str().map(str::to_owned))
        .collect();
    if !changed.is_empty() {
        ui.add_space(space::MD);
        kit::caption(ui, p, i18n::HIST_TRANSITIONS);
        ui.label(RichText::new(changed.join(", ")).color(p.weak));
    }
}

/// Structured data: representation chips, block list and the validation
/// summary of the first retained evidence item.
fn structured(ui: &mut egui::Ui, app: &mut DesktopApp, p: &Palette, v: &Value) {
    let empty = Vec::new();
    let items = v.get("items").and_then(Value::as_array).unwrap_or(&empty);
    if items.is_empty() {
        ui.label(RichText::new(i18n::SD_ABSENT).color(p.weak));
        return;
    }
    if items.len() > 1 {
        ui.horizontal(|ui| {
            for (i, item) in items.iter().enumerate() {
                let rep = item
                    .get("payload")
                    .and_then(|x| x.get("representation"))
                    .and_then(Value::as_str)
                    .unwrap_or("static");
                if kit::tab(ui, p, rep, app.view.schema_sel == i).clicked() {
                    app.view.schema_sel = i;
                }
            }
        });
        ui.add_space(space::SM);
    }
    let item = &items[app.view.schema_sel.min(items.len() - 1)];
    let payload = item.get("payload").cloned().unwrap_or(Value::Null);
    let state = payload.get("state").and_then(Value::as_str).unwrap_or("");
    if state == "absent" {
        ui.label(RichText::new(i18n::SD_ABSENT).color(p.weak));
        let reason = payload.get("reason").and_then(Value::as_str).unwrap_or("");
        if !reason.is_empty() {
            ui.label(RichText::new(reason).color(p.faint));
        }
        return;
    }
    ui.columns(2, |cols| {
        let col = &mut cols[0];
        kit::caption(col, p, i18n::SD_BLOCKS);
        col.add_space(space::XS);
        let blocks = payload
            .get("blocks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if blocks.is_empty() {
            col.label(RichText::new("—").color(p.faint));
        }
        for block in &blocks {
            let state = block.get("state").and_then(Value::as_str).unwrap_or("?");
            let ord = block.get("ordinal").and_then(Value::as_i64).unwrap_or(0);
            col.horizontal(|ui| {
                ui.label(bold(format!("#{ord}")));
                ui.label(RichText::new(state).color(p.weak));
            });
        }
        let nodes = payload
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for node in &nodes {
            let types = node
                .get("types")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|t| t.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
                .join(", ");
            let id = node.get("id").and_then(Value::as_str).unwrap_or("");
            col.horizontal(|ui| {
                ui.label(RichText::new(ph::SUBDIRECTORY_ARROW_RIGHT).color(p.faint));
                ui.label(bold(&types));
                if !id.is_empty() {
                    ui.label(RichText::new(id).color(p.faint).monospace());
                }
            });
        }
        let col = &mut cols[1];
        kit::caption(col, p, i18n::SD_VALIDATION);
        col.add_space(space::XS);
        let validation = payload.get("validation").cloned().unwrap_or(Value::Null);
        let errors = validation
            .get("entity_errors")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let warnings = validation
            .get("entity_warnings")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let color = if errors > 0 {
            p.critical
        } else if warnings > 0 {
            p.muted
        } else {
            p.opportunity
        };
        col.label(RichText::new(i18n::sd_counts(errors, warnings)).color(color));
        for name in [
            "syntax_state",
            "structural_state",
            "feature_eligibility_state",
        ] {
            if let Some(val) = validation.get(name).and_then(Value::as_str) {
                col.horizontal(|ui| {
                    ui.label(RichText::new(name).color(p.weak));
                    ui.label(RichText::new(val).monospace());
                });
            }
        }
    });
}
