//! `cargo xtask package <macos|linux|windows>`: installers built from `target/dist/<version>/`.
use crate::dist::{self, dist_root, run_cmd, version, Opts};
use std::{fs, path::Path, process::Command};

/// Entry point: `package <platform> [--target T]`.
pub fn run(args: &[String]) -> Result<(), String> {
    let platform = args.first().map(String::as_str).unwrap_or("");
    let o = dist::parse_opts(&args[1.min(args.len())..])?;
    match platform {
        "macos" if cfg!(target_os = "macos") => macos(&o),
        "linux" if cfg!(target_os = "linux") => linux(&o),
        "windows" if cfg!(windows) => windows(),
        "macos" | "linux" | "windows" => Err(format!(
            "`package {platform}` builds only on {platform} (see docs/РАБОТА-С-RUST.md)"
        )),
        _ => Err("usage: cargo xtask package <macos|linux|windows>".into()),
    }
}

/// Make sure all four profile folders exist, building them when missing.
fn ensure_dist(o: &Opts) -> Result<(), String> {
    if dist_root().join("all/MANIFEST.json").exists()
        && dist_root().join("lite/MANIFEST.json").exists()
    {
        return Ok(());
    }
    let mut a = vec![];
    if let Some(t) = &o.target {
        a.extend(["--target".to_string(), t.clone()]);
    }
    dist::run(&a)
}

fn sh(cmd: &str, args: &[&str]) -> Result<(), String> {
    run_cmd(Command::new(cmd).env("COPYFILE_DISABLE", "1").args(args))
}

