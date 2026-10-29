//! `cargo xtask dist`: release builds laid out per install profile with a checksum manifest.
//!
//! Output: `target/dist/<version>/<profile>/{bin/,profile.toml,MANIFEST.json,SHA256SUMS,install.sh}`.
//! Without `--profile` all four profiles are produced from a single cargo build.
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// One install profile: which binaries it carries and how the desktop starts.
pub struct Profile {
    /// Profile id used on the command line and as the folder name.
    pub name: &'static str,
    /// Binaries (without extension) shipped in `bin/`.
    pub bins: &'static [&'static str],
    /// Default desktop display mode written to `profile.toml` (`simple` or `agent`).
    pub default_mode: &'static str,
    /// Whether the installer links the binaries onto `PATH`.
    pub on_path: bool,
}

/// The four profiles from `docs/COMPONENTS.md`.
pub const PROFILES: &[Profile] = &[
    Profile {
        name: "lite",
        bins: &["spiderhead-desktop"],
        default_mode: "crawler",
        on_path: false,
    },
    Profile {
        name: "cli",
        bins: &["spiderhead", "spiderhead-mcp"],
        default_mode: "crawler",
        on_path: true,
    },
    Profile {
        name: "desktop",
        bins: &["spiderhead-desktop", "spiderhead-desktop-ctl", "spiderhead"],
        default_mode: "agent",
        on_path: true,
    },
    Profile {
        name: "all",
        bins: &[
            "spiderhead-desktop",
            "spiderhead-desktop-ctl",
            "spiderhead",
            "spiderhead-mcp",
        ],
        default_mode: "agent",
        on_path: true,
    },
];

/// Cargo package that produces a given binary.
fn package_of(bin: &str) -> &'static str {
    match bin {
        "spiderhead" => "spiderhead-cli",
        "spiderhead-mcp" => "spiderhead-mcp",
        _ => "spiderhead-desktop",
    }
}

/// Workspace version (xtask inherits it).
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Command-line options shared by `dist` and `package`.
#[derive(Default)]
pub struct Opts {
    /// Restrict to one profile.
    pub profile: Option<String>,
    /// Cross-compilation target triple.
    pub target: Option<String>,
}

/// Parse `--profile X` / `--target T`.
pub fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let val = |it: &mut std::slice::Iter<String>| {
            it.next().cloned().ok_or(format!("{a} needs a value"))
        };
        match a.as_str() {
            "--profile" => o.profile = Some(val(&mut it)?),
            "--target" => o.target = Some(val(&mut it)?),
            _ => return Err(format!("unknown argument: {a}")),
        }
    }
    if let Some(p) = &o.profile {
        if !PROFILES.iter().any(|x| x.name == p) {
            return Err(format!("unknown profile: {p} (lite|cli|desktop|all)"));
        }
    }
    Ok(o)
}

/// Run an external command, failing with its name on a non-zero exit.
pub fn run_cmd(cmd: &mut Command) -> Result<(), String> {
    let name = cmd.get_program().to_string_lossy().into_owned();
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("{name} failed: {s}")),
        Err(e) => Err(format!("{name}: {e}")),
    }
}

/// `target/dist/<version>` of the current checkout.
pub fn dist_root() -> PathBuf {
    PathBuf::from("target/dist").join(version())
}

fn exe(bin: &str, target: &Option<String>) -> String {
    let win = target
        .as_deref()
        .map_or(cfg!(windows), |t| t.contains("windows"));
    if win {
        format!("{bin}.exe")
    } else {
        bin.to_string()
    }
}

