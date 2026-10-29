//! TUI panels: the frame, its sections, and the shared line renderers.
//!
//! static chrome only; live views live
//! in `watch_views` and the observer engine in `app`.

use crate::localization::{number, ui};
use crate::render::Renderable;
use crate::scan_now;
use crate::state::ShellState;
use crate::text::{escape, Text};
use crate::theme::{badge, Palette};
/// `spiderhead.__version__` — kept in core so the shell shows the toolkit build.
use spiderhead_core::user_agents::PRODUCT_VERSION as TOOLKIT_VERSION;

/// Below this size the shell renders a compact notice instead of a clipped
/// half-frame.
pub const MIN_WIDTH: usize = 56;
/// Companion minimum height.
pub const MIN_HEIGHT: usize = 16;

const TITLE: &str = "SpiderHead Tools";

const PALETTE_HINTS: &str =
    "type filter · up/down move · enter open · esc clear/back · ? keys · q quit";
const DETAIL_HINTS: &str = "enter/esc back · ctrl-c quit";
const HELP_HINTS: &str = "esc back · ctrl-c quit";
const WATCH_HINTS: &str =
    "1 overview · 2 tasks · 3 methods · 4 scans · 5 findings · 6 views · 7 activity · 8 log · 9 Screaming Frog · 0 inbox · arrows/page browse · n note · g goal · q quit";
const NOTE_HINTS: &str = "type note · enter save · esc keep draft · ctrl-x discard";
const FILTER_HINTS: &str = "type finding filter · enter apply · esc discard";
const WATCH_DETAIL_HINTS: &str = "enter/esc back · ctrl-c quit";

/// `_HELP_LINES` — palette key reference.
const HELP_LINES: [(&str, &str); 9] = [
    (
        "up / down, page up / page down",
        "move the cursor through the command list",
    ),
    ("home / end", "jump to the first / last command"),
    ("printable characters", "filter the palette"),
    ("backspace", "delete one filter character"),
    ("enter", "open the selected command"),
    (
        "esc",
        "close a view, clear the filter, then leave the shell",
    ),
    ("q (empty filter)", "leave the shell"),
    ("ctrl-c", "leave the shell immediately"),
    ("?", "show this reference"),
];

/// `command_rows` — the CLI passes its command table in.
pub fn command_rows(commands: &[String]) -> Vec<String> {
    commands.to_vec()
}

/// `_header_text` — versioned title line.
pub fn header_text(palette: &Palette) -> Text {
    if !palette.color {
        return Text::new(
            &format!("{TITLE} {TOOLKIT_VERSION} — {}", ui("interactive shell")),
            "",
        );
    }
    let mut text = Text::blank();
    text.append(TITLE, &palette.accent);
    text.append(&format!(" {TOOLKIT_VERSION}"), &palette.title);
    text.append(&format!("  {}", ui("interactive shell")), &palette.muted);
    text
}

/// `_status_line` — badges plus per-view key hints.
pub fn status_line(state: &ShellState, palette: &Palette, width: usize, height: usize) -> Text {
    let mode = if palette.color {
        badge("ok", "COLOR", palette)
    } else {
        badge("off", "NO COLOR", palette)
    }
    .unwrap_or_default();
    let size = badge("info", &format!("{width}x{height}"), palette).unwrap_or_default();
    let hints = match state.view.as_str() {
        "detail" => DETAIL_HINTS,
        "help" | "watch_help" => HELP_HINTS,
        "watch" => WATCH_HINTS,
        "note" => NOTE_HINTS,
        "watch_filter" => FILTER_HINTS,
        "watch_detail" => WATCH_DETAIL_HINTS,
        _ => PALETTE_HINTS,
    };
    if palette.color {
        Text::from_markup(&format!("{mode}  {size}  {hints}"), "")
    } else {
        Text::new(&format!("{mode}  {size}  {hints}"), "")
    }
}

/// `_palette_lines` — visible slice of the filtered command list.
pub fn palette_lines(state: &ShellState, palette: &Palette, rows: usize) -> Vec<Text> {
    let items = state.filtered();
    let cursor = (state.index as usize).min(items.len().saturating_sub(1));
    let first = (cursor as i64 - rows as i64 + 1)
        .clamp(0, (items.len() as i64 - rows as i64).max(0))
        .max(0) as usize;
    let visible: Vec<&str> = items[first..].iter().take(rows).copied().collect();
    let mut lines: Vec<Text> = Vec::new();
    for (offset, name) in visible.iter().enumerate() {
        let current = first + offset == cursor;
        let marker = if current { ">" } else { " " };
        if palette.color && current {
            lines.push(Text::from_markup(
                &format!("[{}] {} {} [/]", palette.highlight, marker, escape(name)),
                "",
            ));
        } else {
            lines.push(Text::new(&format!(" {marker} {name}"), ""));
        }
    }
    if items.is_empty() {
        let empty = format!("no commands match {:?}", state.query);
        lines.push(if palette.color {
            Text::from_markup(&format!("[{}]{}[/]", palette.muted, escape(&empty)), "")
        } else {
            Text::new(&empty, "")
        });
    }
    lines
}