fn write_exec(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn copy_bin(from: &Path, to_dir: &Path, name: &str) -> Result<(), String> {
    fs::create_dir_all(to_dir).map_err(|e| e.to_string())?;
    fs::copy(from.join(name), to_dir.join(name)).map_err(|e| format!("{name}: {e}"))?;
    Ok(())
}

const INFO_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>SpiderHead</string>
<key>CFBundleDisplayName</key><string>SpiderHead</string>
<key>CFBundleIdentifier</key><string>com.spiderhead.desktop</string>
<key>CFBundleExecutable</key><string>spiderhead-desktop</string>
<key>CFBundleIconFile</key><string>AppIcon</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>@V@</string>
<key>CFBundleShortVersionString</key><string>@V@</string>
<key>LSMinimumSystemVersion</key><string>12.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
"#;

/// Distribution XML with one choice per install profile.
const DISTRIBUTION: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="2">
  <title>SpiderHead @V@</title>
  <options customize="allow" require-scripts="false" hostArchitectures="@ARCH@"/>
  <domains enable_localSystem="true"/>
  <choices-outline>
    <line choice="lite"/>
    <line choice="cli"/>
    <line choice="desktop"/>
    <line choice="all"/>
  </choices-outline>
  <choice id="lite" title="Лёгкая" description="Окно SpiderHead в простом режиме. Командная строка в PATH не добавляется." start_selected="false">
    <pkg-ref id="com.spiderhead.app"/>
  </choice>
  <choice id="cli" title="CLI" description="Команды spiderhead и spiderhead-mcp в /usr/local/bin. Без окна." start_selected="false">
    <pkg-ref id="com.spiderhead.cli"/>
  </choice>
  <choice id="desktop" title="Desktop" description="Окно SpiderHead с режимом «С агентом», spiderhead-desktop-ctl и CLI." start_selected="true">
    <pkg-ref id="com.spiderhead.app"/>
    <pkg-ref id="com.spiderhead.cli"/>
    <pkg-ref id="com.spiderhead.desktop-extras"/>
  </choice>
  <choice id="all" title="Всё" description="Desktop, CLI, MCP-сервер и документация." start_selected="false">
    <pkg-ref id="com.spiderhead.app"/>
    <pkg-ref id="com.spiderhead.cli"/>
    <pkg-ref id="com.spiderhead.desktop-extras"/>
    <pkg-ref id="com.spiderhead.docs"/>
  </choice>
  <pkg-ref id="com.spiderhead.app" version="@V@" onConclusion="none">spiderhead-app.pkg</pkg-ref>
  <pkg-ref id="com.spiderhead.cli" version="@V@" onConclusion="none">spiderhead-cli.pkg</pkg-ref>
  <pkg-ref id="com.spiderhead.desktop-extras" version="@V@" onConclusion="none">spiderhead-desktop-extras.pkg</pkg-ref>
  <pkg-ref id="com.spiderhead.docs" version="@V@" onConclusion="none">spiderhead-docs.pkg</pkg-ref>
</installer-gui-script>
"#;

/// macOS: `.app` bundle, component packages, product `.pkg` with choices, and a `.dmg`.
fn macos(o: &Opts) -> Result<(), String> {
    ensure_dist(o)?;
    let v = version();
    let all = dist_root().join("all");
    let bins = all.join("bin");
    let stage = dist_root().join("_stage-macos");
    let _ = fs::remove_dir_all(&stage);
    let out = dist_root().join("installers");
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    };

    // .app bundle (default mode = simple: no profile file is written by this component)
    let app = stage.join("app-root/Applications/SpiderHead.app/Contents");
    copy_bin(&bins, &app.join("MacOS"), "spiderhead-desktop")?;
    fs::write(app.join("Info.plist"), INFO_PLIST.replace("@V@", v)).map_err(|e| e.to_string())?;
    make_icon(&app.join("Resources"), &stage)?;
    let _ = sh(
        "codesign",
        &[
            "--force",
            "-s",
            "-",
            &app.parent().unwrap_or(&app).to_string_lossy(),
        ],
    ); // ad-hoc only; real signing needs an Apple ID (TODO)

    // CLI payload + postinstall symlinks into /usr/local/bin
    let lib = "usr/local/lib/spiderhead/bin";
    for b in ["spiderhead", "spiderhead-mcp"] {
        copy_bin(&bins, &stage.join("cli-root").join(lib), b)?;
    }
    let scripts = stage.join("cli-scripts");
    fs::create_dir_all(&scripts).map_err(|e| e.to_string())?;
    write_exec(
        &scripts.join("postinstall"),
        &link_script(&["spiderhead", "spiderhead-mcp"], ""),
    )?;

    // Desktop extras: ctl + profile.toml switching the desktop to agent mode
    copy_bin(
        &bins,
        &stage.join("extras-root").join(lib),
        "spiderhead-desktop-ctl",
    )?;
    let xs = stage.join("extras-scripts");
    fs::create_dir_all(&xs).map_err(|e| e.to_string())?;
    let agent = "mkdir -p \"/Library/Application Support/SpiderHead\"\n\
        printf 'profile = \"desktop\"\\ndefault_mode = \"agent\"\\n' > \"/Library/Application Support/SpiderHead/profile.toml\"\n";
    write_exec(
        &xs.join("postinstall"),
        &link_script(&["spiderhead-desktop-ctl"], agent),
    )?;

    // Docs
    let doc = stage.join("docs-root/usr/local/share/doc/spiderhead");
    fs::create_dir_all(&doc).map_err(|e| e.to_string())?;
    for f in ["README.md", "COMPONENTS.md"] {
        fs::copy(all.join("docs").join(f), doc.join(f)).map_err(|e| format!("{f}: {e}"))?;
    }

    let pkgs = stage.join("pkgs");
    fs::create_dir_all(&pkgs).map_err(|e| e.to_string())?;
    component(&stage, "app", "com.spiderhead.app", v, None, true, &pkgs)?;
    component(
        &stage,
        "cli",
        "com.spiderhead.cli",
        v,
        Some("cli-scripts"),
        false,
        &pkgs,
    )?;
    component(
        &stage,
        "extras",
        "com.spiderhead.desktop-extras",
        v,
        Some("extras-scripts"),
        false,
        &pkgs,
    )?;
    component(&stage, "docs", "com.spiderhead.docs", v, None, false, &pkgs)?;
    let dist_xml = stage.join("distribution.xml");
    fs::write(
        &dist_xml,
        DISTRIBUTION.replace("@V@", v).replace("@ARCH@", arch),
    )
    .map_err(|e| e.to_string())?;
    let pkg = out.join(format!("SpiderHead-{v}-macos-{arch}.pkg"));
    let _ = fs::remove_file(&pkg);
    sh(
        "productbuild",
        &[
            "--distribution",
            &dist_xml.to_string_lossy(),
            "--package-path",
            &pkgs.to_string_lossy(),
            &pkg.to_string_lossy(),
        ],
    )?;

    // .dmg for drag-install
    let dmg_dir = stage.join("dmg");
    fs::create_dir_all(&dmg_dir).map_err(|e| e.to_string())?;
    sh(
        "cp",
        &[
            "-R",
            &stage
                .join("app-root/Applications/SpiderHead.app")
                .to_string_lossy(),
            &dmg_dir.to_string_lossy(),
        ],
    )?;
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink("/Applications", dmg_dir.join("Applications"));
    let dmg = out.join(format!("SpiderHead-{v}-macos-{arch}.dmg"));
    let _ = fs::remove_file(&dmg);
    sh(
        "hdiutil",
        &[
            "create",
            "-volname",
            "SpiderHead",
            "-srcfolder",
            &dmg_dir.to_string_lossy(),
            "-ov",
            "-format",
            "UDZO",
            &dmg.to_string_lossy(),
        ],
    )?;
    let _ = fs::remove_dir_all(&stage);
    println!(
        "==> package macos OK:\n    {}\n    {}",
        pkg.display(),
        dmg.display()
    );
    Ok(())
}

