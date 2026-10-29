//! A faithful subset of `rich.text.Text` + `rich.cells` + `rich._wrap`.
//!
//! The TUI's data structures stay symbolic: styles are stored as the same
//! strings the reference produces, so serialized views compare byte-equal.
//! Emoji markup replacement (`:code:`) is not implemented — the shell never
//! feeds markup through `from_markup` with emoji codes (inputs are escaped).

/// How text that exceeds its width is shortened (rich `OverflowMethod`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overflow {
    /// Word-wrap onto following lines (rich default).
    #[default]
    Fold,
    /// Crop and append `…`.
    Ellipsis,
    /// Hard crop at the cell boundary.
    Crop,
    /// Do not shorten at all.
    Ignore,
}

impl Overflow {
    /// Parse a rich overflow method name.
    pub fn named(name: &str) -> Self {
        match name {
            "ellipsis" => Self::Ellipsis,
            "crop" => Self::Crop,
            "ignore" => Self::Ignore,
            _ => Self::Fold,
        }
    }
}

/// Styled region of a `Text` (rich `Span`; offsets are character indices).
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    /// Start character offset (inclusive).
    pub start: usize,
    /// End character offset (exclusive).
    pub end: usize,
    /// Style name as produced by normalized style naming.
    pub style: String,
}

/// Character count.
fn chars(text: &str) -> usize {
    text.chars().count()
}

/// Public `len()` in characters for sibling modules.
pub fn chars_of(text: &str) -> usize {
    text.chars().count()
}

/// Terminal cells used by a string (rich `cell_len`, single-cell fast path
/// included in `unicode-width`'s implementation).
pub fn cell_len(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

/// `rich.cells.set_cell_size`: crop or pad so the string occupies `total` cells.
pub fn set_cell_size(text: &str, total: usize) -> String {
    let size = cell_len(text);
    if size == total {
        return text.to_string();
    }
    if size < total {
        let mut out = String::from(text);
        out.extend(std::iter::repeat_n(' ', total - size));
        return out;
    }
    let mut width = 0;
    let mut end = 0;
    for (byte, c) in text.char_indices() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if width + w > total {
            break;
        }
        width += w;
        end = byte + c.len_utf8();
    }
    text[..end].to_string()
}

/// `rich.cells.chop_cells`: fold a string into lines of at most `width` cells.
pub fn chop_cells(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_width = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if line_width + w > width {
            lines.push(std::mem::take(&mut line));
            line_width = 0;
        }
        line.push(c);
        line_width += w;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// `rich.markup.escape`: prefix tag-looking `[` sequences with a backslash.
pub fn escape(markup: &str) -> String {
    let mut out = String::with_capacity(markup.len());
    let bytes: Vec<char> = markup.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '[' && tag_at(&bytes, i).is_some() {
            out.push('\\');
        }
        out.push(bytes[i]);
        i += 1;
    }
    if out.ends_with('\\') && !out.ends_with("\\\\") {
        out.push('\\');
    }
    out
}

/// If a rich tag opens at `bytes[i]` (`[` then a tag first char, then `]` with
/// no `[` inside), return the index just past its closing `]`.
fn tag_at(bytes: &[char], i: usize) -> Option<usize> {
    if bytes.get(i) != Some(&'[') {
        return None;
    }
    let first = *bytes.get(i + 1)?;
    if !(first.is_ascii_lowercase() || matches!(first, '#' | '/' | '@')) {
        return None;
    }
    let mut j = i + 2;
    while j < bytes.len() {
        match bytes[j] {
            ']' => return Some(j + 1),
            '[' => return None,
            _ => j += 1,
        }
    }
    None
}

/// `Style.normalize`: canonical form, or `style.strip().lower()` on parse errors.
pub fn style_normalize(style: &str) -> String {
    match style_parse(style) {
        Some(canonical) => canonical,
        None => style.trim().to_lowercase(),
    }
}

