//! The `spiderhead` binary: runs the CLI surface and flushes its output.

use std::io::Write;

use spiderhead_cli::{run, Format, Output, Registry};

fn domain_registry() -> Registry {
    spiderhead_cli::registry()
}

fn main() -> anyhow::Result<std::process::ExitCode> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut out = Output::new(Format::Json);
    let code = run(&argv, &domain_registry(), &mut out)?;
    std::io::stdout().write_all(out.stdout.as_bytes())?;
    std::io::stderr().write_all(out.stderr.as_bytes())?;
    Ok(code.into())
}
