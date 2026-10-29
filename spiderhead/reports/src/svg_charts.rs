//! Static SVG charts (bars, lines, donut) for print reports, written without a plotting library.
//!
//! Each function returns one self-contained `<svg>` string in CSS-pixel units.
//! Charts place values they are given: they compute geometry, never a statistic.

/// Average glyph advance as a share of font size, for label collision checks.
pub const CHAR_WIDTH: f64 = 0.58;

use crate::py::html_escape as escape;

/// One series of values plotted against shared labels.
#[derive(Debug, Clone)]
pub struct Series {
    /// Legend/label name.
    pub name: String,
    /// One value per label; `None` skips the slot.
    pub values: Vec<Option<f64>>,
    /// Stroke/fill colour.
    pub color: String,
    /// Dash the polyline.
    pub dashed: bool,
    /// Stroke width.
    pub width: f64,
    /// Draw value labels.
    pub labels: bool,
}

impl Series {
    /// A solid labelled series.
    pub fn new(name: &str, values: Vec<Option<f64>>, color: &str) -> Self {
        Self {
            name: name.to_owned(),
            values,
            color: color.to_owned(),
            dashed: false,
            width: 2.4,
            labels: true,
        }
    }
}

/// Estimated rendered text width in px.
pub fn text_width(text: &str, size: f64) -> f64 {
    text.chars().count() as f64 * size * CHAR_WIDTH
}

/// The smallest 1/2/2.5/5 x 10^k at or above `value` (1 for an all-zero chart).
pub fn nice_ceiling(value: f64) -> f64 {
    if value <= 0.0 {
        return 1.0;
    }
    let exponent = value.log10().floor();
    let base = 10f64.powf(exponent);
    for step in [1.0, 2.0, 2.5, 5.0, 10.0] {
        if value <= step * base {
            return step * base;
        }
    }
    10.0 * base
}

/// Axis ticks from zero; the step is at least 1 (charts plot counts).
fn ticks(maximum: f64, count: i32) -> Vec<f64> {
    let top = nice_ceiling(maximum);
    let step = nice_ceiling(top / count as f64).max(1.0);
    let mut ticks = Vec::new();
    let mut tick = 0.0;
    while tick <= top + step / 1000.0 {
        ticks.push((tick * 1e6).round() / 1e6);
        tick += step;
    }
    ticks
}

/// How many slots one label needs so neighbours do not overlap.
fn stride(slot: f64, widest: f64, gap: f64) -> usize {
    if slot <= 0.0 {
        return 1;
    }
    ((widest + gap) / slot).ceil().max(1.0) as usize
}

fn frame(width: f64, height: f64, body: &[String], title: Option<&str>) -> String {
    let label = title
        .map(|t| format!(" aria-label=\"{}\" role=\"img\"", escape(t)))
        .unwrap_or_default();
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width:.0} {height:.0}\" \
         width=\"{width:.0}\" height=\"{height:.0}\"{label}>{}</svg>",
        body.concat()
    )
}

#[allow(clippy::too_many_arguments)]
fn axes(
    x0: f64,
    y0: f64,
    plot_w: f64,
    plot_h: f64,
    ticks: &[f64],
    fmt: &dyn Fn(f64) -> String,
    font: f64,
    grid: &str,
    ink: &str,
) -> Vec<String> {
    let mut parts = Vec::new();
    let top = ticks.last().copied().filter(|t| *t != 0.0).unwrap_or(1.0);
    for &tick in ticks {
        let y = y0 + plot_h - tick / top * plot_h;
        parts.push(format!(
            "<line x1=\"{x0:.1}\" y1=\"{y:.1}\" x2=\"{:.1}\" y2=\"{y:.1}\" \
             stroke=\"{grid}\" stroke-width=\"1\"/>",
            x0 + plot_w
        ));
        parts.push(format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\" font-size=\"{font:.1}\" \
             fill=\"{ink}\">{}</text>",
            x0 - 8.0,
            y + font * 0.35,
            escape(&fmt(tick))
        ));
    }
    parts
}

