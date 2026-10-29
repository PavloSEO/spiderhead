//! The command table: one description of every CLI command's flags and handler mapping.
//!
//! Pure data plus a registry, so `core` does not depend on the CLI layer; the CLI domain modules fill the table on import.
//! Flag strings, help text and kwarg names are the observable interface and are defined byte-for-byte in the domain tables.
//!
//! Registration is atomic: a rejected batch leaves the table unchanged, so no spec from that batch is kept.
//! The invariants that [`ArgSpec::check`] enforces run before [`CommandTable::register`] inserts anything.

use std::sync::{Mutex, MutexGuard};

use thiserror::Error;

/// How a flag's string becomes a value. The set of kinds is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Plain string.
    Str,
    /// Integer.
    Int,
    /// Floating-point number.
    Float,
    /// Boolean switch (`store_true`).
    Flag,
    /// JSON object.
    Json,
    /// JSON list.
    JsonList,
    /// Comma-separated list.
    List,
    /// One of [`ArgSpec::choices`].
    Choice,
    /// One of the named site lambdas, identified by [`ArgSpec::lambda_id`].
    Lambda,
}

/// A parser default, const-constructible so tables can live in `static` data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DefaultValue {
    /// No default.
    None,
    /// Boolean default.
    Bool(bool),
    /// Integer default.
    Int(i64),
    /// Floating-point default.
    Float(f64),
    /// String default.
    Str(&'static str),
}

/// One CLI flag: how it is declared and how it reaches the handler.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArgSpec {
    /// Handler kwarg name (equal to the argparse dest unless [`ArgSpec::dest`] is given).
    pub kwarg: &'static str,
    /// Primary option string, e.g. `--scan-uuid`.
    pub flag: &'static str,
    /// How the flag's string becomes a value.
    pub kind: Kind,
    /// Byte-exact help text.
    pub help: &'static str,
    /// Argparse dest when it differs from the kwarg.
    pub dest: Option<&'static str>,
    /// Parser default.
    pub default: DefaultValue,
    /// `true`: the kwarg always reaches the handler; `false`: only when the flag was given.
    pub always: bool,
    /// Whether the flag is required.
    pub required: bool,
    /// Accepted values for [`Kind::Choice`].
    pub choices: &'static [&'static str],
    /// Metavar, only when the CLI sets one explicitly.
    pub metavar: Option<&'static str>,
    /// Wrapped in the stdin/`@file` source reader.
    pub source: bool,
    /// Extra option strings on the same dest.
    pub aliases: &'static [&'static str],
    /// `action="append"`.
    pub append: bool,
    /// Registry key of the named site lambda, for [`Kind::Lambda`].
    pub lambda_id: Option<&'static str>,
}

impl ArgSpec {
    /// A flag with the given kwarg, flag and kind; every other field at its default.
    pub const fn new(kwarg: &'static str, flag: &'static str, kind: Kind) -> Self {
        Self {
            kwarg,
            flag,
            kind,
            help: "",
            dest: None,
            default: DefaultValue::None,
            always: false,
            required: false,
            choices: &[],
            metavar: None,
            source: false,
            aliases: &[],
            append: false,
            lambda_id: None,
        }
    }

    /// Checks the invariants every spec must satisfy.
    pub fn check(&self) -> Result<(), TableError> {
        if self.kind == Kind::Choice && self.choices.is_empty() {
            return Err(TableError::Invalid(format!(
                "{}: kind=choice requires choices",
                self.flag
            )));
        }
        if self.kind == Kind::Lambda && self.lambda_id.is_none_or(str::is_empty) {
            return Err(TableError::Invalid(format!(
                "{}: kind=lambda requires lambda_id",
                self.flag
            )));
        }
        Ok(())
    }
}

/// One CLI command: its handler and its flags, in registration order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommandSpec {
    /// Full command name, e.g. `scan-url-query`.
    pub name: &'static str,
    /// Handler function name, e.g. `scan_url_query`.
    pub handler: &'static str,
    /// Flags in registration order; `--help` output and the usage line depend on it.
    pub args: &'static [ArgSpec],
    /// Domain function that builds the handler kwargs for non-standard mappings.
    pub kwargs_post: Option<&'static str>,
    /// CLI module short name, e.g. `scan`.
    pub domain: &'static str,
    /// Subparser help for namespace forms.
    pub description: Option<&'static str>,
    /// Epilog text, when the command has one.
    pub epilog: Option<&'static str>,
    /// Registered but not advertised in `--help` lists.
    pub hidden: bool,
}

impl CommandSpec {
    /// A command with the given name and handler and no flags.
    pub const fn new(name: &'static str, handler: &'static str, domain: &'static str) -> Self {
        Self {
            name,
            handler,
            args: &[],
            kwargs_post: None,
            domain,
            description: None,
            epilog: None,
            hidden: false,
        }
    }
}