/// Shell script that symlinks binaries from the lib dir into `/usr/local/bin`, then runs `extra`.
fn link_script(bins: &[&str], extra: &str) -> String {
    let mut s = String::from("#!/bin/sh\nmkdir -p /usr/local/bin\n");
    for b in bins {
        s.push_str(&format!(
            "ln -sf /usr/local/lib/spiderhead/bin/{b} /usr/local/bin/{b}\n"
        ));
    }
    s + extra + "exit 0\n"
}

/// `pkgbuild` one component package from `<stage>/<name>-root`.
fn component(
    stage: &Path,
    name: &str,
    id: &str,
    v: &str,
    scripts: Option<&str>,
    plist_fix: bool,
    pkgs: &Path,
) -> Result<(), String> {
    let root = stage.join(format!("{name}-root"));
    let file = pkgs.join(format!(
        "spiderhead-{}.pkg",
        if name == "extras" {
            "desktop-extras"
        } else {
            name
        }
    ));
    let mut a: Vec<String> = vec![
        "--root".into(),
        root.to_string_lossy().into(),
        "--identifier".into(),
        id.into(),
        "--version".into(),
        v.into(),
        "--install-location".into(),
        "/".into(),
    ];
    if plist_fix {
        // Keep the .app at /Applications even if another copy exists elsewhere.
        let plist = stage.join(format!("{name}.plist"));
        sh(
            "pkgbuild",
            &[
                "--analyze",
                "--root",
                &root.to_string_lossy(),
                &plist.to_string_lossy(),
            ],
        )?;
        sh(
            "plutil",
            &[
                "-replace",
                "0.BundleIsRelocatable",
                "-bool",
                "NO",
                &plist.to_string_lossy(),
            ],
        )?;
        a.extend(["--component-plist".into(), plist.to_string_lossy().into()]);
    }
    if let Some(s) = scripts {
        a.extend(["--scripts".into(), stage.join(s).to_string_lossy().into()]);
    }
    a.push(file.to_string_lossy().into());
    run_cmd(
        Command::new("pkgbuild")
            .env("COPYFILE_DISABLE", "1")
            .args(&a),
    )
}

/// Build `AppIcon.icns` from the placeholder PNG via `sips` + `iconutil`.
fn make_icon(resources: &Path, stage: &Path) -> Result<(), String> {
    fs::create_dir_all(resources).map_err(|e| e.to_string())?;
    let set = stage.join("AppIcon.iconset");
    fs::create_dir_all(&set).map_err(|e| e.to_string())?;
    for (px, name) in [
        (16, "icon_16x16"),
        (32, "icon_16x16@2x"),
        (128, "icon_128x128"),
        (256, "icon_128x128@2x"),
        (512, "icon_512x512"),
        (1024, "icon_512x512@2x"),
    ] {
        let out = set.join(format!("{name}.png"));
        sh(
            "sips",
            &[
                "-z",
                &px.to_string(),
                &px.to_string(),
                "packaging/macos/icon-placeholder.png",
                "--out",
                &out.to_string_lossy(),
            ],
        )?;
    }
    sh(
        "iconutil",
        &[
            "-c",
            "icns",
            &set.to_string_lossy(),
            "-o",
            &resources.join("AppIcon.icns").to_string_lossy(),
        ],
    )
}

/// Linux: one `.tar.gz` per profile (binaries, `profile.toml`, `install.sh`, checksums).
fn linux(o: &Opts) -> Result<(), String> {
    ensure_dist(o)?;
    let out = dist_root().join("installers");
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let arch = std::env::consts::ARCH;
    for p in dist::PROFILES {
        let tar = out.join(format!(
            "spiderhead-{}-{}-linux-{arch}.tar.gz",
            version(),
            p.name
        ));
        sh(
            "tar",
            &[
                "-czf",
                &tar.to_string_lossy(),
                "-C",
                &dist_root().to_string_lossy(),
                p.name,
            ],
        )?;
        println!("    {}", tar.display());
    }
    println!("==> package linux OK (deb/rpm: TODO, see docs/РАБОТА-С-RUST.md)");
    Ok(())
}

/// Windows: build the WiX source into an `.msi` (needs the `wix` tool).
fn windows() -> Result<(), String> {
    let dist = dist_root().join("all/bin");
    let msi = dist_root()
        .join("installers")
        .join(format!("SpiderHead-{}.msi", version()));
    fs::create_dir_all(msi.parent().unwrap_or(Path::new("."))).map_err(|e| e.to_string())?;
    sh(
        "wix",
        &[
            "build",
            "packaging/windows/spiderhead.wxs",
            "-d",
            &format!("BinDir={}", dist.display()),
            "-d",
            &format!("Version={}", version()),
            "-o",
            &msi.to_string_lossy(),
        ],
    )
}
