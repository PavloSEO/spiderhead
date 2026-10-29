//! Render a Metrica traffic document as a static, print-ready HTML dashboard.
//!
//! Branding is shared with the audit-PDF renderer: a [`Brand`] sets name, colours,
//! font stack and logo text.

use std::path::Path;

use regex::Regex;
use serde_json::Value;

use crate::join::IsoDate;
use crate::py::{html_escape, repr};

mod css;
mod labels;
mod pages;

/// The `schema` value a renderable traffic document must carry.
pub const SCHEMA: &str = "spiderhead.metrika-traffic/1";

pub const PX_PER_MM: f64 = 96.0 / 25.4;
pub const DEFAULT_FONT_STACK: &str =
    "\"Segoe UI\", \"Helvetica Neue\", Arial, \"Noto Sans\", \"DejaVu Sans\", \"Liberation Sans\", sans-serif";
pub const OTHER_COLOR: &str = "#CBD5E1";
pub const GSC_ROWS_PER_PAGE: usize = 20;

const PALETTE: [&str; 10] = [
    "#0EA5A4", "#F59E0B", "#E11D48", "#7C3AED", "#16A34A", "#0284C7", "#DB2777", "#CA8A04",
    "#475569", "#9333EA",
];

/// Report branding; colours are `#RRGGBB`.
#[derive(Debug, Clone, PartialEq)]
pub struct Brand {
    /// Publisher name shown in the header.
    pub name: String,
    /// Accent colour.
    pub accent: String,
    /// Body text colour.
    pub ink: String,
    /// Card background.
    pub card: String,
    /// Table header background.
    pub table_header: String,
    /// Positive-delta colour.
    pub positive: String,
    /// Negative-delta colour.
    pub negative: String,
    /// CSS font stack.
    pub font_stack: String,
    /// Short logo suffix shown after the name.
    pub logo_text: String,
}

impl Default for Brand {
    fn default() -> Self {
        Self {
            name: String::new(),
            accent: "#2F5BD3".to_owned(),
            ink: "#0F1F3D".to_owned(),
            card: "#F5F7FB".to_owned(),
            table_header: "#DCE3EE".to_owned(),
            positive: "#1E8E3E".to_owned(),
            negative: "#C62828".to_owned(),
            font_stack: DEFAULT_FONT_STACK.to_owned(),
            logo_text: String::new(),
        }
    }
}

const COLOR_KEYS: [&str; 6] = [
    "accent",
    "ink",
    "card",
    "table_header",
    "positive",
    "negative",
];