/// Lowercase hex SHA-256 of a file.
pub fn sha256_file(p: &Path) -> Result<String, String> {
    let bytes = fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Build the release binaries and assemble the requested profile folders.
pub fn run(args: &[String]) -> Result<(), String> {
    let o = parse_opts(args)?;
    let chosen: Vec<&Profile> = PROFILES
        .iter()
        .filter(|p| o.profile.as_deref().is_none_or(|n| n == p.name))
        .collect();
    let mut pkgs: Vec<&str> = chosen
        .iter()
        .flat_map(|p| p.bins.iter().map(|b| package_of(b)))
        .collect();
    pkgs.sort();
    pkgs.dedup();
    let mut build = Command::new("cargo");
    build.args(["build", "--release", "--locked"]);
    for p in &pkgs {
        build.args(["-p", p]);
    }
    if let Some(t) = &o.target {
        build.args(["--target", t]);
    }
    println!("==> cargo build --release ({})", pkgs.join(", "));
    run_cmd(&mut build)?;
    let rel = match &o.target {
        Some(t) => PathBuf::from("target").join(t).join("release"),
        None => PathBuf::from("target/release"),
    };
    for p in chosen {
        assemble(p, &rel, &o.target)?;
    }
    println!("==> dist OK: {}", dist_root().display());
    Ok(())
}

/// Copy binaries, write `profile.toml`, checksums and the manifest for one profile.
fn assemble(p: &Profile, rel: &Path, target: &Option<String>) -> Result<(), String> {
    let dir = dist_root().join(p.name);
    let _ = fs::remove_dir_all(&dir);
    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    for b in p.bins {
        let name = exe(b, target);
        fs::copy(rel.join(&name), bin_dir.join(&name))
            .map_err(|e| format!("{}: {e}", rel.join(&name).display()))?;
    }
    if p.name == "all" {
        let docs = dir.join("docs");
        fs::create_dir_all(&docs).map_err(|e| e.to_string())?;
        for f in ["README.md", "docs/COMPONENTS.md"] {
            if let Some(n) = Path::new(f).file_name() {
                fs::copy(f, docs.join(n)).map_err(|e| format!("{f}: {e}"))?;
            }
        }
    }
    fs::copy("packaging/install.sh", dir.join("install.sh")).map_err(|e| e.to_string())?;
    let bins: Vec<String> = p
        .bins
        .iter()
        .map(|b| format!("\"{}\"", exe(b, target)))
        .collect();
    let toml = format!(
        "# Written by `cargo xtask dist`; the installer copies it next to the binaries.\n\
         profile = \"{}\"\nversion = \"{}\"\ndefault_mode = \"{}\"\npath_cli = {}\nbinaries = [{}]\n",
        p.name,
        version(),
        p.default_mode,
        p.on_path,
        bins.join(", ")
    );
    fs::write(dir.join("profile.toml"), toml).map_err(|e| e.to_string())?;
    let mut files = Vec::new();
    collect(&dir, &dir, &mut files)?;
    files.sort();
    let (mut sums, mut entries) = (String::new(), Vec::new());
    for rel_path in &files {
        let full = dir.join(rel_path);
        let sum = sha256_file(&full)?;
        let size = fs::metadata(&full).map_err(|e| e.to_string())?.len();
        sums.push_str(&format!("{sum}  {rel_path}\n"));
        entries.push(json!({"path": rel_path, "sha256": sum, "size": size}));
    }
    fs::write(dir.join("SHA256SUMS"), sums).map_err(|e| e.to_string())?;
    let manifest = json!({
        "version": version(),
        "profile": p.name,
        "target": target.clone().unwrap_or_else(|| "host".into()),
        "default_mode": p.default_mode,
        "files": entries,
    });
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    fs::write(dir.join("MANIFEST.json"), text + "\n").map_err(|e| e.to_string())?;
    println!(
        "    {} -> {} ({} files)",
        p.name,
        dir.display(),
        files.len()
    );
    Ok(())
}

/// Relative paths of all files under `dir` (excluding the generated checksum files).
fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
    for e in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = e.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            collect(root, &path, out)?;
        } else if let Ok(r) = path.strip_prefix(root) {
            let s = r.to_string_lossy().replace('\\', "/");
            if s != "SHA256SUMS" && s != "MANIFEST.json" {
                out.push(s);
            }
        }
    }
    Ok(())
}