/// Rich `Style.parse` + `__str__` for the attribute/color subset used here.
fn style_parse(definition: &str) -> Option<String> {
    const ATTRS: [(&str, &str); 22] = [
        ("dim", "dim"),
        ("d", "dim"),
        ("bold", "bold"),
        ("b", "bold"),
        ("italic", "italic"),
        ("i", "italic"),
        ("underline", "underline"),
        ("u", "underline"),
        ("blink", "blink"),
        ("blink2", "blink2"),
        ("reverse", "reverse"),
        ("r", "reverse"),
        ("conceal", "conceal"),
        ("c", "conceal"),
        ("strike", "strike"),
        ("s", "strike"),
        ("underline2", "underline2"),
        ("uu", "underline2"),
        ("frame", "frame"),
        ("encircle", "encircle"),
        ("overline", "overline"),
        ("o", "overline"),
    ];
    /// Canonical attribute order in `Style.__str__`.
    const ORDER: [&str; 13] = [
        "bold",
        "dim",
        "italic",
        "underline",
        "blink",
        "blink2",
        "reverse",
        "conceal",
        "strike",
        "underline2",
        "frame",
        "encircle",
        "overline",
    ];
    let trimmed = definition.trim();
    if trimmed.eq_ignore_ascii_case("none") || trimmed.is_empty() {
        return Some(String::new());
    }
    let mut set: Vec<(String, bool)> = Vec::new();
    let mut color: Option<String> = None;
    let mut bgcolor: Option<String> = None;
    let mut link: Option<String> = None;
    let mut words = trimmed.split_whitespace().peekable();
    while let Some(original) = words.next() {
        let word = original.to_lowercase();
        if word == "on" {
            let next = words.next()?.to_lowercase();
            color_name(&next)?;
            bgcolor = Some(next);
        } else if word == "not" {
            let next = words.next()?.to_lowercase();
            let attr = ATTRS.iter().find(|(a, _)| *a == next)?.1.to_string();
            set.retain(|(a, _)| *a != attr);
            set.push((attr, false));
        } else if word == "link" {
            link = Some(words.next()?.to_string());
        } else if let Some((_, attr)) = ATTRS.iter().find(|(a, _)| *a == word) {
            let attr = attr.to_string();
            if !set.iter().any(|(a, _)| *a == attr) {
                set.push((attr, true));
            }
        } else {
            color_name(&word)?;
            color = Some(word);
        }
    }
    let mut out: Vec<String> = Vec::new();
    for name in ORDER {
        if let Some((_, on)) = set.iter().find(|(a, _)| a == name) {
            out.push(if *on {
                name.into()
            } else {
                format!("not {name}")
            });
        }
    }
    if let Some(c) = color {
        out.push(c);
    }
    if let Some(bg) = bgcolor {
        out.push(format!("on {bg}"));
    }
    if let Some(l) = link {
        out.push(format!("link {l}"));
    }
    Some(out.join(" "))
}

/// `Color.parse` subset: ANSI names, `#rrggbb`, `rgb(r,g,b)`, `color(N)`.
fn color_name(word: &str) -> Option<String> {
    if word.is_empty() {
        return None;
    }
    if let Some(hex) = word.strip_prefix('#') {
        let ok = hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit());
        return ok.then(|| format!("#{}", hex.to_lowercase()));
    }
    if let Some(inner) = word.strip_prefix("rgb(").and_then(|w| w.strip_suffix(')')) {
        let ok = inner.split(',').all(|p| p.trim().parse::<u8>().is_ok());
        return ok.then(|| word.to_string());
    }
    if let Some(inner) = word
        .strip_prefix("color(")
        .and_then(|w| w.strip_suffix(')'))
    {
        return inner.parse::<u8>().ok().map(|_| word.to_string());
    }
    if word == "default" || ANSI_NAMES.contains(&word) {
        return Some(word.to_string());
    }
    None
}