/// Build a [`Brand`] from `None`, a dict, inline JSON text or a JSON file path.
pub fn load_brand(value: Option<&Value>) -> Result<Brand, String> {
    let document = match value {
        None => return Ok(Brand::default()),
        Some(Value::Null) => return Ok(Brand::default()),
        Some(Value::String(text)) if text.trim().is_empty() => return Ok(Brand::default()),
        Some(Value::String(text)) => {
            let text = text.trim();
            if text.starts_with('{') {
                serde_json::from_str::<Value>(text)
                    .map_err(|e| format!("brand is not valid JSON: {e}"))?
            } else {
                let path = Path::new(text);
                if !path.is_file() {
                    return Err(format!("brand file not found: {text}"));
                }
                let body = std::fs::read_to_string(path)
                    .map_err(|e| format!("brand file is not valid JSON: {e}"))?;
                serde_json::from_str::<Value>(&body)
                    .map_err(|e| format!("brand file is not valid JSON: {e}"))?
            }
        }
        Some(other) => other.clone(),
    };
    if !document.is_object() {
        return Err("brand must be an object, inline JSON, or a JSON file path".to_owned());
    }
    let known: [&str; 9] = [
        "name",
        "accent",
        "ink",
        "card",
        "table_header",
        "positive",
        "negative",
        "font_stack",
        "logo_text",
    ];
    let mut unknown: Vec<String> = document
        .as_object()
        .map(|m| {
            m.keys()
                .filter(|k| !known.contains(&k.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    unknown.sort();
    if !unknown.is_empty() {
        let mut expected: Vec<&str> = known.to_vec();
        expected.sort();
        return Err(format!(
            "unknown brand keys: {unknown:?}; expected some of {expected:?}"
        ));
    }
    let fields = document.as_object().cloned().unwrap_or_default();
    let get = |key: &str| fields.get(key);
    let color_re = Regex::new(r"^#[0-9A-Fa-f]{6}$").unwrap();
    for key in COLOR_KEYS {
        if let Some(raw) = get(key) {
            if !raw.as_str().is_some_and(|c| color_re.is_match(c)) {
                return Err(format!(
                    "brand.{key} must be a #RRGGBB colour, got {}",
                    repr(raw)
                ));
            }
        }
    }
    let font_re = Regex::new(r#"^[A-Za-z0-9 ,'"\-]{1,300}$"#).unwrap();
    if let Some(font) = get("font_stack") {
        if !font.as_str().is_some_and(|f| font_re.is_match(f)) {
            return Err(
                "brand.font_stack may contain only font names, quotes, commas and spaces"
                    .to_owned(),
            );
        }
    }
    for key in ["name", "logo_text"] {
        let raw = get(key);
        let ok = match raw {
            None => true,
            Some(v) => v.as_str().is_some_and(|t| t.chars().count() <= 80),
        };
        if !ok {
            return Err(format!("brand.{key} must be text of at most 80 characters"));
        }
    }
    let mut brand = Brand::default();
    for (key, item) in &fields {
        let Some(text) = item.as_str() else { continue };
        match key.as_str() {
            "name" => brand.name = text.to_owned(),
            "logo_text" => brand.logo_text = text.to_owned(),
            "font_stack" => brand.font_stack = text.to_owned(),
            "accent" => brand.accent = text.to_owned(),
            "ink" => brand.ink = text.to_owned(),
            "card" => brand.card = text.to_owned(),
            "table_header" => brand.table_header = text.to_owned(),
            "positive" => brand.positive = text.to_owned(),
            "negative" => brand.negative = text.to_owned(),
            _ => {}
        }
    }
    Ok(brand)
}

// --- labels ----------------------------------------------------------------

/// Locale-aware formatting of numbers, percentages, durations and dates.
#[derive(Clone, Copy)]
pub(crate) struct Fmt<'a> {
    pub lang: &'a str,
    pub labels: &'static Value,
}

fn label<'a>(labels: &'a Value, key: &str) -> &'a str {
    labels.get(key).and_then(Value::as_str).unwrap_or("")
}

fn label_map<'a>(labels: &'a Value, key: &str, entry: &str) -> &'a str {
    labels
        .get(key)
        .and_then(Value::as_object)
        .and_then(|m| m.get(entry))
        .and_then(Value::as_str)
        .unwrap_or("")
}

impl<'b> Fmt<'b> {
    fn t<'a>(&self, key: &str, scope: &str) -> &'a Value {
        let all_key = format!("{key}_all");
        if scope == "all" && self.labels.get(&all_key).is_some() {
            return &self.labels[&all_key];
        }
        &self.labels[key]
    }

    /// `f"{value:,.{digits}f}"` with the locale's group/decimal separators.
    pub(crate) fn number(&self, value: Option<f64>, digits: usize) -> String {
        let Some(value) = value else {
            return label(self.labels, "na").to_owned();
        };
        let text = comma_group(&format!("{value:.digits$}"));
        text.replace(',', "\u{0}")
            .replace('.', label(self.labels, "decimal"))
            .replace('\u{0}', label(self.labels, "group"))
    }

    /// A share that rounds to zero but is not zero reads as "<0.1%", not "0.0%".
    fn share(&self, value: Option<f64>) -> String {
        if let Some(v) = value {
            if 0.0 < v && v < 0.05 {
                return format!("<{}", self.pct(Some(0.1), 1));
            }
        }
        self.pct(value, 1)
    }

