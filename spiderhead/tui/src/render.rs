//! Faithful subset of Rich's render pipeline (`Segment`, `render_lines`,
//! `Padding`, `Panel`, `Group`, measurement) in one place.
//!
//! The shell builds symbolic renderables; this module turns them into lines
//! of styled cells exactly like `console.render_lines`, so bounds checks and
//! plain-text exports match the expected frames byte for byte. Segment styles
//! keep Rich's normalized names; the fixture layer compares text content.

use crate::text::{cell_len, set_cell_size, Overflow, Text};

/// How a column's text aligns inside its cell (rich `JustifyMethod`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Justify {
    /// Rich default — no extra padding or reshaping.
    Default,
    /// Left-aligned, padded on the right.
    Left,
    /// Centered.
    Center,
    /// Right-aligned.
    Right,
    /// Spaces expanded to fill the width (not used by the shell).
    Full,
}

/// One styled fragment of a line (rich `Segment`; styles kept as names).
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// Literal text (cell content, never contains `\n` after cropping).
    pub text: String,
    /// Combined style name for the fragment.
    pub style: String,
}

impl Segment {
    fn cell_length(&self) -> usize {
        cell_len(&self.text)
    }
}

/// A rendered row: segments whose cell lengths sum to at most the width.
pub type Line = Vec<Segment>;

/// Render-time options (rich `ConsoleOptions` subset).
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Maximum line width in cells.
    pub max_width: usize,
    /// Maximum height in lines (`None` = unbounded).
    pub height: Option<usize>,
    /// Justify method inherited by `Text` (column justify in tables).
    pub justify: Option<Justify>,
    /// Overflow method inherited by `Text`.
    pub overflow: Option<Overflow>,
    /// Force no-wrap for children.
    pub no_wrap: bool,
}

impl Options {
    /// `options.update(width=...)`.
    pub fn update_width(&self, width: usize) -> Self {
        Options {
            max_width: width,
            ..*self
        }
    }
}

/// Symbolic renderable tree mirroring the Rich objects the shell builds.
#[derive(Debug, Clone)]
pub enum Renderable {
    /// A `rich.text.Text`.
    Text(Text),
    /// `rich.console.Group` — children rendered in order.
    Group(Vec<Renderable>),
    /// `rich.padding.Padding` (top, right, bottom, left).
    Padding(Box<Renderable>, (usize, usize, usize, usize)),
    /// `rich.panel.Panel`.
    Panel(Panel),
    /// `rich.layout.Layout`.
    Layout(Box<crate::rlayout::Layout>),
    /// `rich.table.Table` configured as a grid (no borders/header).
    Grid(crate::rlayout::Grid),
}

/// `rich.panel.Panel` fields the shell uses.
#[derive(Debug, Clone)]
pub struct Panel {
    /// Child renderable.
    pub renderable: Box<Renderable>,
    /// Title text (already `from_markup` like Rich does for `str` titles).
    pub title: Option<String>,
    /// `title_align` — "left"|"center"|"right".
    pub title_align: String,
    /// Border style name (`"none"` still draws the glyph, unstyled).
    pub border_style: String,
    /// Padding (top, right, bottom, left).
    pub padding: (usize, usize, usize, usize),
    /// Explicit height constraint (`Panel(height=)`).
    pub height: Option<usize>,
    /// Whether the panel fills available width (Rich default `True`).
    pub expand: bool,
}

/// `console.render_lines(renderable, options, pad=True)` — lines padded to
/// `max_width` cells and `height` rows.
pub fn render_lines(renderable: &Renderable, options: &Options) -> Vec<Line> {
    let segments = render(renderable, options);
    let mut lines: Vec<Line> =
        split_and_crop_lines(segments.into_iter(), options.max_width, true).collect();
    if let Some(h) = options.height {
        lines.truncate(h);
        while lines.len() < h {
            lines.push(vec![Segment {
                text: " ".repeat(options.max_width),
                style: String::new(),
            }]);
        }
    }
    lines
}

/// `console.render` — flatten a renderable into a segment stream.
pub fn render(renderable: &Renderable, options: &Options) -> Vec<Segment> {
    let mut out = Vec::new();
    render_into(renderable, options, &mut out);
    out
}

