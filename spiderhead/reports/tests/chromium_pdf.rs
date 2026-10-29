//! chromium_pdf ported tests: discovery order, command flags, print lifecycle
//! against real temp-file fake browsers instead of the reference injected seams.

use std::path::{Path, PathBuf};

use spiderhead_reports::chromium_pdf::{browser_command, find_browser, print_to_pdf};

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("reports-chromium-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A fake "browser": a shell script; `body` is the shell payload. It receives
/// the real browser argv, so `--print-to-pdf=…` tells it where to write.
fn fake_browser(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("fake-chrome.sh");
    std::fs::write(
        &path,
        format!("#!/bin/sh\nPDF=\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    --print-to-pdf=*) PDF=\"${{arg#--print-to-pdf=}}\" ;;\n  esac\ndone\n{body}"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

#[test]
fn browser_discovery_order() {
    let dir = tmpdir("discovery");
    let present = dir.join("chrome");
    std::fs::write(&present, b"").unwrap();
    std::env::set_var("SEOHEAD_CHROME", &present);
    let found = find_browser();
    std::env::remove_var("SEOHEAD_CHROME");
    assert_eq!(
        found,
        serde_json::json!({"path": present.to_string_lossy(), "source": "SEOHEAD_CHROME"})
    );

    std::env::set_var("SEOHEAD_CHROME", dir.join("no-such-browser"));
    let missing = find_browser();
    std::env::remove_var("SEOHEAD_CHROME");
    assert!(
        missing["path"].is_null()
            && missing["reason"]
                .as_str()
                .unwrap()
                .contains("SEOHEAD_CHROME")
    );
    // reference also covers standard-location and PATH branches through injected
    // is_file/which seams; the Rust discovery reads the real filesystem, so
    // those two branches are not reproducible without touching the host.
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn browser_command_keeps_the_sandbox() {
    let dir = tmpdir("command");
    let command = browser_command(
        "chrome",
        &dir.join("a.html"),
        &dir.join("a.pdf"),
        &dir.join("profile"),
    );
    assert!(!command.iter().any(|a| a == "--no-sandbox"));
    for flag in ["--headless=new", "--disable-gpu", "--no-pdf-header-footer"] {
        assert!(command.iter().any(|a| a == flag), "missing {flag}");
    }
    assert!(command.iter().any(|a| a.starts_with("--user-data-dir=")));
    assert!(command.last().unwrap().starts_with("file:"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn print_waits_for_a_pdf_written_after_the_command_returns() {
    let dir = tmpdir("late");
    let html = dir.join("report.html");
    std::fs::write(&html, "<html></html>").unwrap();
    let target = dir.join("report.pdf");
    // The command exits at once; the PDF only appears after it returned.
    let browser = fake_browser(
        &dir,
        "( sleep 0.4; printf '%s' '%PDF-1.7 late-write' > \"$PDF\" ) &\nexit 0\n",
    );
    let result = print_to_pdf(&html, &target, Some(browser.to_str().unwrap()), 30.0);
    assert_eq!(result["status"].as_str().unwrap(), "ok");
    assert!(std::fs::read(&target).unwrap().starts_with(b"%PDF-"));
    assert!(std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .all(|e| !e.file_name().to_string_lossy().contains("partial")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn print_reports_failure_when_no_pdf_appears() {
    let dir = tmpdir("failure");
    let html = dir.join("report.html");
    std::fs::write(&html, "<html></html>").unwrap();

    let notpdf = fake_browser(
        &dir,
        "printf '%s' '<html>not a pdf</html>' > \"$PDF\"\nexit 0\n",
    );
    let result = print_to_pdf(
        &html,
        &dir.join("r.pdf"),
        Some(notpdf.to_str().unwrap()),
        30.0,
    );
    assert_eq!(result["status"].as_str().unwrap(), "failed");
    assert!(result["reason"].as_str().unwrap().contains("not a PDF"));
    assert!(!dir.join("r.pdf").exists());

    let silent = fake_browser(&dir, "exit 0\n");
    let timed = print_to_pdf(
        &html,
        &dir.join("t.pdf"),
        Some(silent.to_str().unwrap()),
        0.01,
    );
    assert_eq!(timed["status"].as_str().unwrap(), "failed");
    assert!(timed["reason"].as_str().unwrap().contains("timeout"));

    let crashed = fake_browser(&dir, "exit 1\n");
    let result = print_to_pdf(
        &html,
        &dir.join("c.pdf"),
        Some(crashed.to_str().unwrap()),
        30.0,
    );
    assert_eq!(result["status"].as_str().unwrap(), "failed");
    assert!(result["reason"]
        .as_str()
        .unwrap()
        .contains("exited without"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn print_stops_a_browser_that_keeps_running_after_the_pdf() {
    let dir = tmpdir("hang");
    let html = dir.join("report.html");
    std::fs::write(&html, "<html></html>").unwrap();
    let pidfile = dir.join("pid");
    // Chrome on macOS writes the PDF and does not exit; it is stopped, not awaited.
    let browser = fake_browser(
        &dir,
        &format!(
            "printf '%s' '%PDF-1.7 done' > \"$PDF\"\necho $$ > \"{}\"\nsleep 30\n",
            pidfile.display()
        ),
    );
    let result = print_to_pdf(
        &html,
        &dir.join("k.pdf"),
        Some(browser.to_str().unwrap()),
        30.0,
    );
    assert_eq!(result["status"].as_str().unwrap(), "ok");
    let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_owned();
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(!alive, "browser process {pid} should have been stopped");
    let _ = std::fs::remove_dir_all(&dir);
}

// the reference suite — not ported: it needs a
// real installed Chromium, which CI hosts do not guarantee.