    fn pct(&self, value: Option<f64>, digits: usize) -> String {
        let Some(value) = value else {
            return label(self.labels, "na").to_owned();
        };
        label(self.labels, "percent").replace("{value}", &self.number(Some(value), digits))
    }

    fn signed_pct(&self, value: f64) -> String {
        let sign = if value > 0.0 {
            "+"
        } else if value < 0.0 {
            "\u{2212}"
        } else {
            ""
        };
        format!("{}{}", sign, self.pct(Some(value.abs()), 1))
    }

    fn duration(&self, seconds: Option<f64>) -> String {
        let Some(seconds) = seconds else {
            return label(self.labels, "na").to_owned();
        };
        let total = seconds.round_ties_even() as i64;
        format!("{}:{:02}", total.div_euclid(60), total.rem_euclid(60))
    }

    fn kpi(&self, key: &str, value: Option<f64>) -> String {
        match key {
            "bounce_rate" | "new_visitors_pct" | "repeat_visitors_2_3_pct" => self.pct(value, 1),
            "avg_visit_duration_seconds" => self.duration(value),
            "page_depth" => self.number(value, 2),
            "visits_per_day" => self.number(value, 1),
            _ => self.number(value, 0),
        }
    }

    /// `"{day} {month_short}"` (+ ` {year}` when `year`) — errors on a bad date.
    fn day(&self, iso: &str, year: bool) -> Result<String, String> {
        let day = IsoDate::parse(iso)
            .ok_or_else(|| format!("invalid ISO date in traffic document: {iso}"))?;
        let month = self
            .labels
            .get("months_short")
            .and_then(Value::as_array)
            .and_then(|m| m.get((day.month() - 1) as usize))
            .and_then(Value::as_str)
            .unwrap_or("");
        let mut out = format!("{} {}", day.day(), month);
        if year {
            out += &format!(" {}", day.year());
        }
        Ok(out)
    }

    /// `"{month_nominative|short} {year}"`.
    fn month(&self, iso: &str) -> Result<String, String> {
        let day = IsoDate::parse(iso)
            .ok_or_else(|| format!("invalid ISO date in traffic document: {iso}"))?;
        let index = (day.month() - 1) as usize;
        let names = self
            .labels
            .get("months_nominative")
            .and_then(Value::as_array)
            .or_else(|| self.labels.get("months_short").and_then(Value::as_array));
        let name = names
            .and_then(|n| n.get(index))
            .and_then(Value::as_str)
            .unwrap_or("");
        Ok(format!("{} {}", name, day.year()))
    }

    /// `"{day(date1)} – {day(date2)}"`.
    fn span(&self, span: &Value) -> Result<String, String> {
        let date1 = span.get("date1").and_then(Value::as_str).unwrap_or("");
        let date2 = span.get("date2").and_then(Value::as_str).unwrap_or("");
        Ok(format!(
            "{} – {}",
            self.day(date1, true)?,
            self.day(date2, true)?
        ))
    }
}

/// Insert `,` group separators into the integer part of a formatted number.
fn comma_group(text: &str) -> String {
    let (sign, rest) = match text.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", text),
    };
    let (int, frac) = match rest.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (rest, None),
    };
    let mut grouped = String::with_capacity(int.len() + int.len() / 3);
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    match frac {
        Some(frac) => format!("{sign}{grouped}.{frac}"),
        None => format!("{sign}{grouped}"),
    }
}

// --- small HTML helpers ------------------------------------------------------

/// `str.format(**values)` for the `{placeholder}` templates in the labels.
fn tpl(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for (key, value) in values {
        out = out.replace(&format!("{{{key}}}"), value);
    }
    out
}