fn x_labels(
    labels: &[String],
    centers: &[f64],
    y: f64,
    slot: f64,
    font: f64,
    ink: &str,
) -> Vec<String> {
    if labels.is_empty() {
        return Vec::new();
    }
    let widest = labels
        .iter()
        .map(|l| text_width(l, font))
        .fold(0.0, f64::max);
    let stride = stride(slot, widest, 6.0);
    let mut parts = Vec::new();
    let last = labels.len() - 1;
    for (index, (label, x)) in labels.iter().zip(centers.iter()).enumerate() {
        let keep =
            index % stride == 0 || (index == last && (last % stride) as f64 >= stride as f64 / 2.0);
        if !keep {
            continue;
        }
        parts.push(format!(
            "<text x=\"{x:.1}\" y=\"{y:.1}\" text-anchor=\"middle\" font-size=\"{font:.1}\" \
             fill=\"{ink}\">{}</text>",
            escape(label)
        ));
    }
    parts
}

/// Chart geometry shared by bars and lines.
pub struct ChartOpts {
    /// Canvas width.
    pub width: f64,
    /// Canvas height.
    pub height: f64,
    /// Value label formatter.
    pub fmt: Box<dyn Fn(f64) -> String>,
    /// Axis formatter (defaults to `fmt`).
    pub axis_fmt: Option<Box<dyn Fn(f64) -> String>>,
    /// Text colour.
    pub ink: String,
    /// Gridline colour.
    pub grid: String,
    /// Base font size.
    pub font: f64,
    /// Accessibility title.
    pub title: Option<String>,
}

impl ChartOpts {
    /// Default values for each option.
    pub fn new(width: f64, height: f64, fmt: impl Fn(f64) -> String + 'static) -> Self {
        Self {
            width,
            height,
            fmt: Box::new(fmt),
            axis_fmt: None,
            ink: "#334155".into(),
            grid: "#E2E8F0".into(),
            font: 11.0,
            title: None,
        }
    }
}

