//! Charts drawn with the egui painter: bar, stacked, 100 % stacked, line and
//! sparkline, with y/x axis labels, a legend and a hover tooltip. Data comes
//! in as plain slices.

use egui::{
    pos2, vec2, Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Response, RichText,
    Sense, Shape, Stroke, Ui, Vec2,
};

use super::thousands;
use crate::theme::{ty, Palette};

/// One data series: a name, a colour and a value per x position.
pub struct Series<'a> {
    /// Legend and tooltip name.
    pub name: &'a str,
    /// Fill or line colour.
    pub color: Color32,
    /// One value per label.
    pub values: &'a [f64],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Stack,
    Percent,
    Line,
}

/// Plot margins: room for y labels on the left and x labels below.
const LEFT: f32 = 40.0;
const BOTTOM: f32 = 18.0;
const TOP: f32 = 6.0;
const RIGHT: f32 = 6.0;

/// Axis number: `850`, `12k`, `1,5M`.
fn short_num(v: f64) -> String {
    let trim = |x: f64| format!("{x:.1}").trim_end_matches(".0").replace('.', ",");
    if v >= 1e6 {
        format!("{}M", trim(v / 1e6))
    } else if v >= 1e3 {
        format!("{}k", trim(v / 1e3))
    } else {
        format!("{v:.0}")
    }
}

/// Smallest 1/2/2.5/5 × 10^k at or above `v`, for a tidy top gridline.
fn nice_max(v: f64) -> f64 {
    if v <= 0.0 {
        return 1.0;
    }
    let exp = 10f64.powf(v.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .iter()
        .map(|m| m * exp)
        .find(|c| *c >= v)
        .unwrap_or(10.0 * exp)
}

/// Legend row: a coloured dot and a name per series.
pub fn legend(ui: &mut Ui, p: &Palette, items: &[(&str, Color32)]) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (name, color) in items {
            ui.label(RichText::new("●").size(ty::OVERLINE).color(*color));
            ui.label(RichText::new(*name).size(ty::CAPTION).color(p.weak));
            ui.add_space(8.0);
        }
    });
}

fn column_total(series: &[Series<'_>], i: usize) -> f64 {
    series
        .iter()
        .map(|s| s.values.get(i).copied().unwrap_or(0.0))
        .sum()
}

fn plot(ui: &mut Ui, p: &Palette, labels: &[&str], series: &[Series<'_>], kind: Kind, size: Vec2) {
    if series.len() > 1 {
        let items: Vec<_> = series.iter().map(|s| (s.name, s.color)).collect();
        legend(ui, p, &items);
    }
    let (outer, response) = ui.allocate_exact_size(size, Sense::hover());
    let area = Rect::from_min_max(
        pos2(outer.left() + LEFT, outer.top() + TOP),
        pos2(outer.right() - RIGHT, outer.bottom() - BOTTOM),
    );
    let n = labels.len().max(1);
    let top = match kind {
        Kind::Percent => 100.0,
        Kind::Stack => nice_max((0..n).map(|i| column_total(series, i)).fold(0.0, f64::max)),
        Kind::Line => nice_max(
            series
                .iter()
                .flat_map(|s| s.values.iter().copied())
                .fold(0.0, f64::max),
        ),
    };
    let painter = ui.painter();
    let axis = FontId::new(ty::OVERLINE, FontFamily::Proportional);
    for step in 0..=2 {
        let v = top * f64::from(step) / 2.0;
        let y = area.bottom() - area.height() * (step as f32 / 2.0);
        painter.line_segment(
            [pos2(area.left(), y), pos2(area.right(), y)],
            Stroke::new(1.0_f32, p.border_soft),
        );
        let text = if kind == Kind::Percent {
            format!("{v:.0} %")
        } else {
            short_num(v)
        };
        painter.text(
            pos2(area.left() - 6.0, y),
            Align2::RIGHT_CENTER,
            text,
            axis.clone(),
            p.faint,
        );
    }
    let slot = area.width() / n as f32;
    let every = n.div_ceil(8).max(1);
    for (i, label) in labels.iter().enumerate().step_by(every) {
        painter.text(
            pos2(area.left() + slot * (i as f32 + 0.5), area.bottom() + 4.0),
            Align2::CENTER_TOP,
            label,
            axis.clone(),
            p.faint,
        );
    }
    let hover = response
        .hover_pos()
        .filter(|h| area.contains(*h))
        .map(|h| (((h.x - area.left()) / slot) as usize).min(n - 1));
    if let Some(i) = hover {
        let band = Rect::from_min_max(
            pos2(area.left() + slot * i as f32, area.top()),
            pos2(area.left() + slot * (i as f32 + 1.0), area.bottom()),
        );
        painter.rect_filled(band, 0.0, p.hover.gamma_multiply(0.7));
    }
    if kind == Kind::Line {
        for s in series {
            let pts: Vec<Pos2> = s
                .values
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    pos2(
                        area.left() + slot * (i as f32 + 0.5),
                        area.bottom() - area.height() * (*v / top) as f32,
                    )
                })
                .collect();
            painter.add(Shape::line(pts.clone(), Stroke::new(2.0_f32, s.color)));
            if let Some(pt) = hover.and_then(|i| pts.get(i)) {
                painter.circle(*pt, 3.5, p.surface, Stroke::new(2.0_f32, s.color));
            }
        }
    } else {
        let bar_w = (slot * 0.72).max(1.0);
        for i in 0..n {
            let x = area.left() + slot * (i as f32 + 0.5) - bar_w / 2.0;
            let total = column_total(series, i);
            let scale = match kind {
                Kind::Percent if total > 0.0 => 100.0 / total / top,
                _ => 1.0 / top,
            };
            let mut base = area.bottom();
            for s in series {
                let h = area.height() * (s.values.get(i).copied().unwrap_or(0.0) * scale) as f32;
                if h > 0.0 {
                    let seg = Rect::from_min_size(pos2(x, base - h), vec2(bar_w, h));
                    painter.rect_filled(seg, CornerRadius::ZERO, s.color);
                }
                base -= h;
            }
        }
    }
    if let Some(i) = hover {
        let total = column_total(series, i);
        response.on_hover_ui_at_pointer(|ui| {
            ui.label(RichText::new(labels.get(i).copied().unwrap_or("")).color(p.weak));
            for s in series {
                let v = s.values.get(i).copied().unwrap_or(0.0);
                let mut text = thousands(v as i64);
                if kind == Kind::Percent && total > 0.0 {
                    text = format!("{text} · {:.1} %", v / total * 100.0).replace('.', ",");
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").size(ty::OVERLINE).color(s.color));
                    ui.label(RichText::new(format!("{}  {text}", s.name)).size(ty::CAPTION));
                });
            }
        });
    }
}