/// The text of a JSON value as its plain string form.
fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// A number cell value: ints and floats both read through `as_f64`.
fn num(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

/// Blend `colour` toward `other` by `weight` (0 keeps colour, 1 gives other).
fn mix(colour: &str, other: &str, weight: f64) -> String {
    let parse = |c: &str| -> [f64; 3] {
        [1, 3, 5].map(|i| f64::from(u8::from_str_radix(&c[i..i + 2], 16).unwrap_or(0)))
    };
    let a = parse(colour);
    let b = parse(other);
    let mut out = String::from("#");
    for (x, y) in a.iter().zip(b.iter()) {
        out += &format!("{:02X}", (x + (y - x) * weight).round_ties_even() as i64);
    }
    out
}

fn delta_html(
    fmt: &Fmt,
    delta: Option<f64>,
    status: Option<&str>,
    lower_is_better: bool,
) -> String {
    if status == Some("new") {
        return format!(
            "<span class=\"delta new\">{}</span>",
            html_escape(label(fmt.labels, "new"))
        );
    }
    let Some(delta) = delta else {
        return format!(
            "<span class=\"delta none\">{}</span>",
            html_escape(label(fmt.labels, "na"))
        );
    };
    if delta == 0.0 {
        return format!(
            "<span class=\"delta flat\">{}</span>",
            html_escape(&fmt.pct(Some(0.0), 1))
        );
    }
    let up = delta > 0.0;
    let good = up != lower_is_better;
    let arrow = if up { "▲" } else { "▼" };
    let css = if good { "good" } else { "bad" };
    format!(
        "<span class=\"delta {css}\"><i>{arrow}</i>{}</span>",
        html_escape(&fmt.signed_pct(delta))
    )
}

/// The row label: a dictionary name for known ids, else `name`, else "(not set)".
fn label_for(fmt: &Fmt, block_key: &str, row: &Value) -> String {
    let names_key = match block_key {
        "channels" => "channel_names",
        "devices" => "device_names",
        "gender" => "gender_names",
        "age" => "age_names",
        _ => "",
    };
    let id = row.get("id").and_then(Value::as_str);
    if let (Some(names), Some(id)) = (fmt.labels.get(names_key), id) {
        if let Some(name) = names.get(id).and_then(Value::as_str) {
            return name.to_owned();
        }
    }
    row.get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| label(fmt.labels, "not_set"))
        .to_owned()
}

fn unavailable(fmt: &Fmt, block: Option<&Value>, tall: bool) -> String {
    let reason = block
        .and_then(|b| b.get("reason"))
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| label(fmt.labels, "na").to_owned());
    let css = if tall {
        "unavailable tall"
    } else {
        "unavailable"
    };
    format!(
        "<div class=\"card {css}\"><div class=\"ua-title\">{}</div><div class=\"ua-reason\">{}</div></div>",
        html_escape(label(fmt.labels, "unavailable")),
        html_escape(&reason)
    )
}

