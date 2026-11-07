//! Native PDF rendering of the technical-audit overview model (issue #43):
//! no browser, no venv, no intermediate HTML. The same bounded model the
//! Chromium path prints, drawn directly through `krilla` with the embedded
//! Roboto faces (Cyrillic coverage). Selected by default; the Chromium path
//! stays available via `SPIDERHEAD_PDF_ENGINE=chromium`.

use std::path::Path;

use krilla::color::rgb;
use krilla::geom::Point;
use krilla::page::PageSettings;
use krilla::paint::Fill;
use krilla::text::{Font, TextDirection};
use krilla::Document;
use serde_json::{json, Value};

/// A4 in PostScript points.
const A4_W: f32 = 595.28;
const A4_H: f32 = 841.89;
const MARGIN: f32 = 48.0;
const BODY: f32 = 10.0;
const TITLE: f32 = 18.0;
/// Rows of findings the overview shows (the same bound as the Chromium path).
const MAX_ROWS: usize = 32;

const ROBOTO_REGULAR: &[u8] = include_bytes!("../../desktop/assets/fonts/Roboto-Regular.ttf");
const ROBOTO_MEDIUM: &[u8] = include_bytes!("../../desktop/assets/fonts/Roboto-Medium.ttf");

/// One text line of the layout.
struct LineSpec {
    text: String,
    size: f32,
    bold: bool,
    dark: bool,
}

/// The whole report as flowing lines, split into pages up front.
struct Layout {
    pages: Vec<Vec<LineSpec>>,
    y: f32,
    lang_ru: bool,
}

impl Layout {
    fn new(lang_ru: bool) -> Self {
        Layout {
            pages: vec![Vec::new()],
            y: MARGIN,
            lang_ru,
        }
    }

    /// The footer word follows the report language.
    fn footer_word(&self) -> &'static str {
        if self.lang_ru {
            "страница"
        } else {
            "page"
        }
    }

    fn line(&mut self, text: &str, size: f32, bold: bool, gap_after: f32, dark: bool) {
        if self.y + size + gap_after > A4_H - MARGIN {
            self.pages.push(Vec::new());
            self.y = MARGIN;
        }
        self.y += size + gap_after;
        self.pages
            .last_mut()
            .expect("a page always exists")
            .push(LineSpec {
                text: text.to_owned(),
                size,
                bold,
                dark,
            });
    }
}