/// `rich.color.ANSI_COLOR_NAMES`.
static ANSI_NAMES: &[&str] = &[
    "aquamarine1",
    "aquamarine3",
    "black",
    "blue",
    "blue1",
    "blue3",
    "blue_violet",
    "bright_black",
    "bright_blue",
    "bright_cyan",
    "bright_green",
    "bright_magenta",
    "bright_red",
    "bright_white",
    "bright_yellow",
    "cadet_blue",
    "chartreuse1",
    "chartreuse2",
    "chartreuse3",
    "chartreuse4",
    "cornflower_blue",
    "cornsilk1",
    "cyan",
    "cyan1",
    "cyan2",
    "cyan3",
    "dark_blue",
    "dark_cyan",
    "dark_goldenrod",
    "dark_green",
    "dark_khaki",
    "dark_magenta",
    "dark_olive_green1",
    "dark_olive_green2",
    "dark_olive_green3",
    "dark_orange",
    "dark_orange3",
    "dark_red",
    "dark_sea_green",
    "dark_sea_green1",
    "dark_sea_green2",
    "dark_sea_green3",
    "dark_sea_green4",
    "dark_slate_gray1",
    "dark_slate_gray2",
    "dark_slate_gray3",
    "dark_turquoise",
    "dark_violet",
    "deep_pink1",
    "deep_pink2",
    "deep_pink3",
    "deep_pink4",
    "deep_sky_blue1",
    "deep_sky_blue2",
    "deep_sky_blue3",
    "deep_sky_blue4",
    "dodger_blue1",
    "dodger_blue2",
    "dodger_blue3",
    "gold1",
    "gold3",
    "gray0",
    "gray100",
    "gray11",
    "gray15",
    "gray19",
    "gray23",
    "gray27",
    "gray3",
    "gray30",
    "gray35",
    "gray37",
    "gray39",
    "gray42",
    "gray46",
    "gray50",
    "gray53",
    "gray54",
    "gray58",
    "gray62",
    "gray63",
    "gray66",
    "gray69",
    "gray7",
    "gray70",
    "gray74",
    "gray78",
    "gray82",
    "gray84",
    "gray85",
    "gray89",
    "gray93",
    "green",
    "green1",
    "green3",
    "green4",
    "green_yellow",
    "grey0",
    "grey100",
    "grey11",
    "grey15",
    "grey19",
    "grey23",
    "grey27",
    "grey3",
    "grey30",
    "grey35",
    "grey37",
    "grey39",
    "grey42",
    "grey46",
    "grey50",
    "grey53",
    "grey54",
    "grey58",
    "grey62",
    "grey63",
    "grey66",
    "grey69",
    "grey7",
    "grey70",
    "grey74",
    "grey78",
    "grey82",
    "grey84",
    "grey85",
    "grey89",
    "grey93",
    "honeydew2",
    "hot_pink",
    "hot_pink2",
    "hot_pink3",
    "indian_red",
    "indian_red1",
    "khaki1",
    "khaki3",
    "light_coral",
    "light_cyan1",
    "light_cyan3",
    "light_goldenrod1",
    "light_goldenrod2",
    "light_goldenrod3",
    "light_green",
    "light_pink1",
    "light_pink3",
    "light_pink4",
    "light_salmon1",
    "light_salmon3",
    "light_sea_green",
    "light_sky_blue1",
    "light_sky_blue3",
    "light_slate_blue",
    "light_slate_gray",
    "light_slate_grey",
    "light_steel_blue",
    "light_steel_blue1",
    "light_steel_blue3",
    "light_yellow3",
    "magenta",
    "magenta1",
    "magenta2",
    "magenta3",
    "medium_orchid",
    "medium_orchid1",
    "medium_orchid3",
    "medium_purple",
    "medium_purple1",
    "medium_purple2",
    "medium_purple3",
    "medium_purple4",
    "medium_spring_green",
    "medium_turquoise",
    "medium_violet_red",
    "misty_rose1",
    "misty_rose3",
    "navajo_white1",
    "navajo_white3",
    "navy_blue",
    "orange1",
    "orange3",
    "orange4",
    "orange_red1",
    "orchid",
    "orchid1",
    "orchid2",
    "pale_green1",
    "pale_green3",
    "pale_turquoise1",
    "pale_turquoise4",
    "pale_violet_red1",
    "pink1",
    "pink3",
    "plum1",
    "plum2",
    "plum3",
    "plum4",
    "purple",
    "purple3",
    "purple4",
    "red",
    "red1",
    "red3",
    "rosy_brown",
    "royal_blue1",
    "salmon1",
    "sandy_brown",
    "sea_green1",
    "sea_green2",
    "sea_green3",
    "sky_blue1",
    "sky_blue2",
    "sky_blue3",
    "slate_blue1",
    "slate_blue3",
    "spring_green1",
    "spring_green2",
    "spring_green3",
    "spring_green4",
    "steel_blue",
    "steel_blue1",
    "steel_blue3",
    "tan",
    "thistle1",
    "thistle3",
    "turquoise2",
    "turquoise4",
    "violet",
    "wheat1",
    "wheat4",
    "white",
    "yellow",
    "yellow1",
    "yellow2",
    "yellow3",
    "yellow4",
];

