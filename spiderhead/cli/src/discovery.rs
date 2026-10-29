//! Project autodiscovery for the command line, plus the `init` and `mcp` commands that sit
//! outside the argparse-shaped tree.

use serde_json::Value;

use crate::{ExitCode, Output, ParsedCommand};

const PROJECT_FLAGS: [&str; 2] = ["--project", "--directory"];

/// Commands whose directory names something to create.
fn creates(path: &[String]) -> bool {
    matches!(path.join(" ").as_str(), "project new" | "project-new")
}

/// Flags to append when a parse failed only because the project argument was missing and a
/// project is discoverable from the working directory.
pub fn missing_project_flags(code: i32, stderr: &str) -> Option<Vec<String>> {
    if code != 2 {
        return None;
    }
    let marker = "the following arguments are required: ";
    let line = stderr.lines().find_map(|l| l.split_once(marker))?.1;
    let names: Vec<&str> = line.split(", ").map(str::trim).collect();
    if !names.iter().all(|n| PROJECT_FLAGS.contains(n)) {
        return None;
    }
    let found = spiderhead_core::project_root::discover_here()?;
    let value = found.to_string_lossy().into_owned();
    Some(
        names
            .into_iter()
            .flat_map(|n| [n.to_owned(), value.clone()])
            .collect(),
    )
}

/// Fill an unset `project` / `directory` option from the working directory.
pub fn fill_project(cmd: &mut ParsedCommand) {
    if creates(&cmd.path) {
        return;
    }
    let unset = |cmd: &ParsedCommand, key: &str| cmd.values.get(key).is_some_and(Value::is_null);
    let keys: Vec<&str> = ["project", "directory"]
        .into_iter()
        .filter(|k| unset(cmd, k))
        .collect();
    // Commands that take both keep them distinct: fill only an unambiguous single one.
    if keys.len() != 1 || cmd.values.contains_key("project") && cmd.values.contains_key("directory")
    {
        return;
    }
    if let Some(found) = spiderhead_core::project_root::discover_here() {
        cmd.values.insert(
            keys[0].to_owned(),
            Value::String(found.to_string_lossy().into_owned()),
        );
    }
}

/// `spiderhead mcp` and `spiderhead init`, handled before argparse.
pub fn builtin(argv: &[String], out: &mut Output) -> Option<ExitCode> {
    match argv.first().map(String::as_str) {
        // Bare `spiderhead mcp` (optionally `--transport stdio`) starts the stdio server;
        // any other `mcp` invocation keeps the registered command's parsing and help.
        Some("mcp") if matches!(&argv[1..], [] | [_, _] if argv[1] == "--transport" && argv[2] == "stdio") => {
            Some(ExitCode(spiderhead_mcp::stdio::run(
                "spiderhead mcp",
                argv[1..].iter().cloned(),
            )))
        }
        Some("init") => Some(init(&argv[1..], out)),
        _ => None,
    }
}

const INIT_USAGE: &str = "usage: spiderhead init [--site URL] [--directory DIR]\n\n\
Create a project in DIR/seo (DIR defaults to the current directory).";

fn init(args: &[String], out: &mut Output) -> ExitCode {
    let mut site = None;
    let mut directory = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--site" => site = it.next().cloned(),
            "--directory" => directory = it.next().cloned(),
            "-h" | "--help" => {
                out.println(INIT_USAGE);
                return ExitCode::SUCCESS;
            }
            other => {
                out.eprintln(INIT_USAGE);
                out.eprintln(&format!("spiderhead init: unknown argument: {other}"));
                return ExitCode::USAGE;
            }
        }
    }
    let dir = directory
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    match spiderhead_projects::workspace::init_project(&dir, site.as_deref()) {
        Ok(created) => {
            out.println(&serde_json::to_string_pretty(&created).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Err(error) => {
            out.eprintln(&format!("error: {error}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Format;

    #[test]
    fn only_project_flags_trigger_a_retry() {
        assert!(
            missing_project_flags(2, "error: the following arguments are required: --out")
                .is_none()
        );
        assert!(missing_project_flags(0, "").is_none());
    }

    #[test]
    fn init_creates_the_seo_folder_and_a_gitignore() {
        let dir = std::env::temp_dir().join(format!("sh-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut out = Output::new(Format::Json);
        let argv: Vec<String> = ["init", "--site", "https://example.com/", "--directory"]
            .iter()
            .map(|s| s.to_string())
            .chain([dir.to_string_lossy().into_owned()])
            .collect();
        let code = builtin(&argv, &mut out).unwrap();
        assert_eq!(code, ExitCode::SUCCESS, "{}", out.stderr);
        let manifest = std::fs::read_to_string(dir.join("seo/project.json")).unwrap();
        assert!(
            !manifest.contains(dir.to_str().unwrap()),
            "paths stay relative"
        );
        assert!(std::fs::read_to_string(dir.join("seo/.gitignore"))
            .unwrap()
            .contains("scans/"));
        assert!(spiderhead_core::project_root::discover(&dir.join("seo")).is_some());
    }
}
