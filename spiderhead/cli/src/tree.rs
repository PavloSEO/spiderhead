//! The command tree as data: `data/cli_tree.json`, generated from the reference argparse tree.
//!
//! The JSON is embedded with `include_str!` and parsed once. Nothing here knows about a
//! particular command; the help formatter and the argument parser both walk these structs.

use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::Value;

/// argparse's `SUPPRESS` marker as it appears in the captured tree.
pub const SUPPRESS: &str = "==SUPPRESS==";

const TREE_JSON: &str = include_str!("../data/cli_tree.json");

/// How many values an argument takes (argparse `nargs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nargs {
    /// Exactly one (`nargs=None`).
    One,
    /// `?`
    Optional,
    /// `*`
    ZeroOrMore,
    /// `+`
    OneOrMore,
    /// `...` as `argparse.REMAINDER`.
    Remainder,
    /// `A...` (a sub-command followed by anything).
    Parser,
    /// Takes nothing and stores nothing in the namespace.
    Suppress,
    /// Exactly this many (0 for flags).
    Count(usize),
}

/// One action as captured from `parser._actions`.
#[derive(Debug, Clone, Deserialize)]
pub struct Action {
    /// Action class: `store`, `store_true`, `append`, `help`, `version`, `parsers`, ...
    pub kind: String,
    /// Option strings; empty for positionals.
    pub option_strings: Vec<String>,
    /// Destination name.
    pub dest: String,
    #[serde(default)]
    nargs: Value,
    /// `const`, as JSON.
    #[serde(rename = "const", default)]
    pub constant: Value,
    /// Default, as JSON.
    #[serde(default)]
    pub default: Value,
    /// Name of the `type=` callable, when any.
    #[serde(rename = "type", default)]
    pub type_name: Option<String>,
    /// Accepted values, when restricted.
    #[serde(default)]
    pub choices: Option<Vec<Value>>,
    /// Whether the argument must be given.
    #[serde(default)]
    pub required: bool,
    /// Raw `help=` text.
    #[serde(default)]
    pub help: Option<String>,
    /// `help` with `%(...)s` expanded by argparse, when it contained `%`.
    #[serde(default)]
    pub help_expanded: Option<String>,
    /// `metavar=`: a string or a list of strings.
    #[serde(default)]
    pub metavar: Value,
    /// Version text for `version` actions.
    #[serde(default)]
    pub version: Option<String>,
    /// Sub-parsers declared by a `parsers` action, in registration order.
    #[serde(default)]
    pub parsers: Vec<SubParser>,
}

/// One entry of a sub-parsers action.
#[derive(Debug, Clone, Deserialize)]
pub struct SubParser {
    /// Primary name.
    pub name: String,
    /// Alternative names.
    pub aliases: Vec<String>,
    /// Help shown in the parent's listing.
    pub help: Option<String>,
    /// Whether the parent's help lists it (it was added with `help=`).
    pub listed: bool,
}

impl Action {
    /// Decoded `nargs`.
    pub fn nargs(&self) -> Nargs {
        match &self.nargs {
            Value::Null => Nargs::One,
            Value::Number(n) => Nargs::Count(n.as_u64().unwrap_or(0) as usize),
            Value::String(s) => match s.as_str() {
                "?" => Nargs::Optional,
                "*" => Nargs::ZeroOrMore,
                "+" => Nargs::OneOrMore,
                "..." => Nargs::Remainder,
                "A..." => Nargs::Parser,
                _ => Nargs::Suppress,
            },
            _ => Nargs::One,
        }
    }

    /// `help=SUPPRESS`: hidden from help and usage.
    pub fn hidden(&self) -> bool {
        self.help.as_deref() == Some(SUPPRESS)
    }

    /// Choices rendered the way argparse prints them (`str(choice)`).
    pub fn choice_strings(&self) -> Option<Vec<String>> {
        self.choices.as_ref().map(|c| {
            c.iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect()
        })
    }

    /// `metavar` as a tuple of strings, when one was set.
    pub fn metavar_tuple(&self) -> Option<Vec<String>> {
        match &self.metavar {
            Value::String(s) => Some(vec![s.clone()]),
            Value::Array(a) => Some(
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
            ),
            _ => None,
        }
    }