/// Port of `rich.text.Text` (the subset the shell uses).
#[derive(Debug, Clone, Default)]
pub struct Text {
    /// The literal characters (`plain` property).
    pub plain: String,
    /// Base style name applied to the whole text.
    pub style: String,
    /// Styled regions (character offsets).
    pub spans: Vec<Span>,
    /// Never word-wrap when true.
    pub no_wrap: bool,
    /// Overflow behaviour (rich default `fold`).
    pub overflow: Option<Overflow>,
    /// Appended when rendered (usually "\n").
    pub end: String,
}

impl Text {
    /// `Text(text, style=style)`.
    pub fn new(text: &str, style: &str) -> Self {
        Text {
            plain: text.to_string(),
            style: style.to_string(),
            end: "\n".into(),
            ..Default::default()
        }
    }

    /// `Text()` with no style and no end marker semantics needed here.
    pub fn blank() -> Self {
        Text::new("", "")
    }

    /// `Text.from_markup(markup, style=style)` — tags push/pop spans; `\[`
    /// escapes. Unclosed tags span to the end, like rich.
    pub fn from_markup(markup: &str, style: &str) -> Self {
        let mut text = Text {
            style: style.to_string(),
            end: "\n".into(),
            ..Default::default()
        };
        if !markup.contains('[') {
            text.plain = markup.to_string();
            return text;
        }
        let raw: Vec<char> = markup.chars().collect();
        let mut spans: Vec<Span> = Vec::new();
        // (tag_name_normalized, start char offset)
        let mut stack: Vec<(String, usize)> = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == '\\' {
                // Count the backslash run; escapes apply only before a valid tag.
                let mut n = 0;
                while raw.get(i + n) == Some(&'\\') {
                    n += 1;
                }
                let tag_start = i + n;
                let valid_tag =
                    raw.get(tag_start) == Some(&'[') && tag_at(&raw, tag_start).is_some();
                if !valid_tag {
                    // Not a tag context: backslashes stay (rich yields them as text);
                    // `replace("\\[", "[")` strips a backslash that precedes a bare `[`.
                    for _ in 0..n {
                        text.plain.push('\\');
                    }
                    i += n;
                    if raw.get(i) == Some(&'[') && n % 2 == 1 {
                        text.plain.pop();
                        text.plain.push('[');
                        i += 1;
                    }
                    continue;
                }
                i += n;
                for _ in 0..n / 2 {
                    text.plain.push('\\');
                }
                if n % 2 == 1 {
                    // Literal tag: "[" is emitted; the rest parses as plain text.
                    text.plain.push('[');
                    i += 1;
                    continue;
                }
                continue;
            }
            if raw[i] == '[' && tag_at(&raw, i).is_some() {
                let end = tag_at(&raw, i).unwrap();
                let tag_text: String = raw[i + 1..end - 1].iter().collect();
                i = end;
                let (name, params) = match tag_text.split_once('=') {
                    Some((n, p)) => (n.to_string(), Some(p.to_string())),
                    None => (tag_text, None),
                };
                if let Some(style_name) = name.strip_prefix('/') {
                    let style_name = style_name.trim();
                    if style_name.is_empty() {
                        if let Some((open, start)) = stack.pop() {
                            let _ = open;
                            // Rebuild str(tag): name (+ " params" if present).
                            let (tag_name, tag_params) = stack_last_tag(&open);
                            spans.push(Span {
                                start,
                                end: chars(&text.plain),
                                style: str_tag(&tag_name, &tag_params),
                            });
                        } else {
                            // Markup error; the shell never produces it.
                        }
                    } else {
                        let normalized = style_normalize(style_name);
                        if let Some(pos) = stack.iter().rposition(|(n, _)| tag_key(n) == normalized)
                        {
                            let (open, start) = stack.remove(pos);
                            let (tag_name, tag_params) = stack_last_tag(&open);
                            spans.push(Span {
                                start,
                                end: chars(&text.plain),
                                style: str_tag(&tag_name, &tag_params),
                            });
                        }
                    }
                } else {
                    let normalized = style_normalize(&name);
                    let stored = match params {
                        Some(p) => format!("{normalized}={p}"),
                        None => normalized,
                    };
                    stack.push((stored, chars(&text.plain)));
                }
                continue;
            }
            // Rich also replaces `\[` with `[` in plain chunks; handled above.
            text.plain.push(raw[i]);
            i += 1;
        }
        let tail = chars(&text.plain);
        while let Some((open, start)) = stack.pop() {
            let (tag_name, tag_params) = stack_last_tag(&open);
            let style = str_tag(&tag_name, &tag_params);
            if !style.is_empty() {
                spans.push(Span {
                    start,
                    end: tail,
                    style,
                });
            }
        }
        spans.reverse();
        spans.sort_by_key(|s| s.start);
        text.spans = spans;
        text
    }

    /// `text.append(s, style)` — records a span when `style` is non-empty.
    pub fn append(&mut self, s: &str, style: &str) {
        let start = chars(&self.plain);
        self.plain.push_str(s);
        if !style.is_empty() {
            self.spans.push(Span {
                start,
                end: chars(&self.plain),
                style: style.to_string(),
            });
        }
    }

    /// `text.stylize(style, start, end)`.
    pub fn stylize(&mut self, style: &str, start: usize, end: usize) {
        if style.is_empty() || start >= end {
            return;
        }
        self.spans.push(Span {
            start,
            end,
            style: style.to_string(),
        });
    }

    /// `text.plain` cell width.
    pub fn cell_len(&self) -> usize {
        cell_len(&self.plain)
    }

    /// `text.copy()`.
    pub fn copy(&self) -> Self {
        self.clone()
    }

    /// `text.split("\n", allow_blank=True)` — spans are re-offset per line and
    /// the separator segments are dropped (rich's `if line.plain != separator`).
    pub fn split_lines(&self) -> Vec<Text> {
        let mut offsets: Vec<usize> = Vec::new();
        for (pos, c) in self.plain.chars().enumerate() {
            if c == '\n' {
                offsets.push(pos);
                offsets.push(pos + 1);
            }
        }
        self.divide(&offsets)
            .into_iter()
            .filter(|t| t.plain != "\n")
            .collect()
    }

    /// `text.divide(offsets)` — character offsets; spans re-offset per segment.
    pub fn divide(&self, offsets: &[usize]) -> Vec<Text> {
        let mut cuts = vec![0usize];
        cuts.extend(offsets.iter().copied());
        cuts.push(chars(&self.plain));
        let plain_chars: Vec<char> = self.plain.chars().collect();
        cuts.windows(2)
            .map(|w| {
                let (a, b) = (w[0], w[1]);
                let segment: String = plain_chars
                    .get(a.min(plain_chars.len())..b.min(plain_chars.len()))
                    .unwrap_or(&[])
                    .iter()
                    .collect();
                let spans = self
                    .spans
                    .iter()
                    .filter_map(|span| {
                        let start = span.start.max(a);
                        let end = span.end.min(b);
                        (start < end).then(|| Span {
                            start: start - a,
                            end: end - a,
                            style: span.style.clone(),
                        })
                    })
                    .collect();
                Text {
                    plain: segment,
                    style: self.style.clone(),
                    spans,
                    // rich's divide constructs fresh Texts: no_wrap and end
                    // reset to the defaults rather than copying self's.
                    no_wrap: false,
                    overflow: self.overflow,
                    end: "\n".into(),
                }
            })
            .collect()
    }

    /// `text.expand_tabs(tab_size)` — tab stops every `tab_size` characters
    ///.
    pub fn expand_tabs(&mut self, tab_size: usize) {
        if !self.plain.contains('\t') || tab_size == 0 {
            return;
        }
        let lines: Vec<String> = self
            .plain
            .split('\n')
            .map(|line| {
                let mut parts: Vec<String> = line.split('\t').map(str::to_string).collect();
                for part_index in 0..parts.len().saturating_sub(1) {
                    let pos: usize = parts[..=part_index].iter().map(|p| chars(p)).sum();
                    let spaces = tab_size - (pos % tab_size);
                    parts[part_index + 1] =
                        format!("{}{}", " ".repeat(spaces), parts[part_index + 1]);
                }
                parts.concat()
            })
            .collect();
        self.plain = lines.join("\n");
    }

    /// `text.right_crop(n)`.
    pub fn right_crop(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        let keep = chars(&self.plain).saturating_sub(n);
        self.plain = self.plain.chars().take(keep).collect();
        self.trim_spans();
    }

    /// `text.rstrip_end(size)` — drop trailing whitespace beyond `size` chars.
    pub fn rstrip_end(&mut self, size: usize) {
        let text_length = chars(&self.plain);
        if text_length > size {
            let excess = text_length - size;
            let ws = self
                .plain
                .chars()
                .rev()
                .take_while(|c| c.is_whitespace())
                .count();
            if ws > 0 {
                self.right_crop(ws.min(excess));
            }
        }
    }

    /// `text.rstrip()` — trim trailing whitespace.
    pub fn rstrip(&mut self) {
        self.plain = self.plain.trim_end().to_string();
    }

    /// Drop/clip spans beyond the end of the text (rich `_trim_spans`).
    pub fn trim_spans(&mut self) {
        let max_offset = chars(&self.plain);
        self.spans.retain_mut(|span| {
            if span.end > max_offset {
                span.end = max_offset;
            }
            span.start < max_offset
        });
    }

    /// `text.truncate(max_width, overflow, pad)`.
    pub fn truncate(&mut self, max_width: usize, overflow: Option<Overflow>, pad: bool) {
        let of = overflow.or(self.overflow).unwrap_or(Overflow::Fold);
        if of == Overflow::Ignore {
            return;
        }
        let length = cell_len(&self.plain);
        if length > max_width {
            self.plain = if of == Overflow::Ellipsis {
                format!(
                    "{}…",
                    set_cell_size(&self.plain, max_width.saturating_sub(1))
                )
            } else {
                set_cell_size(&self.plain, max_width)
            };
            self.trim_spans();
        } else if pad && length < max_width {
            self.plain
                .extend(std::iter::repeat_n(' ', max_width - length));
        }
    }

    /// `text.pad_left(n)` / `pad_right(n)` / `pad(n)`.
    pub fn pad_left(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        self.plain = format!("{}{}", " ".repeat(n), self.plain);
        for span in &mut self.spans {
            span.start += n;
            span.end += n;
        }
    }

    /// Pad `n` cells on the right.
    pub fn pad_right(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        self.plain.extend(std::iter::repeat_n(' ', n));
    }

    /// Pad `n` cells on both sides.
    pub fn pad(&mut self, n: usize) {
        self.pad_left(n);
        self.pad_right(n);
    }

    /// `text.wrap(width, overflow=..., no_wrap=...)`: rich's divide_line +
    /// rstrip_end + truncate pipeline (justify "default" is a no-op).
    pub fn wrap(
        &self,
        width: usize,
        overflow: Option<Overflow>,
        no_wrap: Option<bool>,
    ) -> Vec<Text> {
        let wrap_overflow = overflow.or(self.overflow).unwrap_or(Overflow::Fold);
        let no_wrap = no_wrap.unwrap_or(self.no_wrap) || wrap_overflow == Overflow::Ignore;
        let mut lines: Vec<Text> = Vec::new();
        for mut line in self.split_lines() {
            if line.plain.contains('\t') {
                line.expand_tabs(8);
            }
            let mut new_lines: Vec<Text> = if no_wrap {
                vec![line]
            } else {
                let offsets = divide_line(&line.plain, width, wrap_overflow == Overflow::Fold);
                let mut divided = line.divide(&offsets);
                for l in &mut divided {
                    l.rstrip_end(width);
                }
                divided
            };
            for l in &mut new_lines {
                l.truncate(width, Some(wrap_overflow), false);
            }
            lines.extend(new_lines);
        }
        lines
    }
}