/// Cut `text` to about `width_pt` points at `size`, with an ellipsis.
fn fit(text: &str, size: f32, width_pt: f32) -> String {
    let per_char = size * 0.55;
    let max = ((width_pt / per_char) as usize).max(4);
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// A value rendered as its plain form; a missing value as an em dash.
fn num(value: Option<&Value>) -> String {
    match value {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "—".to_owned(),
    }
}

/// Render the overview model to `path` natively. Returns the same result
/// shape as the Chromium path, with `engine: "native"` and the page count.
pub fn write_pdf_report_native(model: &Value, path: &Path, lang: &str) -> Value {
    let ru = lang == "ru";
    let mut layout = Layout::new(ru);

    let title = if ru {
        "Технический SEO-аудит"
    } else {
        "Technical SEO audit"
    };
    layout.line(title, TITLE, true, 8.0, true);
    let source = model.get("source").cloned().unwrap_or(Value::Null);
    let domain = source.get("domain").and_then(Value::as_str).unwrap_or("");
    let generated = source
        .get("generated_at")
        .and_then(Value::as_str)
        .unwrap_or("");
    layout.line(
        &fit(domain, BODY, A4_W - 2.0 * MARGIN),
        BODY,
        false,
        2.0,
        false,
    );
    layout.line(generated, BODY, false, 12.0, false);

    let summary_label = if ru { "Сводка" } else { "Summary" };
    layout.line(summary_label, BODY + 2.0, true, 6.0, true);
    let summary_source = model
        .get("summary")
        .and_then(|s| s.get("source"))
        .cloned()
        .unwrap_or(Value::Null);
    let by_severity = summary_source
        .get("findings_by_severity")
        .cloned()
        .unwrap_or(json!({}));
    let pages_label = if ru {
        "Страниц проверено"
    } else {
        "Pages checked"
    };
    let findings_label = if ru { "Находок" } else { "Findings" };
    layout.line(
        &format!(
            "{}: {}   {}: {}   {}: {} / {} / {}",
            pages_label,
            num(summary_source.get("pages_checked")),
            findings_label,
            num(summary_source.get("findings_total")),
            if ru { "критич" } else { "crit" },
            num(by_severity.get("critical")),
            num(by_severity.get("warning")),
            num(by_severity.get("notice")),
        ),
        BODY,
        false,
        12.0,
        false,
    );

    let findings_label = if ru {
        "Находки (обзор)"
    } else {
        "Findings (overview)"
    };
    layout.line(findings_label, BODY + 2.0, true, 6.0, true);
    let rows = model
        .get("findings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for row in rows.iter().take(MAX_ROWS) {
        let record = row.get("record").cloned().unwrap_or(Value::Null);
        let display = row.get("display").cloned().unwrap_or(Value::Null);
        let severity = record
            .get("severity")
            .and_then(Value::as_str)
            .unwrap_or("—");
        let check = display
            .get("check_key")
            .and_then(Value::as_str)
            .or_else(|| record.get("check").and_then(Value::as_str))
            .unwrap_or("—");
        let url = record.get("url").and_then(Value::as_str).unwrap_or("");
        layout.line(
            &format!(
                "{}  {}  {}",
                fit(&format!("{severity:<8}"), BODY, 46.0),
                fit(check, BODY, 170.0),
                fit(url, BODY, A4_W - 2.0 * MARGIN - 240.0)
            ),
            BODY,
            false,
            4.0,
            false,
        );
    }
    if rows.is_empty() {
        let none = if ru {
            "Находок нет"
        } else {
            "No findings recorded"
        };
        layout.line(none, BODY, false, 4.0, false);
    }

    let font = match Font::new(ROBOTO_REGULAR.to_vec().into(), 0) {
        Some(font) => font,
        None => {
            return json!({
                "ok": false, "format": "pdf", "engine": "native", "status": "failed",
                "error": "embedded font rejected",
            })
        }
    };
    let bold = Font::new(ROBOTO_MEDIUM.to_vec().into(), 0).unwrap_or_else(|| font.clone());
    let mut document = Document::new();
    let dark_fill = Fill {
        paint: rgb::Color::new(20, 20, 20).into(),
        ..Default::default()
    };
    let gray_fill = Fill {
        paint: rgb::Color::new(60, 60, 60).into(),
        ..Default::default()
    };
    let footer_fill = Fill {
        paint: rgb::Color::new(120, 120, 120).into(),
        ..Default::default()
    };
    let mut written_pages = 0usize;
    for (index, lines) in layout.pages.iter().enumerate() {
        if lines.is_empty() && layout.pages.len() > 1 {
            continue;
        }
        let mut page =
            document.start_page_with(PageSettings::from_wh(A4_W, A4_H).expect("A4 size"));
        let mut surface = page.surface();
        let mut y = MARGIN;
        for spec in lines {
            y += spec.size;
            surface.set_fill(Some(if spec.dark {
                dark_fill.clone()
            } else {
                gray_fill.clone()
            }));
            surface.draw_text(
                Point::from_xy(MARGIN, y),
                if spec.bold {
                    bold.clone()
                } else {
                    font.clone()
                },
                spec.size,
                &spec.text,
                false,
                TextDirection::Auto,
            );
            y += 2.0;
        }
        surface.set_fill(Some(footer_fill.clone()));
        surface.draw_text(
            Point::from_xy(MARGIN, A4_H - 18.0),
            font.clone(),
            8.0,
            &format!("{} {}", layout.footer_word(), index + 1),
            false,
            TextDirection::Auto,
        );
        written_pages += 1;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let bytes = match document.finish() {
        Ok(bytes) => bytes,
        Err(e) => {
            return json!({
                "ok": false, "format": "pdf", "engine": "native", "status": "failed",
                "error": format!("pdf serialization failed: {e:?}"),
            })
        }
    };
    if let Err(e) = std::fs::write(path, &bytes) {
        return json!({
            "ok": false, "format": "pdf", "engine": "native", "status": "failed",
            "error": format!("pdf write failed: {e}"),
        });
    }
    json!({
        "ok": true, "format": "pdf", "engine": "native", "status": "complete",
        "pages": written_pages,
    })
}
