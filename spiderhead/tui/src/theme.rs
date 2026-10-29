//! Color, typography and status-badge contract for the interactive shell.
//!
//! Plain data plus string helpers; the
//! renderer turns style names into ANSI output only when color is enabled.

use crate::error::{TuiError, TuiResult};

/// Status kinds the shell can show. `ok` marks a wired capability, `info` a
/// neutral fact, `warn` a degraded mode, `off` a disabled feature.
pub const STATUS_KINDS: [&str; 4] = ["ok", "warn", "info", "off"];

fn status_style(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "ok" => "bold green",
        "warn" => "bold yellow",
        "info" => "bold #1565C0",
        "off" => "dim",
        _ => return None,
    })
}

fn status_glyph(kind: &str) -> &'static str {
    match kind {
        "ok" => "●",
        "warn" => "▲",
        "info" => "■",
        _ => "○",
    }
}

fn status_ascii(kind: &str) -> &'static str {
    match kind {
        "ok" => "[OK]",
        "warn" => "[!]",
        "info" => "[i]",
        _ => "[-]",
    }
}

/// Resolved display mode for one shell run.
///
/// Style names are kept symbolic so a theme change touches this file only.
#[derive(Debug, Clone)]
pub struct Palette {
    /// Whether styled output is allowed.
    pub color: bool,
    /// Accent style (headings, selected rows, key hints).
    pub accent: String,
    /// Title style.
    pub title: String,
    /// Body style (empty = terminal default).
    pub body: String,
    /// Muted style (secondary text).
    pub muted: String,
    /// Highlight style (selection background).
    pub highlight: String,
}

impl Palette {
    /// `theme.resolve_palette(color=...)`.
    pub fn resolve(color: bool) -> Self {
        Palette {
            color,
            accent: "bold #1565C0".into(),
            title: "bold #f1f5f9".into(),
            body: String::new(),
            muted: "#94a3b8".into(),
            highlight: "bold #f8fafc on #1e3a5f".into(),
        }
    }
}

/// `theme.resolve_palette(color=...)`.
pub fn resolve_palette(color: bool) -> Palette {
    Palette::resolve(color)
}

/// Whether the shell may emit styled output.
///
/// Order: an explicit `--no-color` flag wins, then the `NO_COLOR` convention
/// (any present value, including empty), then `TERM=dumb`, then a non-tty
/// stdout. `env` accepts an explicit map so tests do not mutate the process.
pub fn color_enabled(
    no_color_flag: bool,
    env: Option<&[(String, String)]>,
    stdout_is_tty: bool,
) -> bool {
    if no_color_flag {
        return false;
    }
    let has = |name: &str| -> Option<String> {
        match env {
            Some(map) => map.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()),
            None => std::env::var(name).ok(),
        }
    };
    let no_color_set = match env {
        Some(map) => map.iter().any(|(k, _)| k == "NO_COLOR"),
        None => std::env::var_os("NO_COLOR").is_some(),
    };
    if no_color_set {
        return false;
    }
    if has("TERM").as_deref() == Some("dumb") {
        return false;
    }
    stdout_is_tty
}

/// Badge string; degrades to an ASCII tag when color is off.
pub fn badge(kind: &str, label: &str, palette: &Palette) -> TuiResult<String> {
    let Some(style) = status_style(kind) else {
        return Err(TuiError::BadgeKind(kind.into()));
    };
    if !palette.color {
        return Ok(format!("{} {}", status_ascii(kind), label));
    }
    Ok(format!("[{}]{} {}[/]", style, status_glyph(kind), label))
}
