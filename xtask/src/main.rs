//! Repo automation: `cargo xtask <ci|layers|slices|oracle|dist|package|version>`.
mod dist;
mod oracle;
mod package;
mod version;

use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
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
        "ci" => ci(std::env::args().skip(2).any(|a| a == "--quick")),
        "windows-check" => windows_check(),
        "layers" => layers(),
        "slices" => slices(),
        "rebaseline" => rebaseline(),
        "oracle" => oracle::run(&std::env::args().skip(2).collect::<Vec<_>>()),
        "dist" => report(dist::run(&std::env::args().skip(2).collect::<Vec<_>>())),
        "package" => report(package::run(&std::env::args().skip(2).collect::<Vec<_>>())),
        "version" => report(version::run(&std::env::args().skip(2).collect::<Vec<_>>())),
        _ => {
            eprintln!(
                "usage: cargo xtask <ci [--quick]|windows-check|layers|slices|rebaseline|oracle|dist|package|version>"
            );
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
fn test_args(quick: bool) -> &'static [&'static str] {
    if quick {
        return if has_nextest() {
            &[
                "nextest",
                "run",
                "--workspace",
                "--lib",
                "--bins",
                "--no-fail-fast",
            ]
        } else {
            &["test", "--workspace", "--lib", "--bins"]
        };
    }
    if has_nextest() {
        &["nextest", "run", "--workspace", "--no-fail-fast"]
    } else {
        &["test", "--workspace", "--lib", "--bins", "--tests"]
    }
}

fn has_nextest() -> bool {
    Command::new("cargo")
        .args(["nextest", "--version"])
        .output()
        .is_ok_and(|o| o.status.success())
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

/// Full gate, or with `quick` the per-merge subset: fmt, clippy and the unit tests of
/// every crate (no integration tests, doctests or windows check).
fn ci(quick: bool) -> bool {
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
        ("test", test_args(quick)),
        ("doctest", &["test", "--workspace", "--doc"]),
    ];
    println!("==> version: every spelling of the product version agrees");
    if let Err(e) = version::run(&["check".to_string()]) {
        eprintln!("{e}\n==> version FAILED");
        return false;
    }
    let steps = if quick { &steps[..3] } else { &steps[..] };
    for (name, args) in steps.iter().copied() {
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
    if !quick {
        println!(
            "==> windows-check: cargo check --workspace --all-targets --target {WINDOWS_TARGET}"
        );
        if !windows_check() {
            eprintln!("==> windows-check FAILED");
            return false;
        }
    }
    if quick {
        println!("==> ci --quick OK");
    } else {
        println!("==> ci OK");
    }
    true
}

/// Shared baselines regenerated before the full suite, in dependency order:
/// raw captures first (they feed every downstream baseline), then the
/// artifacts other fixtures are derived from. Each entry is a separate `cargo
/// test` invocation so the writes happen before any reader sees them.
const PRE_BLESS: &[(&str, &[&str])] = &[
    (
        "raw captures",
        &["test", "-p", "spiderhead-crawl", "--test", "parity"],
    ),
    (
        "scan status",
        &[
            "test",
            "-p",
            "spiderhead-store",
            "--lib",
            "scan_status_matches_the_reference_baseline",
        ],
    ),
    (
        "audit.v2",
        &[
            "test",
            "-p",
            "spiderhead-store",
            "--lib",
            "reader_matches_oracle_baselines_for_every_site",
        ],
    ),
    (
        "ledger projection",
        &[
            "test",
            "-p",
            "spiderhead-store",
            "--test",
            "ledger_oracle",
            "prepared_source",
        ],
    ),
];

/// Regenerate the test fixtures from live output, then keep large JSON small.
///
/// `SPIDERHEAD_BLESS=1` makes every comparison site rewrite its expected
/// artifact from the freshly computed value, so a rebaseline always goes
/// through the same code paths that later verify it. Afterwards any `*.json`
/// fixture above 1 MiB is stored zstd-compressed as `<name>.zst` and the plain
/// file removed; the testkit readers prefer the compressed sibling.
fn rebaseline() -> bool {
    for (name, args) in PRE_BLESS.iter().copied() {
        println!("==> pre-bless {name}: cargo {}", args.join(" "));
        let ok = Command::new("cargo")
            .args(args)
            .env("SPIDERHEAD_BLESS", "1")
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("==> pre-bless {name} FAILED");
            return false;
        }
    }
    let ok = Command::new("cargo")
        .args(test_args(false))
        .env("SPIDERHEAD_BLESS", "1")
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("==> rebaseline FAILED");
        return false;
    }
    let compressed = compress_big_fixtures();
    refresh_raw_sums();
    println!("==> rebaseline OK ({compressed} fixture(s) compressed)");
    true
}

/// Rewrite `oracle/raw-scans/SHA256SUMS.txt` after captures were republished.
fn refresh_raw_sums() {
    let dir = Path::new("oracle/raw-scans");
    let mut files = Vec::new();
    collect_paths(dir, &mut files, &|p| {
        p.extension().is_some_and(|e| e == "sqlite")
    });
    files.sort();
    let mut out = String::new();
    for path in files {
        if path.file_name().is_some_and(|n| n == "SHA256SUMS.txt") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let digest = sha256_hex(&bytes);
        if let Ok(rel) = path.strip_prefix(dir) {
            out.push_str(&format!("{digest}  {}\n", rel.display()));
        }
    }
    let _ = fs::write(dir.join("SHA256SUMS.txt"), out);
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn collect_paths<F: Fn(&Path) -> bool>(dir: &Path, out: &mut Vec<PathBuf>, accept: &F) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_paths(&path, out, accept);
        } else if accept(&path) {
            out.push(path);
        }
    }
}

/// Walk the fixture trees and return `(compressed_count)`. Fixture scope is
/// `oracle/**` and every `tests/` subtree; shipped data (`*/data/`) stays
/// plain. A stale `.zst` sibling of a small `.json` is removed.
fn compress_big_fixtures() -> usize {
    let mut files = Vec::new();
    collect_json(Path::new("oracle"), &mut files);
    collect_json(Path::new("spiderhead"), &mut files);
    let mut compressed = 0usize;
    for path in files {
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        let zst = zst_sibling(&path);
        if meta.len() > 1_048_576 {
            let Ok(raw) = fs::read(&path) else {
                continue;
            };
            let packed = match zstd::encode_all(raw.as_slice(), 3) {
                Ok(packed) => packed,
                Err(e) => {
                    eprintln!("{}: compress failed: {e}", path.display());
                    continue;
                }
            };
            if fs::write(&zst, packed).is_ok() {
                let _ = fs::remove_file(&path);
                compressed += 1;
            } else {
                eprintln!("{}: write failed", zst.display());
            }
        } else if zst.exists() {
            let _ = fs::remove_file(&zst);
        }
    }
    compressed
}

fn zst_sibling(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".zst");
    PathBuf::from(name)
}

fn is_fixture_path(path: &Path) -> bool {
    path.starts_with("oracle") || path.components().any(|c| c.as_os_str() == "tests")
}

fn collect_json(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            if name == "target" || name == ".git" {
                continue;
            }
            collect_json(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") && is_fixture_path(&path) {
            out.push(path);
        }
    }
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
