//! Repo automation: `cargo xtask <ci|layers|slices|oracle|dist|package>`.
mod dist;
mod oracle;
mod package;

use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    process::{exit, Command},
};

/// Dependency layers: a crate may depend only on crates of the same or a lower layer.
const LAYERS: &[(&str, u8)] = &[
    ("spiderhead-core", 0),
    ("spiderhead-control", 0),
    ("spiderhead-formats", 1),
    ("spiderhead-store", 2),
    ("spiderhead-parse", 2),
    ("spiderhead-checks", 3),
    ("spiderhead-log-analyzer", 3),
    ("spiderhead-fetch", 3),
    ("spiderhead-render", 3),
    ("spiderhead-crawl", 4),
    ("spiderhead-sf-import", 5),
    ("spiderhead-catalog", 5),
    ("spiderhead-reports", 5),
    ("spiderhead-projects", 5),
    ("spiderhead-sources", 5),
    ("spiderhead-semantics", 5),
    ("spiderhead-recon", 5),
    ("spiderhead-mcp", 6),
    ("spiderhead-tui", 6),
    ("spiderhead-remote", 6),
    ("spiderhead-cli", 7),
    ("spiderhead-oracle-diff", 7),
    ("spiderhead-oracle-testkit", 7),
    ("spiderhead-desktop", 8),
];

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let ok = match cmd.as_str() {
        "ci" => ci(),
        "windows-check" => windows_check(),
        "layers" => layers(),
        "slices" => slices(),
        "oracle" => oracle::run(&std::env::args().skip(2).collect::<Vec<_>>()),
        "dist" => report(dist::run(&std::env::args().skip(2).collect::<Vec<_>>())),
        "package" => report(package::run(&std::env::args().skip(2).collect::<Vec<_>>())),
        _ => {
            eprintln!("usage: cargo xtask <ci|windows-check|layers|slices|oracle|dist|package>");
            false
        }
    };
    exit(if ok { 0 } else { 1 });
}

/// Print a task error (if any) and turn the result into a pass/fail flag.
fn report(r: Result<(), String>) -> bool {
    if let Err(e) = &r {
        eprintln!("error: {e}");
    }
    r.is_ok()
}

/// Tests run under nextest (process per test, parallel) when it is installed;
/// plain `cargo test` otherwise. Doctests always go through `cargo test --doc`.
fn test_args() -> &'static [&'static str] {
    let nextest = Command::new("cargo")
        .args(["nextest", "--version"])
        .output()
        .is_ok_and(|o| o.status.success());
    if nextest {
        &["nextest", "run", "--workspace", "--no-fail-fast"]
    } else {
        &["test", "--workspace", "--lib", "--bins", "--tests"]
    }
}

/// Windows triple the workspace is kept compiling for.
const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";

/// `cargo check` the whole workspace for [`WINDOWS_TARGET`]; installs the
/// rustup target first when it is missing.
///
/// On non-Windows hosts the crates with C build scripts (sqlite, ring, zlib)
/// have no MSVC toolchain to compile under, so the step runs with a stub C
/// compiler that emits empty objects: `check` never links, and all the Rust
/// code still gets fully type-checked.
fn windows_check() -> bool {
    let installed = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(WINDOWS_TARGET))
        .unwrap_or(false);
    if !installed
        && !Command::new("rustup")
            .args(["target", "add", WINDOWS_TARGET])
            .status()
            .is_ok_and(|s| s.success())
    {
        eprintln!("rustup target add {WINDOWS_TARGET} failed");
        return false;
    }
    let mut cmd = Command::new("cargo");
    cmd.args([
        "check",
        "--workspace",
        "--all-targets",
        "--target",
        WINDOWS_TARGET,
    ]);
    #[cfg(not(windows))]
    {
        match windows_cc_stub() {
            Some(dir) => {
                cmd.env("CC_x86_64_pc_windows_msvc", dir.join("cc-stub"))
                    .env("CXX_x86_64_pc_windows_msvc", dir.join("cc-stub"))
                    .env("AR_x86_64_pc_windows_msvc", dir.join("ar-stub"))
                    // sccache refuses script compilers; bypass it for this check.
                    .env("RUSTC_WRAPPER", "");
            }
            None => {
                eprintln!("cannot stage the stub C compiler for the windows check");
                return false;
            }
        }
    }
    cmd.status().is_ok_and(|s| s.success())
}