fn render_into(renderable: &Renderable, options: &Options, out: &mut Vec<Segment>) {
    match renderable {
        Renderable::Text(text) => {
            let justify = text_justify(text)
                .or(options.justify)
                .unwrap_or(Justify::Default);
            let overflow = text.overflow.or(options.overflow).unwrap_or(Overflow::Fold);
            let mut lines = text.wrap(
                options.max_width,
                Some(overflow),
                if options.no_wrap { Some(true) } else { None },
            );
            justify_lines(&mut lines, options.max_width, justify, overflow);
            let joined = join_texts(&Text::new("\n", ""), &lines);
            out.extend(render_text_segments(&joined, &text.end));
        }
        Renderable::Group(children) => {
            for child in children {
                render_into(child, options, out);
            }
        }
        Renderable::Padding(child, (top, right, bottom, left)) => {
            let width = options.max_width;
            let inner = Options {
                max_width: width.saturating_sub(left + right),
                height: options.height.map(|h| h.saturating_sub(top + bottom)),
                ..*options
            };
            let lines = render_lines(child, &inner);
            let pad_left = " ".repeat(*left);
            let pad_right = " ".repeat(*right);
            for _ in 0..*top {
                out.push(Segment {
                    text: format!("{}\n", " ".repeat(width)),
                    style: String::new(),
                });
            }
            for line in lines {
                if *left > 0 {
                    out.push(Segment {
                        text: pad_left.clone(),
                        style: String::new(),
                    });
                }
                out.extend(line);
                let tail = if *right > 0 {
                    format!("{}\n", pad_right)
                } else {
                    "\n".into()
                };
                out.push(Segment {
                    text: tail,
                    style: String::new(),
                });
            }
            for _ in 0..*bottom {
                out.push(Segment {
                    text: format!("{}\n", " ".repeat(width)),
                    style: String::new(),
                });
            }
        }
        Renderable::Panel(panel) => render_panel(panel, options, out),
        Renderable::Layout(layout) => {
            let mut opts = *options;
            if let Some(size) = layout.force_height {
                opts.height = Some(size);
            }
            crate::rlayout::render_layout(layout, &opts, out);
        }
        Renderable::Grid(grid) => crate::rlayout::render_grid(grid, options, out),
    }
}

/// Texts carry no `justify` field in this port (the shell never sets one).
fn text_justify(_text: &Text) -> Option<Justify> {
    None
}

/// `rich.panel.Panel.__rich_console__` for the box the shell draws (ROUNDED,
/// no subtitles except where supplied).
fn render_panel(panel: &Panel, options: &Options, out: &mut Vec<Segment>) {
    const TOP_LEFT: char = '╭';
    const TOP: char = '─';
    const TOP_RIGHT: char = '╮';
    const MID_LEFT: char = '│';
    const MID_RIGHT: char = '│';
    const BOTTOM_LEFT: char = '╰';
    const BOTTOM: char = '─';
    const BOTTOM_RIGHT: char = '╯';

    let (pad_top, pad_right, pad_bottom, pad_left) = panel.padding;
    let border = panel.border_style.clone();
    let width = options.max_width;
    if width == 0 {
        return;
    }
    let child: Renderable = if (pad_top | pad_right | pad_bottom | pad_left) > 0 {
        Renderable::Padding(
            panel.renderable.clone(),
            (pad_top, pad_right, pad_bottom, pad_left),
        )
    } else {
        (*panel.renderable).clone()
    };
    let mut child_width = width - 2; // expand
    let mut child_height = panel.height.or(options.height);
    if let Some(h) = child_height {
        child_height = Some(h.saturating_sub(2));
    }
    // Rich pads the title by one cell on each side before laying it on the border.
    let title_text = panel
        .title
        .as_ref()
        .map(|t| Text::from_markup(&format!(" {t} "), ""));
    if let Some(t) = &title_text {
        child_width = (options.max_width.saturating_sub(2)).min(child_width.max(t.cell_len() + 2));
    }
    let width = child_width + 2;
    let lines = render_lines(
        &child,
        &Options {
            max_width: child_width,
            height: child_height,
            ..*options
        },
    );
    let push = |out: &mut Vec<Segment>, text: String, style: &str| {
        out.push(Segment {
            text,
            style: style.to_string(),
        });
    };
    match title_text.filter(|_| width > 4) {
        None => {
            push(
                out,
                format!("{TOP_LEFT}{}{TOP_RIGHT}", TOP.to_string().repeat(width - 2)),
                &border,
            );
        }
        Some(title_text) => {
            let title = align_border_text(
                &title_text,
                width.saturating_sub(4),
                &panel.title_align,
                TOP,
                &border,
            );
            push(out, format!("{TOP_LEFT}{TOP}"), &border);
            out.extend(title);
            push(out, format!("{TOP}{TOP_RIGHT}"), &border);
        }
    }
    push(out, "\n".into(), "");
    for line in lines {
        push(out, MID_LEFT.to_string(), &border);
        out.extend(line);
        push(out, MID_RIGHT.to_string(), &border);
        push(out, "\n".into(), "");
    }
    push(
        out,
        format!(
            "{BOTTOM_LEFT}{}{BOTTOM_RIGHT}",
            BOTTOM.to_string().repeat(width - 2)
        ),
        &border,
    );
    push(out, "\n".into(), "");
}