/// Description/help provider injected by the CLI layer — mirrors the
/// `build_parser`/`HANDLERS` lookups in `_command_reference` without creating
/// a downward dependency on the CLI crate.
pub trait CommandDocs {
    /// Handler docstring for the command (`HANDLERS[name.replace('-','_')]`).
    fn description(&self, command: &str) -> Option<String>;
    /// `command.format_help()` output for the command, when it is a leaf.
    fn help(&self, command: &str) -> Option<String>;
}

/// `_command_reference` — shared CLI arguments and handler documentation,
/// executed nowhere.
pub fn command_reference(name: &str, docs: Option<&dyn CommandDocs>) -> Vec<String> {
    let description = docs
        .and_then(|d| d.description(name))
        .unwrap_or_else(|| "Command group; choose one of its listed subcommands.".into());
    let mut lines = vec![
        name.to_string(),
        description,
        String::new(),
        "Not executed here. Run the CLI explicitly when ready.".into(),
        String::new(),
    ];
    match docs.and_then(|d| d.help(name)) {
        Some(help) => lines.extend(help.lines().map(str::to_string)),
        None => lines.push(format!("seohead {name} --help")),
    }
    lines
}

/// `_detail_lines`.
pub fn detail_lines(
    state: &ShellState,
    _palette: &Palette,
    docs: Option<&dyn CommandDocs>,
) -> Vec<Text> {
    command_reference(state.selected().unwrap_or(""), docs)
        .into_iter()
        .map(|line| Text::new(&line, ""))
        .collect()
}

/// `_help_lines`.
pub fn help_lines(palette: &Palette) -> Vec<Text> {
    let mut out = vec![
        if palette.color {
            Text::new("keys", &palette.title)
        } else {
            Text::new("KEYS", "")
        },
        Text::blank(),
    ];
    let width = HELP_LINES.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (combo, meaning) in HELP_LINES {
        out.push(Text::new(&format!("  {combo:width$}  {meaning}"), ""));
    }
    out
}

/// `_select` — a bounded selection list without assigning an action to Enter.
pub fn select(lines: &[String], index: i64, palette: &Palette) -> Vec<Text> {
    if lines.is_empty() {
        return vec![Text::new("  no retained entries", "")];
    }
    let selected = index.clamp(0, lines.len() as i64 - 1) as usize;
    lines
        .iter()
        .enumerate()
        .map(|(ordinal, line)| {
            Text::new(
                &format!(" {} {line}", if ordinal == selected { ">" } else { " " }),
                if palette.color && ordinal == selected {
                    &palette.highlight
                } else {
                    ""
                },
            )
        })
        .collect()
}

/// `_note_lines` — the note editor body.
pub fn note_lines(state: &ShellState, palette: &Palette) -> Vec<Text> {
    let title = if state.note_kind == "proposed_goal" {
        "new proposed goal"
    } else {
        "new project note"
    };
    let label = if palette.color {
        Text::new(title, &palette.title)
    } else {
        Text::new(&title.to_uppercase(), "")
    };
    let cursor = state
        .note_cursor
        .unwrap_or_else(|| state.note_text.chars().count());
    let mut draft = Text::new(
        if state.note_text.is_empty() {
            "(type or dictate text, then press enter)"
        } else {
            &state.note_text
        },
        "",
    );
    if !state.note_text.is_empty() {
        draft.stylize("reverse", cursor, cursor + 1);
        if cursor == state.note_text.chars().count() {
            draft.append("▏", if palette.color { &palette.accent } else { "" });
        }
    }
    vec![
        label,
        Text::new(
            &format!(
                "{}/{} characters",
                number(state.note_text.chars().count() as i64),
                number(state.note_limit)
            ),
            "",
        ),
        Text::new(
            &state.note_error,
            if palette.color { "#fbbf24" } else { "" },
        ),
        draft,
    ]
}