/// Errors raised while registering command specs.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TableError {
    /// A spec or flag broke one of its invariants.
    #[error("{0}")]
    Invalid(String),
    /// A command name was registered twice.
    #[error("duplicate CommandSpec: {0}")]
    Duplicate(String),
}

/// An ordered table of command specs, keyed by command name.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct CommandTable {
    specs: Vec<CommandSpec>,
}

impl CommandTable {
    /// An empty table.
    pub const fn new() -> Self {
        Self { specs: Vec::new() }
    }

    /// Add specs in order. The batch is validated first: nothing is inserted unless every spec
    /// is valid and no name repeats the table or the batch.
    pub fn register(&mut self, specs: &[CommandSpec]) -> Result<(), TableError> {
        for (index, spec) in specs.iter().enumerate() {
            let duplicate_in_table = self.specs.iter().any(|known| known.name == spec.name);
            let duplicate_in_batch = specs[..index].iter().any(|known| known.name == spec.name);
            if duplicate_in_table || duplicate_in_batch {
                return Err(TableError::Duplicate(spec.name.to_owned()));
            }
            for arg in spec.args {
                arg.check()?;
            }
        }
        self.specs.extend_from_slice(specs);
        Ok(())
    }

    /// The spec registered under `name`.
    pub fn get(&self, name: &str) -> Option<&CommandSpec> {
        self.specs.iter().find(|spec| spec.name == name)
    }

    /// Every spec, in registration order.
    pub fn all(&self) -> &[CommandSpec] {
        &self.specs
    }

    /// Every command name, in registration order.
    pub fn names(&self) -> Vec<&'static str> {
        self.specs.iter().map(|spec| spec.name).collect()
    }
}

static GLOBAL: Mutex<CommandTable> = Mutex::new(CommandTable::new());

/// The process-wide table the CLI domain modules register into.
pub fn global() -> MutexGuard<'static, CommandTable> {
    GLOBAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_FLAG: ArgSpec = ArgSpec {
        help: "Targets",
        ..ArgSpec::new("targets", "--targets", Kind::List)
    };
    const BAD_CHOICE: ArgSpec = ArgSpec::new("mode", "--mode", Kind::Choice);
    const BAD_LAMBDA: ArgSpec = ArgSpec::new("site", "--site", Kind::Lambda);
    const ARGS: &[ArgSpec] = &[LIST_FLAG];

    const PARSE: CommandSpec = CommandSpec {
        args: ARGS,
        ..CommandSpec::new("parse", "parse", "parse")
    };
    const CRAWL: CommandSpec = CommandSpec::new("crawl", "crawl_site", "crawl");

    // Registry behaviour: register, get and names.
    #[test]
    fn registers_in_order_and_looks_up_by_name() {
        let mut table = CommandTable::new();
        table.register(&[PARSE, CRAWL]).unwrap();
        assert_eq!(table.names(), vec!["parse", "crawl"]);
        assert_eq!(
            table.get("crawl").map(|spec| spec.handler),
            Some("crawl_site")
        );
        assert!(table.get("missing").is_none());
    }

    #[test]
    fn duplicate_names_are_rejected_and_leave_the_table_unchanged() {
        let mut table = CommandTable::new();
        table.register(&[PARSE]).unwrap();
        let error = table.register(&[CRAWL, PARSE]).unwrap_err();
        assert_eq!(error, TableError::Duplicate("parse".into()));
        assert_eq!(table.names(), vec!["parse"]);
        let twice = table.register(&[CRAWL, CRAWL]).unwrap_err();
        assert_eq!(twice, TableError::Duplicate("crawl".into()));
    }

    // Each ArgSpec invariant rejects an invalid spec.
    #[test]
    fn choice_and_lambda_flags_need_their_key() {
        assert!(BAD_CHOICE.check().is_err());
        assert!(BAD_LAMBDA.check().is_err());
        let ok = ArgSpec {
            choices: &["a"],
            ..BAD_CHOICE
        };
        assert!(ok.check().is_ok());
        let keyed = ArgSpec {
            lambda_id: Some("site_rules"),
            ..BAD_LAMBDA
        };
        assert!(keyed.check().is_ok());
    }

    #[test]
    fn an_invalid_flag_rejects_the_whole_batch() {
        let mut table = CommandTable::new();
        const BROKEN_ARGS: &[ArgSpec] = &[BAD_CHOICE];
        const BROKEN: CommandSpec = CommandSpec {
            args: BROKEN_ARGS,
            ..CommandSpec::new("broken", "broken", "misc")
        };
        assert!(table.register(&[CRAWL, BROKEN]).is_err());
        assert!(table.all().is_empty());
    }
}