/// Panel's `align_text` — fit the title between two border runs.
fn align_border_text(
    text: &Text,
    width: usize,
    align: &str,
    character: char,
    style: &str,
) -> Vec<Segment> {
    let mut text = text.clone();
    text.truncate(width, None, false);
    let excess = width.saturating_sub(text.cell_len());
    let fill = |n: usize| Segment {
        text: character.to_string().repeat(n),
        style: style.to_string(),
    };
    let mut title_segs = render_text_segments(&text, "");
    let mut out = Vec::new();
    match align {
        "left" => {
            out.append(&mut title_segs);
            if excess > 0 {
                out.push(fill(excess));
            }
        }
        "right" => {
            if excess > 0 {
                out.push(fill(excess));
            }
            out.append(&mut title_segs);
        }
        _ => {
            let left = excess / 2;
            out.push(fill(left));
            out.append(&mut title_segs);
            out.push(fill(excess - left));
        }
    }
    out
}

/// `Text.join(separator, lines)` — rich `Text("\n").join(lines)` equivalent:
/// texts concatenated with the separator, spans re-offset.
pub fn join_texts(separator: &Text, items: &[Text]) -> Text {
    let mut result = Text {
        plain: String::new(),
        style: separator.style.clone(),
        ..Default::default()
    };
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            let offset = crate::text::chars_of(&result.plain);
            result.plain.push_str(&separator.plain);
            for span in &separator.spans {
                result.spans.push(crate::text::Span {
                    start: span.start + offset,
                    end: span.end + offset,
                    style: span.style.clone(),
                });
            }
        }
        let offset = crate::text::chars_of(&result.plain);
        result.plain.push_str(&item.plain);
        for span in &item.spans {
            result.spans.push(crate::text::Span {
                start: span.start + offset,
                end: span.end + offset,
                style: span.style.clone(),
            });
        }
    }
    result
}

/// `Text.render` — split into segments at span boundaries; the base `style`
/// participates as stack id 0 like Rich.
pub fn render_text_segments(text: &Text, end: &str) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let plain = &text.plain;
    let char_pos: Vec<usize> = plain
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(plain.len()))
        .collect();
    if text.spans.is_empty() && text.style.is_empty() {
        if !plain.is_empty() {
            out.push(Segment {
                text: plain.clone(),
                style: String::new(),
            });
        }
        if !end.is_empty() {
            out.push(Segment {
                text: end.to_string(),
                style: String::new(),
            });
        }
        return out;
    }
    // (offset, leaving, style_id); style_id 0 = the Text's base style.
    let mut marks: Vec<(usize, bool, usize)> = vec![(0, false, 0)];
    for (i, span) in text.spans.iter().enumerate() {
        marks.push((span.start, false, i + 1));
        marks.push((span.end, true, i + 1));
    }
    marks.push((plain.chars().count(), true, 0));
    marks.sort_by_key(|(offset, leaving, _)| (*offset, *leaving));
    let mut stack: Vec<usize> = Vec::new();
    for window in marks.windows(2) {
        let (offset, leaving, style_id) = window[0];
        let (next_offset, _, _) = window[1];
        if leaving {
            stack.retain(|id| *id != style_id);
        } else {
            stack.push(style_id);
        }
        if next_offset > offset {
            let style = combine_styles(&stack, text);
            out.push(Segment {
                text: plain[char_pos[offset]..char_pos[next_offset]].to_string(),
                style,
            });
        }
    }
    if !end.is_empty() {
        out.push(Segment {
            text: end.to_string(),
            style: String::new(),
        });
    }
    out
}

