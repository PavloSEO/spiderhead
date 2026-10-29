//! `usage:` lines and `--help` screens, laid out exactly like reference 3.14's `argparse.HelpFormatter`.
//!
//! This is a port of the formatter, not an approximation: same wrapping of the usage line, same
//! help-column rule (`max_help_position`), same section order, same blank-line collapsing. Colour
//! is never emitted (the reference capture runs with `NO_COLOR`).

use crate::textwrap;
use crate::tree::{Action, Nargs, Node};

/// Terminal width the way `shutil.get_terminal_size()` resolves it without a tty:
/// `$COLUMNS` when it is a positive integer, else 80.
pub fn terminal_columns() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&c| c > 0)
        .unwrap_or(80)
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Default metavar of an argument: `dest` for positionals, `DEST` for options.
fn default_metavar(action: &Action) -> String {
    if action.option_strings.is_empty() {
        action.dest.clone()
    } else {
        action.dest.to_uppercase()
    }
}

/// `_metavar_formatter(...)(size)`: the tuple of metavars, repeated `size` times when a plain string.
fn metavars(node: &Node, action: &Action, default: &str, size: usize) -> Vec<String> {
    if action.metavar_is_tuple() {
        return action.metavar_tuple().unwrap_or_default();
    }
    let one = if let Some(m) = action.metavar_tuple() {
        m.into_iter().next().unwrap_or_default()
    } else if let Some(c) = action.choice_strings() {
        format!("{{{}}}", c.join(","))
    } else if action.kind == "parsers" {
        format!("{{{}}}", node.child_names().join(","))
    } else {
        default.to_owned()
    };
    vec![one; size]
}

/// `_format_args`: the value part of an invocation, e.g. `[X [X ...]]`.
fn format_args(node: &Node, action: &Action, default: &str) -> String {
    let get = |n| metavars(node, action, default, n);
    match action.nargs() {
        Nargs::One => get(1).join(" "),
        Nargs::Optional => format!("[{}]", get(1)[0]),
        Nargs::ZeroOrMore => {
            let m = get(1);
            if m.len() == 2 {
                format!("[{} [{} ...]]", m[0], m[1])
            } else {
                format!("[{} ...]", m[0])
            }
        }
        Nargs::OneOrMore => {
            let m = get(2);
            format!("{} [{} ...]", m[0], m.get(1).unwrap_or(&m[0]))
        }
        Nargs::Remainder => "...".to_owned(),
        Nargs::Parser => format!("{} ...", get(1)[0]),
        Nargs::Suppress => String::new(),
        Nargs::Count(n) => get(n).join(" "),
    }
}

/// `_format_action_invocation`: the left column of a help entry.
fn invocation(node: &Node, action: &Action) -> String {
    if action.option_strings.is_empty() {
        return metavars(node, action, &default_metavar(action), 1).join(" ");
    }
    let opts = action.option_strings.join(", ");
    if action.nargs() == Nargs::Count(0) {
        opts
    } else {
        format!(
            "{opts} {}",
            format_args(node, action, &default_metavar(action))
        )
    }
}

/// The text of one choice pseudo-action: name plus `(aliases)`.
fn sub_label(p: &crate::tree::SubParser) -> String {
    if p.aliases.is_empty() {
        p.name.clone()
    } else {
        format!("{} ({})", p.name, p.aliases.join(", "))
    }
}

fn expand_help(action: &Action) -> Option<&str> {
    action.help_expanded.as_deref().or(action.help.as_deref())
}

struct Formatter<'a> {
    node: &'a Node,
    width: usize,
    max_help_position: usize,
    indent: usize,
    action_max_length: usize,
    raw: bool,
}

impl<'a> Formatter<'a> {
    fn new(node: &'a Node, columns: usize) -> Self {
        let width = columns.saturating_sub(2);
        Self {
            node,
            width,
            max_help_position: 24.min(width.saturating_sub(20).max(4)),
            indent: 0,
            action_max_length: 0,
            raw: node.formatter == "RawDescriptionHelpFormatter",
        }
    }