/// `_meter` — a denominator-backed gauge; unknown coverage stays unknown.
pub fn meter(label: &str, done: Option<i64>, total: Option<i64>, palette: &Palette) -> Text {
    let mut text = Text::new(
        &format!("{label}\n"),
        if palette.color { &palette.muted } else { "" },
    );
    if total == Some(0) {
        text.append(&ui("None in agreed scope"), "");
        return text;
    }
    let (Some(done), Some(total)) = (done, total) else {
        text.append(
            &ui("Not measured"),
            if palette.color { "#fbbf24" } else { "" },
        );
        return text;
    };
    let fraction = (done as f64 / total as f64).clamp(0.0, 1.0);
    let filled = crate::rlayout::py_round(20.0 * fraction) as usize;
    text.append(
        &format!(
            "{} / {}  {}%\n",
            number(done),
            number(total),
            crate::rlayout::py_round(fraction * 100.0)
        ),
        "",
    );
    text.append(
        &"█".repeat(filled),
        if palette.color { &palette.accent } else { "" },
    );
    text.append(
        &"░".repeat(20 - filled),
        if palette.color { &palette.muted } else { "" },
    );
    text
}

/// `build_frame` — one screen of the shell as a renderable group.
///
/// `dashboard` is `app::watch_dashboard`; it is a parameter so this module
/// keeps the same lazy-through-the-package wiring tests patch.
#[allow(clippy::too_many_arguments)]
pub fn build_frame(
    state: &mut ShellState,
    width: usize,
    height: usize,
    palette: &Palette,
    project: Option<&str>,
    message: Option<&str>,
    snapshot_override: Option<&(Option<serde_json::Value>, Vec<Text>)>,
    view_override: Option<&serde_json::Value>,
    compact: bool,
    docs: Option<&dyn CommandDocs>,
) -> Renderable {
    if compact {
        return Renderable::Group(vec![scan_now::panel(
            snapshot_override.and_then(|(s, _)| s.as_ref()),
            palette,
            true,
        )]);
    }
    let header = header_text(palette);
    let status = status_line(state, palette, width, height);
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        let notice = Text::new(
            &format!(
                "terminal too small ({width}x{height}); need at least {MIN_WIDTH}x{MIN_HEIGHT}\nresize the window or press q / ctrl-c to leave"
            ),
            "",
        );
        return Renderable::Group(vec![
            header.into(),
            Text::blank().into(),
            notice.into(),
            Text::blank().into(),
            status.into(),
        ]);
    }
    if project.is_some()
        && [
            "watch",
            "watch_detail",
            "watch_filter",
            "note",
            "watch_help",
        ]
        .contains(&state.view.as_str())
    {
        return Renderable::Group(vec![crate::app::watch_dashboard(
            state,
            project.unwrap_or_default(),
            palette,
            width,
            height,
            message,
            snapshot_override,
            view_override,
        )]);
    }
    let footer = status.wrap(width, None, None);
    let body_rows = (height as i64 - 4 - footer.len() as i64).max(1) as usize;
    let mut body: Vec<Text> = match state.view.as_str() {
        "detail" => detail_lines(state, palette, docs),
        "help" => help_lines(palette),
        _ => {
            let filter_line = if palette.color {
                Text::from_markup(
                    &format!("[{}]filter:[/] {}", palette.muted, escape(&state.query)),
                    "",
                )
            } else {
                Text::new(&format!("filter: {}", state.query), "")
            };
            let mut group = vec![header.into(), Text::blank().into(), filter_line.into()];
            group.extend(
                palette_lines(state, palette, (body_rows as i64 - 1).max(1) as usize)
                    .into_iter()
                    .map(Renderable::Text),
            );
            group.push(Text::blank().into());
            group.extend(footer.into_iter().map(Renderable::Text));
            return Renderable::Group(group);
        }
    };
    // Keep each evidence row within its allotted line; a long URL or note
    // must not push the footer below the terminal viewport.
    if ["detail", "help"].contains(&state.view.as_str()) {
        body = body
            .iter()
            .flat_map(|line| line.wrap(width, None, None))
            .collect();
        state.watch_detail_offset = state
            .watch_detail_offset
            .min((body.len() as i64 - body_rows as i64).max(0));
        body = body.split_off((state.watch_detail_offset.max(0) as usize).min(body.len()));
    }
    for line in &mut body {
        line.no_wrap = true;
        line.overflow = Some(crate::text::Overflow::Ellipsis);
    }
    let mut group: Vec<Renderable> = vec![header.into(), Text::blank().into()];
    group.extend(body.into_iter().take(body_rows).map(Renderable::Text));
    group.push(Text::blank().into());
    group.extend(footer.into_iter().map(Renderable::Text));
    Renderable::Group(group)
}

impl From<Text> for Renderable {
    fn from(text: Text) -> Self {
        Renderable::Text(text)
    }
}