/// Combined style name for the active stack (Rich `Style.combine` — later
/// ids override; here we keep the normalized names joined for fixtures).
fn combine_styles(stack: &[usize], text: &Text) -> String {
    let mut styles: Vec<&str> = Vec::new();
    for id in stack
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
    {
        let name = if id == 0 {
            text.style.as_str()
        } else {
            text.spans
                .get(id - 1)
                .map(|s| s.style.as_str())
                .unwrap_or("")
        };
        if !name.is_empty() {
            styles.push(name);
        }
    }
    styles.join(" ")
}

/// `Segment.split_and_crop_lines` (include_new_lines=False, pad as given).
pub fn split_and_crop_lines(
    segments: impl Iterator<Item = Segment>,
    length: usize,
    pad: bool,
) -> impl Iterator<Item = Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut line: Line = Vec::new();
    for segment in segments {
        if segment.text.contains('\n') {
            let mut text = segment.text.as_str();
            while !text.is_empty() {
                match text.find('\n') {
                    Some(pos) => {
                        let (chunk, rest) = text.split_at(pos);
                        if !chunk.is_empty() {
                            line.push(Segment {
                                text: chunk.to_string(),
                                style: segment.style.clone(),
                            });
                        }
                        lines.push(adjust_line_length(std::mem::take(&mut line), length, pad));
                        text = &rest[1..];
                    }
                    None => {
                        line.push(Segment {
                            text: text.to_string(),
                            style: segment.style.clone(),
                        });
                        break;
                    }
                }
            }
        } else {
            line.push(segment);
        }
    }
    if !line.is_empty() {
        lines.push(adjust_line_length(line, length, pad));
    }
    lines.into_iter()
}

/// `Segment.adjust_line_length` — crop a segment list to `length` cells or
/// pad with a trailing space segment.
pub fn adjust_line_length(line: Line, length: usize, pad: bool) -> Line {
    let line_length: usize = line.iter().map(Segment::cell_length).sum();
    if line_length < length {
        if pad {
            let mut new_line = line;
            new_line.push(Segment {
                text: " ".repeat(length - line_length),
                style: String::new(),
            });
            return new_line;
        }
        return line;
    }
    if line_length > length {
        let mut new_line: Line = Vec::new();
        let mut used = 0;
        for segment in line {
            let seg_len = segment.cell_length();
            if used + seg_len < length {
                used += seg_len;
                new_line.push(segment);
            } else {
                new_line.push(Segment {
                    text: set_cell_size(&segment.text, length - used),
                    style: segment.style,
                });
                break;
            }
        }
        return new_line;
    }
    line
}

/// `Segment.set_shape` — crop/pad every line to `width` and pad to `height`.
pub fn set_shape(lines: &[Line], width: usize, height: Option<usize>) -> Vec<Line> {
    let target = height.unwrap_or(lines.len());
    let mut shaped: Vec<Line> = lines
        .iter()
        .take(target)
        .map(|line| adjust_line_length(line.clone(), width, true))
        .collect();
    while shaped.len() < target {
        shaped.push(vec![Segment {
            text: " ".repeat(width),
            style: String::new(),
        }]);
    }
    shaped
}