/// Single-series bar chart.
pub fn bar_chart(ui: &mut Ui, p: &Palette, labels: &[&str], series: &Series<'_>, size: Vec2) {
    plot(
        ui,
        p,
        labels,
        std::slice::from_ref(series),
        Kind::Stack,
        size,
    );
}

/// Bars stacked bottom-up in series order.
pub fn stacked_bar_chart(
    ui: &mut Ui,
    p: &Palette,
    labels: &[&str],
    series: &[Series<'_>],
    size: Vec2,
) {
    plot(ui, p, labels, series, Kind::Stack, size);
}

/// Stacked bars normalised to 100 % per column.
pub fn percent_bar_chart(
    ui: &mut Ui,
    p: &Palette,
    labels: &[&str],
    series: &[Series<'_>],
    size: Vec2,
) {
    plot(ui, p, labels, series, Kind::Percent, size);
}

/// One polyline per series.
pub fn line_chart(ui: &mut Ui, p: &Palette, labels: &[&str], series: &[Series<'_>], size: Vec2) {
    plot(ui, p, labels, series, Kind::Line, size);
}

/// Axis-less trend line with a soft fill and a dot on the last value.
pub fn sparkline(ui: &mut Ui, values: &[f64], color: Color32, size: Vec2) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if values.len() < 2 {
        return response;
    }
    let (lo, hi) = values
        .iter()
        .fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
    let span = (hi - lo).max(f64::EPSILON);
    let inner = rect.shrink2(vec2(3.0, 4.0));
    let step = inner.width() / (values.len() - 1) as f32;
    let pts: Vec<Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            pos2(
                inner.left() + step * i as f32,
                inner.bottom() - inner.height() * ((v - lo) / span) as f32,
            )
        })
        .collect();
    let painter = ui.painter();
    // One convex quad per segment: a single polygon would mis-fill dips.
    for w in pts.windows(2) {
        let quad = vec![
            w[0],
            w[1],
            pos2(w[1].x, inner.bottom()),
            pos2(w[0].x, inner.bottom()),
        ];
        painter.add(Shape::convex_polygon(
            quad,
            crate::theme::Palette::tint(color, 36),
            Stroke::NONE,
        ));
    }
    painter.add(Shape::line(pts.clone(), Stroke::new(1.5_f32, color)));
    if let Some(last) = pts.last() {
        painter.circle_filled(*last, 2.5, color);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_numbers_are_short() {
        assert_eq!(short_num(850.0), "850");
        assert_eq!(short_num(12_500.0), "12,5k");
        assert_eq!(short_num(25_000.0), "25k");
        assert_eq!(short_num(1_500_000.0), "1,5M");
    }

    #[test]
    fn nice_max_rounds_up_to_a_round_step() {
        assert_eq!(nice_max(0.0), 1.0);
        assert_eq!(nice_max(23.0), 25.0);
        assert_eq!(nice_max(26_000.0), 50_000.0);
        assert_eq!(nice_max(100.0), 100.0);
    }
}
