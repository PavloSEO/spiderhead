//! `cargo xtask version <show|set X.Y.Z|check [--tag vX.Y.Z]>`: one command bumps every
//! place the product version is spelled; `check` fails when any of them disagrees.
use std::{fs, process::Command};

const CARGO: &str = "Cargo.toml";
const PRESETS: &str = "spiderhead/core/src/user_agents/presets.rs";
const README: &str = "README.md";
const CITATION: &str = "CITATION.cff";

/// A version spelled as `<prefix><digits and dots>` inside a file.
const SPELLINGS: &[(&str, &str, &str)] = &[
    (PRESETS, "PRODUCT_VERSION: &str = \"", "PRODUCT_VERSION"),
    (PRESETS, "SpiderHeadBot/", "default user agent"),
    (README, "badge/version-", "README badge"),
    (CITATION, "\nversion: ", "CITATION.cff"),
    (CARGO, "\nversion = \"", "workspace version"),
];

pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("show") => {
            println!("{}", workspace_version()?);
            Ok(())
        }
        Some("set") => set(args.get(1).ok_or("usage: version set X.Y.Z")?),
        Some("check") => {
            let tag = match args.get(1).map(String::as_str) {
                Some("--tag") => Some(args.get(2).ok_or("--tag needs a value")?.as_str()),
                _ => None,
            };
            check(tag)
        }
        _ => Err("usage: cargo xtask version <show|set X.Y.Z|check [--tag vX.Y.Z]>".into()),
    }
}

fn read(path: &str) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
}

fn is_version(v: &str) -> bool {
    let parts: Vec<_> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn run_len(s: &str) -> usize {
    s.bytes()
        .take_while(|b| b.is_ascii_digit() || *b == b'.')
        .count()
}

/// Every version spelled right after `prefix`.
fn found(text: &str, prefix: &str) -> Vec<String> {
    text.match_indices(prefix)
        .map(|(i, _)| {
            let rest = &text[i + prefix.len()..];
            rest[..run_len(rest)].to_string()
        })
        .collect()
}

/// Replace every version spelled right after `prefix`.
fn replace(text: &str, prefix: &str, new: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(prefix) {
        let end = i + prefix.len();
        out.push_str(&rest[..end]);
        out.push_str(new);
        rest = &rest[end + run_len(&rest[end..])..];
    }
    out.push_str(rest);
    out
}

/// `[workspace.package] version`: the first top-level `version = ` line of Cargo.toml.
fn workspace_version() -> Result<String, String> {
    let text = format!("\n{}", read(CARGO)?);
    let v = found(&text, "\nversion = \"")
        .into_iter()
        .next()
        .ok_or("Cargo.toml: no workspace version")?;
    is_version(&v)
        .then_some(v)
        .ok_or_else(|| "Cargo.toml: workspace version is not X.Y.Z".into())
}

fn set(new: &str) -> Result<(), String> {
    if !is_version(new) {
        return Err(format!("`{new}` is not X.Y.Z"));
    }
    for path in [CARGO, PRESETS, README, CITATION] {
        let mut text = format!("\n{}", read(path)?);
        for (_, prefix, _) in SPELLINGS.iter().filter(|s| s.0 == path) {
            // Cargo.toml: only the first match is the workspace version.
            text = if path == CARGO {
                let i = text
                    .find(prefix)
                    .ok_or("Cargo.toml: no workspace version")?
                    + prefix.len();
                format!("{}{new}{}", &text[..i], &text[i + run_len(&text[i..])..])
            } else {
                replace(&text, prefix, new)
            };
        }
        fs::write(path, &text[1..]).map_err(|e| format!("{path}: {e}"))?;
    }
    // Refresh the workspace members' entries in Cargo.lock without touching dependencies.
    let ok = Command::new("cargo")
        .args(["update", "--workspace", "--offline"])
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        return Err("cargo update --workspace failed; refresh Cargo.lock by hand".into());
    }
    println!("version set to {new}");
    check(None)
}

fn check(tag: Option<&str>) -> Result<(), String> {
    let want = workspace_version()?;
    let mut bad = Vec::new();
    for (path, prefix, what) in SPELLINGS {
        let text = format!("\n{}", read(path)?);
        let mut seen = found(&text, prefix);
        if *path == CARGO {
            seen.truncate(1);
        }
        if seen.is_empty() {
            bad.push(format!("{what} ({path}): not found"));
        }
        bad.extend(
            seen.into_iter()
                .filter(|v| *v != want)
                .map(|v| format!("{what} ({path}): {v}, workspace is {want}")),
        );
    }
    if let Some(tag) = tag {
        if tag.strip_prefix('v') != Some(want.as_str()) {
            bad.push(format!("tag {tag} != workspace version {want}"));
        }
    }
    if bad.is_empty() {
        println!("version {want}: consistent");
        Ok(())
    } else {
        Err(bad.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_every_spelling_and_keeps_the_rest() {
        let s = "A SpiderHeadBot/0.9.1 (x) B SpiderHeadBot/0.9.1 (y)";
        assert_eq!(
            replace(s, "SpiderHeadBot/", "1.2.3"),
            "A SpiderHeadBot/1.2.3 (x) B SpiderHeadBot/1.2.3 (y)"
        );
        assert_eq!(found(s, "SpiderHeadBot/"), ["0.9.1", "0.9.1"]);
    }

    #[test]
    fn rejects_malformed_versions() {
        assert!(is_version("0.10.0"));
        for bad in ["1.2", "1.2.3.4", "v1.2.3", "1..3", "1.2.x", ""] {
            assert!(!is_version(bad), "{bad}");
        }
    }
}