#[allow(clippy::too_many_arguments)]
fn table(
    fmt: &Fmt,
    block_key: &str,
    block: Option<&Value>,
    limit: usize,
    numbered: bool,
    show_share: bool,
    wide_label: bool,
    roomy: bool,
) -> String {
    if block.and_then(|b| b.get("status")).and_then(Value::as_str) != Some("ok") {
        return unavailable(fmt, block, false);
    }
    let block = block.unwrap_or(&Value::Null);
    let empty: Vec<Value> = Vec::new();
    let rows = block
        .get("rows")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let rows = &rows[..rows.len().min(limit)];
    let head = label_map(fmt.labels, "dimension", block_key);
    let mut cols = String::new();
    if numbered {
        cols += "<col class=\"c-num\">";
    }
    cols += "<col class=\"c-label\"><col class=\"c-val\">";
    if show_share {
        cols += "<col class=\"c-share\">";
    }
    cols += "<col class=\"c-delta\">";
    let mut th = String::new();
    if numbered {
        th += "<th class=\"num idx\">#</th>";
    }
    th += &format!("<th>{}</th>", html_escape(head));
    th += &format!(
        "<th class=\"num\">{}</th>",
        html_escape(label(fmt.labels, "visits"))
    );
    if show_share {
        th += &format!(
            "<th class=\"num\">{}</th>",
            html_escape(label(fmt.labels, "share"))
        );
    }
    th += &format!(
        "<th class=\"num\">{}</th>",
        html_escape(label(fmt.labels, "change"))
    );
    let mut body = String::new();
    for (index, row) in rows.iter().enumerate() {
        let mut cells = String::new();
        if numbered {
            cells += &format!("<td class=\"num idx\">{}.</td>", index + 1);
        }
        let label = label_for(fmt, block_key, row);
        cells += &format!(
            "<td class=\"label\" title=\"{0}\">{0}</td>",
            html_escape(&label)
        );
        cells += &format!(
            "<td class=\"num\">{}</td>",
            html_escape(&fmt.number(num(row.get("visits")), 0))
        );
        if show_share {
            cells += &format!(
                "<td class=\"num muted\">{}</td>",
                html_escape(&fmt.share(num(row.get("share_pct"))))
            );
        }
        cells += &format!(
            "<td class=\"num\">{}</td>",
            delta_html(
                fmt,
                num(row.get("delta_pct")),
                row.get("delta_status").and_then(Value::as_str),
                false
            )
        );
        body += &format!("<tr>{cells}</tr>");
    }
    let mut foot = String::new();
    if numbered {
        foot += "<td class=\"num idx\"></td>";
    }
    foot += &format!("<td>{}</td>", html_escape(label(fmt.labels, "total")));
    foot += &format!(
        "<td class=\"num\">{}</td>",
        html_escape(&fmt.number(num(block.get("total")), 0))
    );
    if show_share {
        let total_share = match num(block.get("total")) {
            Some(t) if t != 0.0 => Some(100.0),
            _ => None,
        };
        foot += &format!(
            "<td class=\"num muted\">{}</td>",
            html_escape(&fmt.pct(total_share, 1))
        );
    }
    foot += &format!(
        "<td class=\"num\">{}</td>",
        delta_html(
            fmt,
            num(block.get("total_delta_pct")),
            block.get("total_delta_status").and_then(Value::as_str),
            false
        )
    );
    let css = format!(
        "data{}{}",
        if wide_label { " wide" } else { "" },
        if roomy { " roomy" } else { "" }
    );
    format!(
        "<div class=\"card table-card\"><table class=\"{css}\"><colgroup>{cols}</colgroup>\
         <thead><tr>{th}</tr></thead><tbody>{body}</tbody>\
         <tfoot><tr>{foot}</tr></tfoot></table></div>"
    )
}

fn legend(items: &[(String, String)], dashed: &std::collections::HashSet<usize>) -> String {
    let mut chips = String::new();
    for (index, (colour, label)) in items.iter().enumerate() {
        let style = if dashed.contains(&index) {
            format!("border-top:0.6mm dashed {colour};background:none;height:0")
        } else {
            format!("background:{colour}")
        };
        chips += &format!(
            "<span class=\"chip\"><i style=\"{style}\"></i>{}</span>",
            html_escape(label)
        );
    }
    format!("<div class=\"legend\">{chips}</div>")
}

// --- page renderer -----------------------------------------------------------

pub(crate) struct Renderer<'a> {
    doc: &'a Value,
    brand: &'a Brand,
    fmt: Fmt<'a>,
    scope: String,
    period: &'a Value,
    ids: String,
    site: String,
    series_colors: [String; 3],
}

impl<'a> Renderer<'a> {
    fn new(document: &'a Value, brand: &'a Brand, lang: &'a str) -> Self {
        let labels = labels::labels(lang).unwrap_or(&Value::Null);
        let scope = document
            .get("traffic")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("organic")
            .to_owned();
        let ids = document
            .get("counter_ids")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .map(|id| match id.as_str() {
                        Some(text) => text.to_owned(),
                        None => crate::py::str_value(id),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let site = document
            .get("site_label")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or(if brand.name.is_empty() {
                None
            } else {
                Some(brand.name.as_str())
            })
            .unwrap_or("")
            .to_owned();
        Self {
            doc: document,
            brand,
            fmt: Fmt { lang, labels },
            scope,
            period: document.get("period").unwrap_or(&Value::Null),
            ids,
            site,
            series_colors: [
                brand.accent.clone(),
                mix(&brand.accent, "#FFFFFF", 0.55),
                "#94A3B8".to_owned(),
            ],
        }
    }

    fn blocks(&self) -> &'a Value {
        self.doc.get("blocks").unwrap_or(&Value::Null)
    }

    fn block(&self, key: &str) -> Option<&'a Value> {
        self.blocks().get(key)
    }