    fn visible(&self, idx: usize) -> Option<&'a Action> {
        self.node.actions.get(idx).filter(|a| !a.hidden())
    }

    /// Phase 1 of `add_argument`: widen the help column to fit this entry and its sub-entries.
    fn measure(&mut self, action: &Action) {
        let mut longest = chars(&invocation(self.node, action)) + self.indent;
        if action.kind == "parsers" {
            for p in action.parsers.iter().filter(|p| p.listed) {
                longest = longest.max(chars(&sub_label(p)) + self.indent + 2);
            }
        }
        self.action_max_length = self.action_max_length.max(longest);
    }

    fn fill_text(&self, text: &str, width: usize, indent: &str) -> String {
        if self.raw {
            text.split_inclusive('\n')
                .map(|l| format!("{indent}{l}"))
                .collect()
        } else {
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            textwrap::fill(&text, width, indent)
        }
    }

    fn format_text(&self, text: &str) -> String {
        let text = if text.contains("%(prog)") {
            text.replace("%(prog)s", &self.node.prog).replace("%%", "%")
        } else {
            text.to_owned()
        };
        let width = self.width.saturating_sub(self.indent).max(11);
        format!(
            "{}\n\n",
            self.fill_text(&text, width, &" ".repeat(self.indent))
        )
    }

    /// `_format_action` for one entry; `label`/`help` are given so choice pseudo-actions reuse it.
    fn format_entry(&self, label: &str, help: Option<&str>) -> String {
        let help_position = (self.action_max_length + 2).min(self.max_help_position);
        let help_width = self.width.saturating_sub(help_position).max(11);
        let action_width = help_position as isize - self.indent as isize - 2;
        let pad = " ".repeat(self.indent);
        let mut out;
        let indent_first;
        if help.is_none_or(str::is_empty) {
            return format!("{pad}{label}\n");
        } else if (chars(label) as isize) <= action_width {
            let fill = " ".repeat((action_width as usize).saturating_sub(chars(label)));
            out = format!("{pad}{label}{fill}  ");
            indent_first = 0;
        } else {
            out = format!("{pad}{label}\n");
            indent_first = help_position;
        }
        let text = help.unwrap_or_default();
        if text.trim().is_empty() {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            return out;
        }
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let lines = textwrap::wrap(&text, help_width, "", "");
        if let Some(first) = lines.first() {
            out.push_str(&format!("{}{first}\n", " ".repeat(indent_first)));
        }
        for line in lines.iter().skip(1) {
            out.push_str(&format!("{}{line}\n", " ".repeat(help_position)));
        }
        out
    }

    fn format_action(&mut self, action: &Action) -> String {
        let mut out = self.format_entry(&invocation(self.node, action), expand_help(action));
        if action.kind == "parsers" {
            self.indent += 2;
            for p in action.parsers.iter().filter(|p| p.listed) {
                out.push_str(&self.format_entry(&sub_label(p), p.help.as_deref()));
            }
            self.indent -= 2;
        }
        out
    }
}

/// `_get_actions_usage_parts`: the pieces of the usage line and the index where positionals begin.
fn usage_parts(node: &Node) -> (Vec<String>, usize) {
    let actions: Vec<usize> = (0..node.actions.len())
        .filter(|&i| !node.actions[i].hidden())
        .collect();
    let mut group_of: Vec<Option<usize>> = vec![None; node.actions.len()];
    for (g, group) in node.mutex.iter().enumerate() {
        for &i in &group.actions {
            if actions.contains(&i) {
                group_of[i] = Some(g);
            }
        }
    }
    let mut remaining: Vec<usize> = actions.clone();
    let take = |remaining: &mut Vec<usize>, i: usize| -> bool {
        remaining
            .iter()
            .position(|&x| x == i)
            .map(|p| remaining.remove(p))
            .is_some()
    };
    let mut positionals: Vec<(Option<bool>, Vec<usize>)> = Vec::new();
    for &i in &actions {
        if node.actions[i].option_strings.is_empty() {
            take(&mut remaining, i);
            match group_of[i] {
                Some(g) => {
                    let mut members: Vec<usize> = node.mutex[g]
                        .actions
                        .iter()
                        .copied()
                        .filter(|&j| {
                            !node.actions[j].option_strings.is_empty() && take(&mut remaining, j)
                        })
                        .collect();
                    members.push(i);
                    positionals.push((Some(node.mutex[g].required), members));
                }
                None => positionals.push((None, vec![i])),
            }
        }
    }
    let mut optionals: Vec<(Option<bool>, Vec<usize>)> = Vec::new();
    for &i in &actions {
        if !node.actions[i].option_strings.is_empty() && remaining.contains(&i) {
            take(&mut remaining, i);
            match group_of[i] {
                Some(g) => {
                    let mut members = vec![i];
                    members.extend(node.mutex[g].actions.iter().copied().filter(|&j| {
                        !node.actions[j].option_strings.is_empty() && take(&mut remaining, j)
                    }));
                    optionals.push((Some(node.mutex[g].required), members));
                }
                None => optionals.push((None, vec![i])),
            }
        }
    }
    let n_opt = optionals.len();
    let mut parts: Vec<String> = Vec::new();
    let mut pos_start = None;
    for (n, (required, group)) in optionals.into_iter().chain(positionals).enumerate() {
        let start = parts.len();
        if n == n_opt {
            pos_start = Some(start);
        }
        let in_group = group.len() > 1;
        for &i in &group {
            let action = &node.actions[i];
            let part = if action.option_strings.is_empty() {
                let mut p = format_args(node, action, &default_metavar(action));
                if in_group && p.starts_with('[') && p.ends_with(']') {
                    p = p[1..p.len() - 1].to_owned();
                }
                p
            } else {
                let first = &action.option_strings[0];
                let mut p = if action.nargs() == Nargs::Count(0) {
                    first.clone()
                } else {
                    format!(
                        "{first} {}",
                        format_args(node, action, &default_metavar(action))
                    )
                };
                if !(action.required || required == Some(true) || in_group) {
                    p = format!("[{p}]");
                }
                p
            };
            parts.push(part);
        }
        if in_group {
            let req = required == Some(true);
            parts[start] = format!("{}{}", if req { '(' } else { '[' }, parts[start]);
            let last = parts.len() - 1;
            for part in &mut parts[start..last] {
                part.push_str(" |");
            }
            parts[last].push(if req { ')' } else { ']' });
        }
    }
    let pos_start = pos_start.unwrap_or(parts.len());
    (parts, pos_start)
}

