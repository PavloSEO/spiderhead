//! Code-owned input contracts for public commands.
//!
//! The catalogue deliberately describes accepted inputs, not execution. In particular, a
//! `scan_artifact` is a retained local file: reading it never replays a crawl or promises that a
//! body was retained.
//!
//! The data lives in `direct_*` (public commands, in registry order) and `sf` (the SF family and `mcp`). Both hold data only.

use std::collections::BTreeSet;

mod direct_1;
mod direct_2;
mod direct_3;
mod direct_4;
mod direct_5;
mod direct_6;
mod kind;
mod render_header;
mod sf;
#[cfg(test)]
mod tests;

pub use kind::InputKind;

/// One supported input form: its kind, the handler keyword arguments it names, and notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputForm {
    /// What kind of input this form consumes.
    pub kind: InputKind,
    /// Handler keyword arguments that carry the input.
    pub arguments: &'static [&'static str],
    /// Free-form note; empty when there is nothing to add.
    pub note: &'static str,
    /// Arguments that must be supplied together with this form (empty when independent).
    pub required_with: &'static [&'static str],
}

/// One public command and its alternative supported input forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandContract {
    /// Public command name as typed on the CLI (`sf run`, `mcp`, ...).
    pub command: &'static str,
    /// Handler registry key; `None` for the SF family.
    pub handler: Option<&'static str>,
    /// Alternative supported input forms.
    pub forms: &'static [InputForm],
    /// Command-level note; empty when there is nothing to add.
    pub note: &'static str,
}

/// Builds a form that stands alone.
const fn form(
    kind: InputKind,
    arguments: &'static [&'static str],
    note: &'static str,
) -> InputForm {
    InputForm {
        kind,
        arguments,
        note,
        required_with: &[],
    }
}

/// Builds a form that must be supplied together with `required_with`.
const fn form_req(
    kind: InputKind,
    arguments: &'static [&'static str],
    note: &'static str,
    required_with: &'static [&'static str],
) -> InputForm {
    InputForm {
        kind,
        arguments,
        note,
        required_with,
    }
}

/// Builds one command contract.
const fn command(
    command: &'static str,
    handler: Option<&'static str>,
    forms: &'static [InputForm],
    note: &'static str,
) -> CommandContract {
    CommandContract {
        command,
        handler,
        forms,
        note,
    }
}

const DIRECT_PARTS: &[&[CommandContract]] = &[
    direct_1::COMMANDS,
    direct_2::COMMANDS,
    direct_3::COMMANDS,
    direct_4::COMMANDS,
    direct_5::COMMANDS,
    direct_6::COMMANDS,
];

/// Direct CLI command contracts, in catalogue order.
pub fn command_contracts() -> impl Iterator<Item = &'static CommandContract> {
    DIRECT_PARTS.iter().copied().flat_map(|part| part.iter())
}

/// SF family (`sf ...`) and `mcp` contracts, in catalogue order.
pub fn sf_contracts() -> impl Iterator<Item = &'static CommandContract> {
    sf::COMMANDS.iter()
}

/// Every contract: direct commands first, then the SF family.
pub fn contracts() -> impl Iterator<Item = &'static CommandContract> {
    command_contracts().chain(sf_contracts())
}

/// Public commands missing from, or stale in, this catalogue.
///
/// Returns `(missing, stale)`. `commands` are the direct CLI command names and `sf_subcommands`
/// are the names after `sf `; `mcp` is always expected. The caller supplies the interface
/// registries, so this module never imports CLI or server code.
pub fn coverage_gaps(
    commands: &[&str],
    sf_subcommands: &[&str],
) -> (BTreeSet<String>, BTreeSet<String>) {
    let expected: BTreeSet<String> = commands
        .iter()
        .map(|name| (*name).to_owned())
        .chain(sf_subcommands.iter().map(|name| format!("sf {name}")))
        .chain(std::iter::once("mcp".to_owned()))
        .collect();
    let catalogued: BTreeSet<String> = contracts().map(|c| c.command.to_owned()).collect();
    (
        expected.difference(&catalogued).cloned().collect(),
        catalogued.difference(&expected).cloned().collect(),
    )
}

/// Commands whose `input_path` argument is shown as `scan` in the reference.
const SCAN_COMMANDS: &[&str] = &[
    "scan-inspect",
    "scan-archive",
    "scan-url-query",
    "scan-link-inspect",
    "scan-status",
    "scan-rendered-routes",
    "scan-snapshot",
    "scan-pin",
    "scan-reanalyze",
    "scan-graph",
    "scan-sitemap-generate",
];

/// Renders the checked-in public input reference (`docs/INPUTS.md`).
pub fn render_markdown() -> String {
    let mut lines: Vec<String> = render_header::RENDER_HEADER
        .iter()
        .map(|line| (*line).to_owned())
        .collect();
    for contract in contracts() {
        let forms = contract
            .forms
            .iter()
            .map(|form| render_form(contract.command, form))
            .collect::<Vec<_>>()
            .join("<br>");
        let notes = std::iter::once(contract.note)
            .chain(contract.forms.iter().map(|form| form.note))
            .filter(|note| !note.is_empty())
            .collect::<Vec<_>>()
            .join("; ");
        let notes = if notes.is_empty() {
            "—"
        } else {
            notes.as_str()
        };
        lines.push(format!("| `{}` | {forms} | {notes} |", contract.command));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// Renders one form cell: label, the arguments it names, and any required partners.
fn render_form(command: &str, form: &InputForm) -> String {
    let label = form.kind.label();
    if form.arguments.is_empty() {
        return label.to_owned();
    }
    let arguments = form
        .arguments
        .iter()
        .map(|argument| {
            if SCAN_COMMANDS.contains(&command) && *argument == "input_path" {
                "scan"
            } else {
                argument
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut cell = format!("{label} (`{arguments}`)");
    if !form.required_with.is_empty() {
        cell.push_str(&format!("; requires `{}`", form.required_with.join(", ")));
    }
    cell
}