/// `(stored_tag) -> (name, params)` — stored tags keep the normalized name.
fn stack_last_tag(stored: &str) -> (String, Option<String>) {
    match stored.split_once('=') {
        Some((n, p)) => (n.to_string(), Some(p.to_string())),
        None => (stored.to_string(), None),
    }
}

/// `str(Tag)`: `name` or `name params` (space, not `=`, in rich's repr).
fn str_tag(name: &str, params: &Option<String>) -> String {
    match params {
        Some(p) => format!("{name} {p}"),
        None => name.to_string(),
    }
}

/// Key used when matching a closing tag to the stack (`name` part only).
fn tag_key(stored: &str) -> String {
    stored
        .split_once('=')
        .map(|(n, _)| n.to_string())
        .unwrap_or_else(|| stored.to_string())
}

/// Rich `_wrap.divide_line` — word-wrap break offsets (char indices) for `text`
/// at `width` cells. With `fold=false`, overlong words are cropped, not folded.
pub fn divide_line(text: &str, width: usize, fold: bool) -> Vec<usize> {
    let mut breaks = Vec::new();
    let mut cell_offset = 0usize;
    for (mut start, word) in words(text) {
        let word_length = cell_len(word.trim_end());
        let remaining_space = width.saturating_sub(cell_offset);
        if remaining_space >= word_length {
            cell_offset += cell_len(&word);
        } else if word_length > width {
            if fold {
                let folded = chop_cells(&word, width);
                let last_idx = folded.len();
                for (idx, line) in folded.into_iter().enumerate() {
                    if start > 0 {
                        breaks.push(start);
                    }
                    if idx == last_idx - 1 {
                        cell_offset = cell_len(&line);
                    } else {
                        start += chars(&line);
                    }
                }
            } else {
                if start > 0 {
                    breaks.push(start);
                }
                cell_offset = cell_len(&word);
            }
        } else if cell_offset > 0 && start > 0 {
            breaks.push(start);
            cell_offset = cell_len(&word);
        }
    }
    breaks
}

/// `rich._wrap.words` — yields `(start, word)` where word = `\s*\S+\s*`.
fn words(text: &str) -> Vec<(usize, String)> {
    let chars_vec: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars_vec.len() {
        let start = i;
        while i < chars_vec.len() && chars_vec[i].is_whitespace() {
            i += 1;
        }
        while i < chars_vec.len() && !chars_vec[i].is_whitespace() {
            i += 1;
        }
        while i < chars_vec.len() && chars_vec[i].is_whitespace() {
            i += 1;
        }
        if i > start {
            out.push((start, chars_vec[start..i].iter().collect()));
        }
    }
    out
}
