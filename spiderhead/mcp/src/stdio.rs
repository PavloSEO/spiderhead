//! The stdio server entry shared by `spiderhead mcp` and the `spiderhead-mcp` alias.

use std::io::BufReader;
use std::str::FromStr;

use crate::profile::{Profile, UnknownProfile};
use crate::server::Server;
use crate::transport;

/// Usage text, with `prog` as the command the user typed.
pub fn usage(prog: &str) -> String {
    format!("usage: {prog} [--profile {{audit|full|infra|quick-check|router}}]")
}

/// Parse `--profile <name>` / `--help`; `Ok(None)` means help was printed.
pub fn parse_profile(
    prog: &str,
    args: impl Iterator<Item = String>,
) -> Result<Option<Profile>, UnknownProfile> {
    let mut profile = Profile::Full;
    let mut iterator = args;
    while let Some(arg) = iterator.next() {
        match arg.as_str() {
            "--profile" => match iterator.next().as_deref().map(Profile::from_str) {
                Some(Ok(parsed)) => profile = parsed,
                Some(Err(error)) => return Err(error),
                None => return Err(UnknownProfile),
            },
            "--help" | "-h" => {
                println!("{}", usage(prog));
                return Ok(None);
            }
            other => {
                eprintln!("unknown argument: {other}");
                return Err(UnknownProfile);
            }
        }
    }
    Ok(Some(profile))
}

/// Serve MCP over stdin/stdout until stdin closes; returns the process exit code.
pub fn run(prog: &str, args: impl Iterator<Item = String>) -> u8 {
    match parse_profile(prog, args) {
        Ok(Some(profile)) => {
            let server = Server::new(profile);
            match transport::run(BufReader::new(std::io::stdin()), std::io::stdout(), &server) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("{prog}: {error}");
                    1
                }
            }
        }
        Ok(None) => 0,
        Err(error) => {
            eprintln!("{}", usage(prog));
            eprintln!("{prog}: {error}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn profile_defaults_to_full_and_accepts_a_name() {
        assert_eq!(parse_profile("x", args(&[])).unwrap(), Some(Profile::Full));
        assert_eq!(
            parse_profile("x", args(&["--profile", "audit"])).unwrap(),
            Some(Profile::Audit)
        );
    }

    #[test]
    fn unknown_profiles_and_arguments_are_refused() {
        assert!(parse_profile("x", args(&["--profile", "nope"])).is_err());
        assert!(parse_profile("x", args(&["--bogus"])).is_err());
    }
}