    /// Whether `metavar` is a tuple rather than a plain string.
    pub fn metavar_is_tuple(&self) -> bool {
        self.metavar.is_array()
    }
}

/// An argument group (`positional arguments`, `options`, or a custom one).
#[derive(Debug, Clone, Deserialize)]
pub struct Group {
    /// Section heading.
    pub title: Option<String>,
    /// Section description.
    pub description: Option<String>,
    /// Indices into [`Node::actions`].
    pub actions: Vec<usize>,
}

/// A mutually exclusive group.
#[derive(Debug, Clone, Deserialize)]
pub struct Mutex {
    /// One member must be given.
    pub required: bool,
    /// Indices into [`Node::actions`].
    pub actions: Vec<usize>,
}

/// One parser of the tree: the root, a command, or a sub-command.
#[derive(Debug, Clone, Deserialize)]
pub struct Node {
    /// Names from the root, e.g. `["scan", "list"]`.
    pub path: Vec<String>,
    /// `prog` as argparse computed it.
    pub prog: String,
    /// Description text.
    pub description: Option<String>,
    /// Epilog text.
    pub epilog: Option<String>,
    /// Explicit usage string, when set.
    pub usage: Option<String>,
    /// Formatter class name (`HelpFormatter` or `RawDescriptionHelpFormatter`).
    pub formatter: String,
    /// Parser class name; `_SuggestingParser` rewrites an invalid top-level command.
    pub subparser_class: String,
    /// Whether `--opt` prefixes are accepted.
    pub allow_abbrev: bool,
    /// Actions in registration order.
    pub actions: Vec<Action>,
    /// Argument groups in display order.
    pub groups: Vec<Group>,
    /// Mutually exclusive groups.
    pub mutex: Vec<Mutex>,
    /// Sub-parsers.
    pub children: Vec<Node>,
    /// `sf` / `semantics` when this node is owned by a delegated parser.
    #[serde(default)]
    pub delegate: Option<String>,
    /// Handler kwargs accepted through `--input` beyond the declared flags
    /// (`param name -> schema`); the shared handler layer admits them even
    /// though argparse never lists them.
    #[serde(default)]
    pub input_extensions: Option<serde_json::Map<String, Value>>,
}

impl Node {
    /// The child registered under `name` or one of its aliases.
    pub fn child(&self, name: &str) -> Option<&Node> {
        let sub = self.actions.iter().find(|a| a.kind == "parsers")?;
        let hit = sub
            .parsers
            .iter()
            .find(|p| p.name == name || p.aliases.iter().any(|a| a == name))?;
        self.children
            .iter()
            .find(|c| c.path.last() == Some(&hit.name))
    }

    /// Every sub-command name and alias, in registration order.
    pub fn child_names(&self) -> Vec<&str> {
        self.actions
            .iter()
            .filter(|a| a.kind == "parsers")
            .flat_map(|a| a.parsers.iter())
            .flat_map(|p| {
                std::iter::once(p.name.as_str()).chain(p.aliases.iter().map(String::as_str))
            })
            .collect()
    }

    /// Option string -> action index, in registration order.
    pub fn option_strings(&self) -> Vec<(&str, usize)> {
        self.actions
            .iter()
            .enumerate()
            .flat_map(|(i, a)| a.option_strings.iter().map(move |s| (s.as_str(), i)))
            .collect()
    }

    /// Walk this node and all descendants.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a Node>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }
}

/// The whole tree plus the toolkit version the reference original reported.
#[derive(Debug, Clone, Deserialize)]
pub struct Tree {
    /// Version placeholder from the capture (`{VERSION}`).
    pub python_version: String,
    /// Root parser (`spiderhead`).
    pub root: Node,
}

static TREE: LazyLock<Result<Tree, String>> =
    LazyLock::new(|| serde_json::from_str(TREE_JSON).map_err(|e| e.to_string()));

/// The embedded tree.
///
/// # Errors
/// Returns the parse error if the embedded JSON is malformed (a build-time bug).
pub fn tree() -> Result<&'static Tree, crate::CliError> {
    TREE.as_ref().map_err(|e| crate::CliError::Tree(e.clone()))
}