fn get_lines(
    parts: &[String],
    indent: &str,
    prefix: Option<&str>,
    text_width: usize,
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line: Vec<&str> = Vec::new();
    let indent_len = chars(indent);
    let mut line_len = prefix.map_or(indent_len, chars).saturating_sub(1);
    for part in parts {
        let part_len = chars(part);
        if line_len + 1 + part_len > text_width && !line.is_empty() {
            lines.push(format!("{indent}{}", line.join(" ")));
            line.clear();
            line_len = indent_len.saturating_sub(1);
        }
        line.push(part);
        line_len += part_len + 1;
    }
    if !line.is_empty() {
        lines.push(format!("{indent}{}", line.join(" ")));
    }
    if prefix.is_some() {
        if let Some(first) = lines.first_mut() {
            *first = first.chars().skip(indent_len).collect();
        }
    }
    lines
}

/// The `usage: ...` block (with the trailing blank line) of a parser.
fn format_usage(node: &Node, width: usize) -> String {
    let prefix = "usage: ";
    let prog = node.prog.as_str();
    if node.actions.is_empty() {
        return format!("{prefix}{prog}\n\n");
    }
    let (parts, pos_start) = usage_parts(node);
    let mut usage = std::iter::once(prog.to_owned())
        .chain(parts.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");
    let text_width = width;
    if chars(prefix) + chars(&usage) > text_width {
        let opt_parts = &parts[..pos_start];
        let pos_parts = &parts[pos_start..];
        let prog_len = chars(prog);
        let mut lines: Vec<String>;
        if ((chars(prefix) + prog_len) as f64) <= 0.75 * text_width as f64 {
            let indent = " ".repeat(chars(prefix) + prog_len + 1);
            if !opt_parts.is_empty() {
                let first: Vec<String> = std::iter::once(prog.to_owned())
                    .chain(opt_parts.iter().cloned())
                    .collect();
                lines = get_lines(&first, &indent, Some(prefix), text_width);
                lines.extend(get_lines(pos_parts, &indent, None, text_width));
            } else if !pos_parts.is_empty() {
                let first: Vec<String> = std::iter::once(prog.to_owned())
                    .chain(pos_parts.iter().cloned())
                    .collect();
                lines = get_lines(&first, &indent, Some(prefix), text_width);
            } else {
                lines = vec![prog.to_owned()];
            }
        } else {
            let indent = " ".repeat(chars(prefix));
            let all: Vec<String> = parts.clone();
            lines = get_lines(&all, &indent, None, text_width);
            if lines.len() > 1 {
                lines = get_lines(opt_parts, &indent, None, text_width);
                lines.extend(get_lines(pos_parts, &indent, None, text_width));
            }
            lines.insert(0, prog.to_owned());
        }
        usage = lines.join("\n");
    }
    let rest = usage.strip_prefix(prog).unwrap_or(&usage);
    format!("{prefix}{prog}{rest}\n\n")
}

/// `parser.format_usage()`.
pub fn usage(node: &Node) -> String {
    collapse(format_usage(node, terminal_columns().saturating_sub(2)))
}

fn collapse(text: String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut newlines = 0;
    for c in text.chars() {
        if c == '\n' {
            newlines += 1;
            if newlines <= 2 {
                out.push(c);
            }
        } else {
            newlines = 0;
            out.push(c);
        }
    }
    let trimmed = out.trim_matches('\n');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

/// `formatter.add_text(text); formatter.format_help()`: how argparse prints `--version`.
pub fn text(node: &Node, text: &str) -> String {
    collapse(Formatter::new(node, terminal_columns()).format_text(text))
}

/// `parser.format_help()`: the full `--help` screen of a node.
pub fn help(node: &Node) -> String {
    let mut f = Formatter::new(node, terminal_columns());
    for group in &node.groups {
        f.indent = 2;
        for &i in &group.actions {
            if let Some(a) = f.visible(i) {
                f.measure(a);
            }
        }
    }
    f.indent = 0;
    let mut out = format_usage(node, f.width);
    if let Some(d) = &node.description {
        out.push_str(&f.format_text(d));
    }
    for group in &node.groups {
        f.indent = 2;
        let mut body = String::new();
        if let Some(d) = &group.description {
            body.push_str(&f.format_text(d));
        }
        for &i in &group.actions {
            if let Some(a) = f.visible(i) {
                body.push_str(&f.format_action(a));
            }
        }
        f.indent = 0;
        if !body.is_empty() {
            let heading = group
                .title
                .as_deref()
                .map(|t| format!("{t}:\n"))
                .unwrap_or_default();
            out.push_str(&format!("\n{heading}{body}\n"));
        }
    }
    if let Some(e) = &node.epilog {
        out.push_str(&f.format_text(e));
    }
    collapse(out)
}
