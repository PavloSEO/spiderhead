//! `spiderhead-desktop-ctl` — drive a running SpiderHead UI through its control channel.
//!
//! ```text
//! spiderhead-desktop-ctl send --port 7847 --method ui.inspect --params '{"full":true}'
//! spiderhead-desktop-ctl commands [--port 7847] [--search filter]
//! ```
//!
//! Exit codes: 0 the command answered, 1 the app refused it (`ok:false`), 2 the app is not
//! reachable or the arguments are wrong.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use serde_json::Value;

use spiderhead_control::backend::Backend;
use spiderhead_control::client::Remote;

fn usage() -> String {
    "usage: spiderhead-desktop-ctl send [--port N] --method <id> [--params <json>]\n\
       spiderhead-desktop-ctl commands [--port N] [--search <text>]"
        .into()
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        eprintln!("{}", usage());
        return ExitCode::from(2);
    };

    let flags: Vec<String> = args.collect();
    let parsed = match parse_flags(&flags) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("spiderhead-desktop-ctl: {e}\n{}", usage());
            return ExitCode::from(2);
        }
    };
    let port = parsed.port;
    let method = parsed.method;
    let params = parsed.params;
    let search = parsed.search;

    let addr = spiderhead_control::control_addr(&port);
    match command.as_str() {
        "send" => {
            if method.is_empty() {
                eprintln!("spiderhead-desktop-ctl: send needs --method <id>");
                return ExitCode::from(2);
            }
            call(&addr, &method, params)
        }
        "commands" => {
            let params = if search.is_empty() {
                Value::Null
            } else {
                serde_json::json!({"search": search})
            };
            call(&addr, "commands.list", params)
        }
        other => {
            eprintln!(
                "spiderhead-desktop-ctl: unknown command {other}\n{}",
                usage()
            );
            ExitCode::from(2)
        }
    }
}

struct Flags {
    port: String,
    method: String,
    params: Value,
    search: String,
}

/// `--flag value` pairs; a flag without its value is an error, not a silent
/// stop — `send --method` at the end must not run with defaults.
fn parse_flags(flags: &[String]) -> Result<Flags, String> {
    let mut out = Flags {
        port: spiderhead_control::DEFAULT_PORT.to_string(),
        method: String::new(),
        params: Value::Null,
        search: String::new(),
    };
    let mut i = 0;
    while i < flags.len() {
        let name = flags[i].as_str();
        let mut value = || -> Result<String, String> {
            i += 1;
            flags
                .get(i)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match name {
            "--port" => out.port = value()?,
            "--method" => out.method = value()?,
            "--params" => {
                let v = value()?;
                out.params = serde_json::from_str(&v)
                    .map_err(|e| format!("--params is not valid JSON: {e}"))?;
            }
            "--search" => out.search = value()?,
            other => return Err(format!("unknown argument {other}")),
        }
        i += 1;
    }
    Ok(out)
}

fn call(addr: &str, method: &str, params: Value) -> ExitCode {
    let mut remote = match Remote::connect(addr) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("spiderhead-desktop-ctl: {e}");
            return ExitCode::from(2);
        }
    };
    match remote.call(method, params) {
        Ok(result) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&result).unwrap_or_default()
            );
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("spiderhead-desktop-ctl: {message}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // regression (review 2026-10-27): a trailing flag without a value was
    // silently dropped and the command ran with defaults.
    #[test]
    fn flag_without_a_value_is_an_error() {
        for flags in [
            vec!["--port"],
            vec!["--method"],
            vec!["--params"],
            vec!["--search"],
            vec!["--method", "echo", "--port"],
        ] {
            let flags: Vec<String> = flags.iter().map(|s| s.to_string()).collect();
            assert!(parse_flags(&flags).is_err(), "{flags:?}");
        }
    }

    #[test]
    fn flags_parse_in_any_order() {
        let flags: Vec<String> = [
            "--search",
            "scan",
            "--port",
            "9999",
            "--method",
            "ui.inspect",
            "--params",
            "{\"full\":true}",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let f = parse_flags(&flags).unwrap();
        assert_eq!(f.port, "9999");
        assert_eq!(f.method, "ui.inspect");
        assert_eq!(f.params, serde_json::json!({"full": true}));
        assert_eq!(f.search, "scan");
    }
}