/// `Lines.justify` for the subset of methods the tables use.
pub fn justify_lines(lines: &mut [Text], width: usize, justify: Justify, overflow: Overflow) {
    match justify {
        Justify::Default => {}
        Justify::Left => {
            for line in lines.iter_mut() {
                line.truncate(width, Some(overflow), true);
            }
        }
        Justify::Center => {
            for line in lines.iter_mut() {
                line.rstrip();
                line.truncate(width, Some(overflow), false);
                let gap = width.saturating_sub(line.cell_len());
                line.pad_left(gap / 2);
                line.pad_right(width.saturating_sub(line.cell_len()));
            }
        }
        Justify::Right => {
            for line in lines.iter_mut() {
                line.rstrip();
                line.truncate(width, Some(overflow), false);
                line.pad_left(width.saturating_sub(line.cell_len()));
            }
        }
        Justify::Full => {}
    }
}

/// `rich.measure.Measurement`.
#[derive(Debug, Clone, Copy)]
pub struct Measurement {
    /// Minimum cells the renderable needs.
    pub minimum: usize,
    /// Cells it would occupy unconstrained.
    pub maximum: usize,
}

impl Measurement {
    fn with_maximum(self, width: usize) -> Self {
        Measurement {
            minimum: self.minimum.min(width),
            maximum: self.maximum.min(width),
        }
    }

    /// `measurement.clamp(min_width, max_width)`.
    pub fn clamp(self, min_width: Option<usize>, max_width: Option<usize>) -> Self {
        let mut m = self;
        if let Some(w) = min_width {
            m = Measurement {
                minimum: m.minimum.max(w),
                maximum: m.maximum.max(w),
            };
        }
        if let Some(w) = max_width {
            m = m.with_maximum(w);
        }
        m
    }
}

/// `Measurement.get` — `__rich_measure__` for each renderable kind.
pub fn measure(renderable: &Renderable, options: &Options) -> Measurement {
    if options.max_width < 1 {
        return Measurement {
            minimum: 0,
            maximum: 0,
        };
    }
    let raw = measure_raw(renderable, options);
    let m = raw.with_maximum(options.max_width);
    Measurement {
        minimum: m.minimum.min(m.maximum),
        maximum: m.maximum.max(m.minimum),
    }
}

fn measure_raw(renderable: &Renderable, options: &Options) -> Measurement {
    match renderable {
        Renderable::Text(text) => {
            let lines: Vec<&str> = text.plain.split('\n').collect();
            let max_w = lines.iter().map(|l| cell_len(l)).max().unwrap_or(0);
            let min_w = text
                .plain
                .split_whitespace()
                .map(cell_len)
                .max()
                .unwrap_or(max_w);
            Measurement {
                minimum: min_w,
                maximum: max_w,
            }
        }
        Renderable::Group(children) => {
            // measure_renderables: max of mins, max of maxes.
            let mut minimum = 0;
            let mut maximum = 0;
            for child in children {
                let m = measure(child, options);
                minimum = minimum.max(m.minimum);
                maximum = maximum.max(m.maximum);
            }
            Measurement { minimum, maximum }
        }
        Renderable::Padding(child, (top, right, bottom, left)) => {
            let _ = (top, bottom);
            let extra = left + right;
            if options.max_width.saturating_sub(extra) < 1 {
                return Measurement {
                    minimum: options.max_width,
                    maximum: options.max_width,
                };
            }
            let m = measure(child, options);
            Measurement {
                minimum: m.minimum + extra,
                maximum: m.maximum + extra,
            }
            .with_maximum(options.max_width)
        }
        Renderable::Panel(panel) => {
            let (_, right, _, left) = panel.padding;
            let padding = left + right;
            let inner = options.update_width(options.max_width.saturating_sub(padding + 2));
            let mut maximum = measure_raw(&panel.renderable, &inner).maximum;
            if let Some(title) = &panel.title {
                let t = Text::from_markup(&format!(" {title} "), "");
                maximum = maximum.max(measure_raw(&Renderable::Text(t), &inner).maximum);
            }
            let width = maximum + padding + 2;
            Measurement {
                minimum: width,
                maximum: width,
            }
        }
        Renderable::Layout(_) | Renderable::Grid(_) => Measurement {
            minimum: 0,
            maximum: options.max_width,
        },
    }
}