/// Vertical bars, grouped when several series share the labels.
pub fn bar_chart(labels: &[String], series: &[Series], opts: &ChartOpts) -> String {
    let axis_fmt: &dyn Fn(f64) -> String = opts.axis_fmt.as_deref().unwrap_or(&opts.fmt);
    let fmt = &opts.fmt;
    let font = opts.font;
    let maximum = series
        .iter()
        .flat_map(|s| s.values.iter().flatten().copied())
        .fold(0.0, f64::max);
    let ticks = ticks(maximum, 4);
    let top = ticks.last().copied().filter(|t| *t != 0.0).unwrap_or(1.0);
    let y_label_w = ticks
        .iter()
        .map(|t| text_width(&axis_fmt(*t), font))
        .fold(0.0, f64::max)
        + 14.0;
    let x0 = y_label_w;
    let y0 = font * 2.2;
    let plot_w = opts.width - x0 - 6.0;
    let plot_h = opts.height - y0 - font * 2.6;
    let count = labels.len().max(1);
    let slot = plot_w / count as f64;
    let group_w = slot * if series.len() == 1 { 0.72 } else { 0.8 };
    let bar_w = group_w / series.len().max(1) as f64;
    let mut parts = axes(
        x0, y0, plot_w, plot_h, &ticks, axis_fmt, font, &opts.grid, &opts.ink,
    );
    let centers: Vec<f64> = (0..labels.len())
        .map(|i| x0 + slot * (i as f64 + 0.5))
        .collect();
    let label_font = font * 0.92;
    for (s_index, item) in series.iter().enumerate() {
        let texts: Vec<String> = item
            .values
            .iter()
            .map(|v| v.map(fmt).unwrap_or_default())
            .collect();
        let widest = texts
            .iter()
            .map(|t| text_width(t, label_font))
            .fold(0.0, f64::max);
        let horizontal = widest + 2.0 <= bar_w * if series.len() == 1 { 1.0 } else { 1.15 }
            || (series.len() == 1 && widest + 4.0 <= slot);
        let vertical = !horizontal && bar_w >= label_font * 1.05;
        let stride = if horizontal || vertical {
            1
        } else {
            stride(slot, widest, 6.0)
        };
        let peak = item
            .values
            .iter()
            .enumerate()
            .max_by(|a, b| {
                a.1.unwrap_or(0.0)
                    .partial_cmp(&b.1.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i as i64)
            .unwrap_or(-1);
        for (index, value) in item.values.iter().enumerate() {
            let Some(value) = value else { continue };
            let h = (value / top * plot_h).max(0.0);
            let x = centers[index] - group_w / 2.0 + bar_w * s_index as f64;
            let y = y0 + plot_h - h;
            parts.push(format!(
                "<rect x=\"{:.1}\" y=\"{y:.1}\" width=\"{:.1}\" height=\"{h:.1}\" rx=\"{:.1}\" \
                 fill=\"{}\"/>",
                x + 0.6,
                (bar_w - 1.2).max(0.8),
                (2.5_f64).min(bar_w / 4.0),
                item.color
            ));
            if !item.labels || *value == 0.0 || (index % stride != 0 && index as i64 != peak) {
                continue;
            }
            let cx = x + bar_w / 2.0;
            if horizontal {
                parts.push(format!(
                    "<text x=\"{cx:.1}\" y=\"{:.1}\" text-anchor=\"middle\" \
                     font-size=\"{label_font:.1}\" font-weight=\"600\" fill=\"{}\">{}</text>",
                    y - 4.0,
                    opts.ink,
                    escape(&texts[index])
                ));
            } else if vertical {
                let tw = text_width(&texts[index], label_font);
                let inside = h >= tw + 8.0;
                let ty = if inside { y + 5.0 } else { y - 4.0 };
                let anchor = if inside { "end" } else { "start" };
                let colour = if inside { "#FFFFFF" } else { &opts.ink };
                parts.push(format!(
                    "<text transform=\"translate({:.1},{ty:.1}) rotate(-90)\" \
                     text-anchor=\"{anchor}\" font-size=\"{label_font:.1}\" font-weight=\"600\" \
                     fill=\"{colour}\">{}</text>",
                    cx + label_font * 0.35,
                    escape(&texts[index])
                ));
            } else {
                parts.push(format!(
                    "<text x=\"{cx:.1}\" y=\"{:.1}\" text-anchor=\"middle\" \
                     font-size=\"{label_font:.1}\" font-weight=\"600\" fill=\"{}\">{}</text>",
                    y - 4.0,
                    opts.ink,
                    escape(&texts[index])
                ));
            }
        }
    }
    parts.push(format!(
        "<line x1=\"{x0:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"{}\" \
         stroke-width=\"1\"/>",
        y0 + plot_h,
        x0 + plot_w,
        y0 + plot_h,
        opts.ink
    ));
    parts.extend(x_labels(
        labels,
        &centers,
        y0 + plot_h + font * 1.6,
        slot,
        font,
        &opts.ink,
    ));
    frame(opts.width, opts.height, &parts, opts.title.as_deref())
}

/// Lines over shared x positions; the first series gets an area tint and labels.
pub fn line_chart(labels: &[String], series: &[Series], opts: &ChartOpts) -> String {
    let axis_fmt: &dyn Fn(f64) -> String = opts.axis_fmt.as_deref().unwrap_or(&opts.fmt);
    let font = opts.font;
    let maximum = series
        .iter()
        .flat_map(|s| s.values.iter().flatten().copied())
        .fold(0.0, f64::max);
    let ticks = ticks(maximum, 4);
    let top = ticks.last().copied().filter(|t| *t != 0.0).unwrap_or(1.0);
    let y_label_w = ticks
        .iter()
        .map(|t| text_width(&axis_fmt(*t), font))
        .fold(0.0, f64::max)
        + 14.0;
    let x0 = y_label_w;
    let y0 = font * 2.2;
    let plot_w = opts.width - x0 - 14.0;
    let plot_h = opts.height - y0 - font * 2.6;
    let count = labels.len().max(1);
    let step = plot_w / (count as f64 - 1.0).max(1.0);
    let xs: Vec<f64> = (0..labels.len())
        .map(|i| {
            if count > 1 {
                x0 + step * i as f64
            } else {
                x0 + plot_w / 2.0
            }
        })
        .collect();
    let mut parts = axes(
        x0, y0, plot_w, plot_h, &ticks, axis_fmt, font, &opts.grid, &opts.ink,
    );

    let point = |index: usize, value: f64| -> (f64, f64) {
        (xs[index], y0 + plot_h - value / top * plot_h)
    };

    for s_index in (0..series.len()).rev() {
        let item = &series[s_index];
        let mut runs: Vec<Vec<(f64, f64)>> = vec![Vec::new()];
        for (index, value) in item.values.iter().take(xs.len()).enumerate() {
            match value {
                None => {
                    if !runs.last().is_some_and(|r| r.is_empty()) {
                        runs.push(Vec::new());
                    }
                }
                Some(v) => {
                    if let Some(r) = runs.last_mut() {
                        r.push(point(index, *v));
                    }
                }
            };
        }
        for run in &runs {
            if run.is_empty() {
                continue;
            }
            let path: String = run
                .iter()
                .map(|(x, y)| format!("{x:.1},{y:.1}"))
                .collect::<Vec<_>>()
                .join(" ");
            if s_index == 0 && run.len() > 1 {
                let base = y0 + plot_h;
                parts.push(format!(
                    "<polygon points=\"{:.1},{base:.1} {path} {:.1},{base:.1}\" fill=\"{}\" \
                     fill-opacity=\"0.08\"/>",
                    run[0].0,
                    run[run.len() - 1].0,
                    item.color
                ));
            }
            let dash = if item.dashed {
                " stroke-dasharray=\"6 4\""
            } else {
                ""
            };
            parts.push(format!(
                "<polyline points=\"{path}\" fill=\"none\" stroke=\"{}\" \
                 stroke-width=\"{:.1}\" stroke-linejoin=\"round\" stroke-linecap=\"round\"{dash}/>",
                item.color, item.width
            ));
            if run.len() == 1 {
                let (x, y) = run[0];
                parts.push(format!(
                    "<circle cx=\"{x:.1}\" cy=\"{y:.1}\" r=\"3\" fill=\"{}\"/>",
                    item.color
                ));
            }
        }
    }

    if let Some(first) = series.first().filter(|s| s.labels) {
        parts.extend(line_labels(
            first,
            &xs,
            &point,
            &opts.fmt,
            font,
            &opts.ink,
            x0 + plot_w,
        ));
    }
    parts.push(format!(
        "<line x1=\"{x0:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"{}\" \
         stroke-width=\"1\"/>",
        y0 + plot_h,
        x0 + plot_w,
        y0 + plot_h,
        opts.ink
    ));
    parts.extend(x_labels(
        labels,
        &xs,
        y0 + plot_h + font * 1.6,
        step,
        font,
        &opts.ink,
    ));
    frame(opts.width, opts.height, &parts, opts.title.as_deref())
}

/// Label the peak, the trough and the last point without overlapping text.
fn line_labels(
    item: &Series,
    xs: &[f64],
    point: &dyn Fn(usize, f64) -> (f64, f64),
    fmt: &dyn Fn(f64) -> String,
    font: f64,
    ink: &str,
    x_max: f64,
) -> Vec<String> {
    let indexed: Vec<(usize, f64)> = item
        .values
        .iter()
        .take(xs.len())
        .enumerate()
        .filter_map(|(i, v)| v.map(|v| (i, v)))
        .collect();
    if indexed.is_empty() {
        return Vec::new();
    }
    let peak = *indexed
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap();
    if peak.1 == 0.0 {
        return Vec::new();
    }
    let trough = *indexed
        .iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap();
    let last = *indexed.last().unwrap();
    let mut chosen: Vec<(usize, f64, &str)> = vec![(peak.0, peak.1, "above")];
    for (candidate, side) in [(last, "above"), (trough, "below")] {
        if !chosen.iter().any(|(i, _, _)| *i == candidate.0) {
            chosen.push((candidate.0, candidate.1, side));
        }
    }
    let size = font * 0.95;
    let mut placed: Vec<(f64, f64, f64, f64)> = Vec::new();
    let mut parts = Vec::new();
    for (index, value, side) in chosen {
        let (x, y) = point(index, value);
        let text = fmt(value);
        let w = text_width(&text, size);
        let left = (x - w / 2.0).max(xs[0] - 4.0).min(x_max - w);
        let ty = if side == "above" {
            y - 8.0
        } else {
            y + size + 6.0
        };
        let bx = (left - 2.0, ty - size, left + w + 2.0, ty + 3.0);
        if placed
            .iter()
            .any(|b| !(bx.2 < b.0 || bx.0 > b.2 || bx.3 < b.1 || bx.1 > b.3))
        {
            continue;
        }
        placed.push(bx);
        parts.push(format!(
            "<circle cx=\"{x:.1}\" cy=\"{y:.1}\" r=\"3.2\" fill=\"{}\"/>",
            item.color
        ));
        parts.push(format!(
            "<text x=\"{left:.1}\" y=\"{ty:.1}\" font-size=\"{size:.1}\" font-weight=\"600\" \
             fill=\"{ink}\">{}</text>",
            escape(&text)
        ));
    }
    parts
}

/// A ring of segments; labels only on segments wide enough to hold them.
#[allow(clippy::too_many_arguments)]
pub fn donut_chart(
    values: &[f64],
    colors: &[String],
    labels: &[String],
    size: f64,
    center_value: Option<&str>,
    center_label: Option<&str>,
    ink: &str,
    font: f64,
    min_label_share: f64,
    title: Option<&str>,
) -> String {
    let total: f64 = values.iter().filter(|v| **v > 0.0).sum();
    let cx = size / 2.0;
    let cy = cx;
    let outer = size / 2.0 - 2.0;
    let inner = outer * 0.58;
    let mut parts: Vec<String> = Vec::new();
    if total <= 0.0 {
        parts.push(format!(
            "<circle cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"{:.1}\" fill=\"none\" stroke=\"#E2E8F0\" \
             stroke-width=\"{:.1}\"/>",
            (outer + inner) / 2.0,
            outer - inner
        ));
        return frame(size, size, &parts, title);
    }
    let mut angle = -std::f64::consts::PI / 2.0;
    let shares: Vec<f64> = values.iter().map(|v| v.max(0.0) / total).collect();
    for ((share, color), label) in shares.iter().zip(colors.iter()).zip(labels.iter()) {
        let share = *share;
        if share <= 0.0 {
            continue;
        }
        let sweep = share * 2.0 * std::f64::consts::PI;
        if share >= 0.9999 {
            parts.push(format!(
                "<circle cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"{:.1}\" fill=\"none\" stroke=\"{color}\" \
                 stroke-width=\"{:.1}\"/>",
                (outer + inner) / 2.0,
                outer - inner
            ));
        } else {
            let end = angle + sweep;
            let large = if sweep > std::f64::consts::PI { 1 } else { 0 };
            let p = [
                (cx + outer * angle.cos(), cy + outer * angle.sin()),
                (cx + outer * end.cos(), cy + outer * end.sin()),
                (cx + inner * end.cos(), cy + inner * end.sin()),
                (cx + inner * angle.cos(), cy + inner * angle.sin()),
            ];
            parts.push(format!(
                "<path d=\"M{:.2},{:.2} A{outer:.2},{outer:.2} 0 {large} 1 {:.2},{:.2} \
                 L{:.2},{:.2} A{inner:.2},{inner:.2} 0 {large} 0 {:.2},{:.2} Z\" fill=\"{color}\" \
                 stroke=\"#FFFFFF\" stroke-width=\"1.5\"/>",
                p[0].0, p[0].1, p[1].0, p[1].1, p[2].0, p[2].1, p[3].0, p[3].1
            ));
        }
        if share >= min_label_share && !label.is_empty() {
            let mid = angle + sweep / 2.0;
            let radius = (outer + inner) / 2.0;
            let tx = cx + radius * mid.cos();
            let ty = cy + radius * mid.sin();
            parts.push(format!(
                "<text x=\"{tx:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"{font:.1}\" \
                 font-weight=\"700\" fill=\"#FFFFFF\">{}</text>",
                ty + font * 0.35,
                escape(label)
            ));
        }
        angle += sweep;
    }
    if let Some(center_value) = center_value.filter(|s| !s.is_empty()) {
        parts.push(format!(
            "<text x=\"{cx:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"{:.1}\" \
             font-weight=\"700\" fill=\"{ink}\">{}</text>",
            cy + font * 0.2,
            font * 1.9,
            escape(center_value)
        ));
    }
    if let Some(center_label) = center_label.filter(|s| !s.is_empty()) {
        parts.push(format!(
            "<text x=\"{cx:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"{:.1}\" \
             fill=\"{ink}\" fill-opacity=\"0.7\">{}</text>",
            cy + font * 1.6,
            font * 0.9,
            escape(center_label)
        ));
    }
    frame(size, size, &parts, title)
}

/// Format a count as a plain integer string.
pub fn int_fmt(value: f64) -> String {
    format!("{}", value as i64)
}
