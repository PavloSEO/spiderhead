//! The command surface as clap declarations: every command, flag, choice and
//! default of the `spiderhead` CLI, in one tree built with the builder API.
//!
//! [`command`] assembles the whole tree; parsing and the mapping back to
//! [`crate::ParsedCommand`] live in [`crate::parse`]. The declarations are the
//! single source of truth for names, types and defaults — help screens, usage
//! errors and exit codes come from clap itself.

mod base;
mod crawl;
mod project;
mod scan;
mod semantics;
mod sf;
mod sources;

use std::ffi::OsStr;

use clap::builder::{Arg, ArgAction, Command, PossibleValue, TypedValueParser};
use clap::error::{Error as ClapError, ErrorKind};

use crate::convert::{self, Kind};

/// Terminal width help and usage wrap at: `$COLUMNS` when positive, else 80 —
/// the same resolution the previous surface used.
pub fn width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&c| c > 0)
        .unwrap_or(80)
}

/// The whole command tree, rooted at `spiderhead`.
pub fn command() -> Command {
    // `sf` reports its version like the root does, with the same long-only spelling.
    let mut buckets = [
        base::commands(),
        crawl::commands(),
        scan::commands(),
        project::commands(),
        sources::commands(),
        sf::commands(),
        semantics::commands(),
    ];
    for bucket in buckets.iter_mut() {
        for cmd in bucket.iter_mut() {
            if cmd.get_name() == "sf" {
                *cmd = cmd.clone().disable_version_flag(true).arg(version_flag());
            }
        }
    }
    let mut root = Command::new("spiderhead")
        .version(crate::TOOLKIT_VERSION)
        .about("Local-first SEO toolkit: CLI and MCP server.")
        .disable_help_subcommand(true)
        .disable_version_flag(true)
        .term_width(width())
        .arg(version_flag());
    for cmds in buckets {
        root = root.subcommands(cmds);
    }
    root
}

/// `--version` without the `-V` short: the surface keeps one spelling.
fn version_flag() -> Arg {
    Arg::new("version")
        .long("version")
        .action(ArgAction::Version)
        .help("show the version and exit")
}

/// A command with its listing label; every command wraps help at [`width`]
/// and lets a repeated flag override an earlier one.
#[allow(non_snake_case)]
fn c(name: &'static str, about: &'static str) -> Command {
    Command::new(name)
        .about(about)
        .infer_long_args(true)
        .args_override_self(true)
        .term_width(width())
}

/// One value-taking option: `--url URL`, `--limit N`, ...
#[allow(non_snake_case)]
fn v(dest: &'static str, long: &'static str, help: &'static str, py: Py) -> Arg {
    Arg::new(dest)
        .long(long.trim_start_matches('-'))
        .value_parser(py)
        .help(help)
        .allow_negative_numbers(true)
}

/// A switch: `--json` stays absent-false in the parsed values.
#[allow(non_snake_case)]
fn sw(dest: &'static str, long: &'static str, help: &'static str) -> Arg {
    Arg::new(dest)
        .long(long.trim_start_matches('-'))
        .action(ArgAction::SetTrue)
        .default_value("false")
        .help(help)
}

/// A switch whose declared default was null, not false (`sf run --live-recheck`).
#[allow(non_snake_case)]
fn swx(dest: &'static str, long: &'static str, help: &'static str) -> Arg {
    Arg::new(dest)
        .long(long.trim_start_matches('-'))
        .action(ArgAction::SetTrue)
        .help(help)
}

/// The negative half of a `--x` / `--no-x` switch pair (`--no-live-recheck`).
#[allow(non_snake_case)]
fn swf(dest: &'static str, long: &'static str, help: &'static str) -> Arg {
    Arg::new(dest)
        .long(long.trim_start_matches('-'))
        .action(ArgAction::SetFalse)
        .help(help)
}

/// A repeatable option collecting a list (`--set PATH=VALUE`).
#[allow(non_snake_case)]
fn lst(dest: &'static str, long: &'static str, help: &'static str) -> Arg {
    Arg::new(dest)
        .long(long.trim_start_matches('-'))
        .action(ArgAction::Append)
        .value_parser(Py::TEXT)
        .help(help)
        .allow_negative_numbers(true)
}

/// A required positional.
#[allow(non_snake_case)]
fn pos(dest: &'static str, help: &'static str) -> Arg {
    Arg::new(dest)
        .required(true)
        .value_parser(Py::TEXT)
        .help(help)
        .allow_negative_numbers(true)
}

/// An optional positional.
#[allow(non_snake_case)]
fn poso(dest: &'static str, help: &'static str) -> Arg {
    Arg::new(dest)
        .value_parser(Py::TEXT)
        .help(help)
        .allow_negative_numbers(true)
}

/// A positional with restricted values (`mcp status`).
#[allow(non_snake_case)]
fn posc(dest: &'static str, help: &'static str, py: Py) -> Arg {
    Arg::new(dest)
        .value_parser(py)
        .help(help)
        .allow_negative_numbers(true)
}