    fn palette(&self, count: usize) -> Vec<String> {
        let mut colours: Vec<&str> = vec![self.brand.accent.as_str()];
        colours.extend(PALETTE.iter().copied());
        (0..count)
            .map(|i| colours[i % colours.len()].to_owned())
            .collect()
    }

    fn number_fmt(&self) -> impl Fn(f64) -> String + 'static {
        let decimal = label(self.fmt.labels, "decimal").to_owned();
        let group = label(self.fmt.labels, "group").to_owned();
        move |v| {
            comma_group(&format!("{v:.0}"))
                .replace(',', "\u{0}")
                .replace('.', &decimal)
                .replace('\u{0}', &group)
        }
    }

    /// Translate `key` in `scope`, then substitute `values` into it.
    fn title(&self, key: &str, values: &[(&str, &str)]) -> Result<String, String> {
        let raw = match self.fmt.t(key, &self.scope) {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        Ok(tpl(&raw, values))
    }

    fn page(&self, title: &str, body: &str, period: Option<String>) -> Result<String, String> {
        let eyebrow = if self.site.is_empty() {
            String::new()
        } else {
            html_escape(&self.site)
        };
        let span = match period {
            Some(text) => html_escape(&text),
            None => html_escape(&self.fmt.span(self.period)?),
        };
        Ok(format!(
            "<section class=\"page\"><header class=\"page-head\"><div class=\"head-text\">\
             {}<h2>{}</h2></div><div class=\"period\">{span}</div></header>\
             <main class=\"page-body\">{body}</main>\
             <footer class=\"page-foot\"><span>{}</span><span>{{PAGE}}</span></footer></section>",
            if eyebrow.is_empty() {
                String::new()
            } else {
                format!("<div class=\"eyebrow\">{eyebrow}</div>")
            },
            html_escape(title),
            html_escape(&tpl(
                label(self.fmt.labels, "footer_source"),
                &[("ids", &self.ids)]
            )),
        ))
    }
}

// --- public entry points -----------------------------------------------------

/// Refuse anything but a `spiderhead.metrika-traffic/1` document with its
/// required parts.
pub fn validate_document(document: &Value) -> Result<(), String> {
    if !document.is_object() {
        return Err("the traffic document must be a JSON object".to_owned());
    }
    if document.get("schema") != Some(&Value::String(SCHEMA.to_owned())) {
        return Err(format!(
            "not a {SCHEMA} document (schema: {})",
            repr(document.get("schema").unwrap_or(&Value::Null))
        ));
    }
    let period = document.get("period");
    let has_span = period
        .and_then(Value::as_object)
        .is_some_and(|p| p.contains_key("date1") && p.contains_key("date2"));
    if !has_span {
        return Err("the traffic document has no period".to_owned());
    }
    if !document.get("blocks").is_some_and(Value::is_object) {
        return Err("the traffic document has no blocks".to_owned());
    }
    Ok(())
}

/// Return the complete HTML report for `document`; nothing is fetched or
/// calculated.
pub fn render_html<'a>(
    document: &'a Value,
    brand: Option<&'a Value>,
    lang: Option<&'a str>,
) -> Result<String, String> {
    validate_document(document)?;
    let language = lang
        .filter(|l| !l.is_empty())
        .or_else(|| document.get("lang").and_then(Value::as_str))
        .unwrap_or("en");
    if labels::labels(language).is_none() {
        return Err("lang must be one of ['en', 'ru']".to_owned());
    }
    let brand = load_brand(brand)?;
    Renderer::new(document, &brand, language).render()
}