/// Stage the no-op `cc`/`ar` stub used by [`windows_check`] on non-Windows
/// hosts and return its directory.
#[cfg(not(windows))]
fn windows_cc_stub() -> Option<std::path::PathBuf> {
    // The compiler stub answers the output-path flag shapes cc-rs uses
    // (`-o path`, `-Fopath`, `/Fopath`); the archiver stub answers the same
    // plus `ar cq lib.a ...` and `lib /OUT:lib.a ...` positional outputs.
    // Emitting the file matters: build scripts hard-link the artifact.
    #[cfg(unix)]
    let cc_script = "#!/bin/sh\n\
        out=\"\"; want=\"\"\n\
        for a in \"$@\"; do\n\
        \tif [ -n \"$want\" ]; then out=\"$a\"; want=\"\"; fi\n\
        \tcase \"$a\" in -o|/o|-Fo|/Fo) want=1;; -Fo*) out=${a#-Fo};; /Fo*) out=${a#/Fo};; esac\n\
        done\n\
        [ -n \"$out\" ] && { mkdir -p \"$(dirname \"$out\")\"; : > \"$out\"; }\n\
        exit 0\n";
    #[cfg(unix)]
    let ar_script = "#!/bin/sh\n\
        out=\"\"; want=\"\"\n\
        for a in \"$@\"; do\n\
        \tif [ -n \"$want\" ]; then out=\"$a\"; want=\"\"; continue; fi\n\
        \tcase \"$a\" in\n\
        \t\t-o|/o|-Fo|/Fo|-OUT|/OUT|/out|-out) want=1;;\n\
        \t\t-Fo*) out=${a#-Fo};; /Fo*) out=${a#/Fo};;\n\
        \t\t-OUT:*|-out:*|/OUT:*|/out:*) out=${a#*:};;\n\
        \t\t*.a|*.lib) [ -z \"$out\" ] && out=\"$a\";;\n\
        \tesac\n\
        done\n\
        [ -n \"$out\" ] && { mkdir -p \"$(dirname \"$out\")\"; : > \"$out\"; }\n\
        exit 0\n";
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("spiderhead-xtask-windows-check");
        fs::create_dir_all(&dir).ok()?;
        for (name, script) in [("cc-stub", cc_script), ("ar-stub", ar_script)] {
            let path = dir.join(name);
            fs::write(&path, script).ok()?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).ok()?;
        }
        Some(dir)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn ci() -> bool {
    let steps: [(&str, &[&str]); 4] = [
        ("fmt", &["fmt", "--all", "--check"]),
        (
            "clippy",
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ),
        ("test", test_args()),
        ("doctest", &["test", "--workspace", "--doc"]),
    ];
    for (name, args) in steps {
        println!("==> {name}: cargo {}", args.join(" "));
        if !Command::new("cargo")
            .args(args)
            .status()
            .is_ok_and(|s| s.success())
        {
            eprintln!("==> {name} FAILED");
            return false;
        }
    }
    // The Windows build is not complete yet (see ROADMAP 0.10), so its check runs on
    // demand with `cargo xtask windows-check` and does not gate `ci`.
    println!("==> ci OK");
    true
}

fn layers() -> bool {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .expect("cargo metadata");
    let meta: Value = serde_json::from_slice(&out.stdout).expect("metadata json");
    let table: BTreeMap<_, _> = LAYERS.iter().copied().collect();
    let mut bad = 0;
    for p in meta["packages"].as_array().unwrap() {
        let name = p["name"].as_str().unwrap();
        if name == "xtask" {
            continue;
        }
        let Some(&mine) = table.get(name) else {
            eprintln!("{name}: not in LAYERS table");
            bad += 1;
            continue;
        };
        for d in p["dependencies"].as_array().unwrap() {
            let dn = d["name"].as_str().unwrap();
            // Test-only dependencies (e.g. the oracle differ) may point anywhere.
            if d["kind"].as_str() == Some("dev") {
                continue;
            }
            if let Some(&theirs) = table.get(dn) {
                if theirs > mine {
                    eprintln!("{name} (layer {mine}) depends on {dn} (layer {theirs})");
                    bad += 1;
                }
            }
        }
    }
    println!("layers: {} violation(s)", bad);
    bad == 0
}

fn slices() -> bool {
    let Ok(text) = fs::read_to_string("slices.toml") else {
        println!("slices.toml not found");
        return true;
    };
    let doc: toml::Value = match text.parse() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("slices.toml: {e}");
            return false;
        }
    };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0;
    if let Some(list) = doc.get("slice").and_then(|v| v.as_array()) {
        for s in list {
            let st = s
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            *counts.entry(st.into()).or_default() += 1;
            total += 1;
        }
    }
    for (k, v) in &counts {
        println!("{k}: {v}");
    }
    println!("total: {total}");
    true
}