/// Chained tweaks the generated declarations use.
pub(crate) trait ArgExt: Sized {
    /// Rename an argument so its id (the parsed-values key) differs from the long flag.
    fn id_(self, id: &'static str) -> Self;
    /// Attach a short spelling.
    fn sh(self, short: char) -> Self;
    /// A display metavar (`--set PATH=VALUE`).
    fn vm(self, value_name: &'static str) -> Self;
    /// Hide from help and usage while staying parseable.
    fn hid(self) -> Self;
    /// Require the flag.
    fn req(self) -> Self;
    /// The value assumed when the flag is absent (already in parser form).
    fn def(self, value: &'static str) -> Self;
    /// A JSON-encoded default for a repeatable option (`[]`).
    fn defj(self, value: &'static str) -> Self;
    /// An extra long spelling of the same argument, shown in help like the
    /// primary entry it shares a dest with.
    fn al(self, long: &'static str) -> Self;
}

impl ArgExt for Arg {
    fn id_(self, id: &'static str) -> Self {
        self.id(id)
    }

    fn sh(self, short: char) -> Self {
        self.short(short)
    }

    fn vm(self, value_name: &'static str) -> Self {
        self.value_name(value_name)
    }

    fn hid(self) -> Self {
        self.hide(true)
    }

    fn req(self) -> Self {
        self.required(true)
    }

    fn def(self, value: &'static str) -> Self {
        self.default_value(value)
    }

    fn defj(self, value: &'static str) -> Self {
        self.default_value(value)
    }

    fn al(self, long: &'static str) -> Self {
        self.visible_alias(long.trim_start_matches('-'))
    }
}

/// A flag value type with its reference conversion and, when set, the
/// accepted choices.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Py {
    kind: Kind,
    choices: &'static [&'static str],
    int_choices: &'static [&'static str],
}

/// The converted value clap hands back: already a handler-shaped JSON value.
#[derive(Debug, Clone)]
pub(crate) struct PyVal {
    pub value: serde_json::Value,
}

#[allow(non_snake_case)]
impl Py {
    pub const TEXT: Py = Py {
        kind: Kind::Text,
        choices: &[],
        int_choices: &[],
    };
    pub const INT: Py = Py {
        kind: Kind::Int,
        choices: &[],
        int_choices: &[],
    };
    pub const FLOAT: Py = Py {
        kind: Kind::Float,
        choices: &[],
        int_choices: &[],
    };
    pub const JSON: Py = Py {
        kind: Kind::JsonList,
        choices: &[],
        int_choices: &[],
    };
    pub const PORT: Py = Py {
        kind: Kind::Port,
        choices: &[],
        int_choices: &[],
    };

    /// Restrict to these strings; clap rejects foreign values for us (exit
    /// code 2) and lists them in help.
    pub const fn ch(mut self, choices: &'static [&'static str]) -> Py {
        self.choices = choices;
        self
    }

    /// Restrict an integer flag to these numbers.
    pub const fn chn(mut self, choices: &'static [&'static str]) -> Py {
        self.int_choices = choices;
        self
    }
}

impl TypedValueParser for Py {
    type Value = PyVal;

    fn parse_ref(
        &self,
        _cmd: &Command,
        arg: Option<&Arg>,
        value: &OsStr,
    ) -> Result<PyVal, ClapError> {
        let arg = arg.expect("value parser is only used on arguments");
        let raw = value.to_string_lossy().into_owned();
        let value = convert::convert_kind(self.kind, &raw).map_err(|message| {
            ClapError::raw(
                ErrorKind::InvalidValue,
                format!("invalid value {raw:?} for {arg}: {message}\n"),
            )
        })?;
        // clap only renders `possible_values`; the parser owns the rejection,
        // so a foreign choice exits 2 right here, compared after conversion
        // like the reference did.
        if !self.int_choices.is_empty() {
            let hit = value
                .as_i64()
                .is_some_and(|n| self.int_choices.iter().any(|c| c.parse::<i64>() == Ok(n)));
            if !hit {
                return Err(ClapError::raw(
                    ErrorKind::InvalidValue,
                    format!(
                        "invalid value {raw:?} for {arg}: expected one of {}\n",
                        self.int_choices.join(", ")
                    ),
                ));
            }
        } else if !self.choices.is_empty() {
            if let serde_json::Value::String(text) = &value {
                if !self.choices.contains(&text.as_str()) {
                    let list = self
                        .choices
                        .iter()
                        .map(|c| format!("{c:?}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(ClapError::raw(
                        ErrorKind::InvalidValue,
                        format!("invalid value {text:?} for {arg}: choose from {list}\n"),
                    ));
                }
            }
        }
        Ok(PyVal { value })
    }

    /// Advertised choices: clap validates against them and shows them in help.
    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue>>> {
        let choices = if !self.choices.is_empty() {
            self.choices
        } else if !self.int_choices.is_empty() {
            self.int_choices
        } else {
            return None;
        };
        Some(Box::new(choices.iter().map(|c| PossibleValue::new(*c))))
    }
}

/// The selector dest each level's subcommand selection lands under; the
/// handler layer drops these selector keys from its kwargs.
pub(crate) fn parser_dest(path: &[String]) -> &'static str {
    match path.last().map(String::as_str) {
        Some("scan") => "scan_command",
        Some("logs") => "logs_command",
        Some("project") => "project_command",
        Some("skill") => "skill_command",
        Some("scenario") => "scenario_command",
        Some("crawl-profile") => "crawl_profile_command",
        Some("semantics") => "stage",
        // root and `sf` select under `command`.
        _ => "command",
    }
}
