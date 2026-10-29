//! `logs_bots` with the multi-step verification arguments, and the new summary views,
//! through the MCP handlers. Offline: ranges come from a local directory.

use std::path::Path;

use serde_json::{json, Value};
use spiderhead_log_analyzer::import::{
    import_logs, ImportContext, ImportOptions, ProjectManifest, ProjectManifestLoader,
};
use spiderhead_mcp::handlers::logs::{logs_bots, logs_summary};

struct Dir;

impl ProjectManifestLoader for Dir {
    fn load(&self, project: &str) -> Result<ProjectManifest, String> {
        Ok(ProjectManifest {
            root: project.into(),
            project_uuid: "u".into(),
            target: "https://example.test/".into(),
        })
    }
}

fn artifact(tmp: &Path) -> String {
    let root = tmp.join("proj");
    std::fs::create_dir_all(&root).unwrap();
    let log = tmp.join("a.log");
    let ua = "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko); compatible; GPTBot/1.1";
    std::fs::write(
        &log,
        format!(
            "20.171.206.9 - - [10/Oct/2026:10:00:01 +0000] \"GET /a.pdf HTTP/1.1\" 200 5 \"-\" \"{ua}\"\n\
             8.8.4.4 - - [10/Oct/2026:10:00:02 +0000] \"GET /b HTTP/1.1\" 200 5 \"-\" \"{ua}\"\n"
        ),
    )
    .unwrap();
    let free = |_: &Path| Ok(u64::MAX);
    let now = || 1_790_000_000.0;
    let ctx = ImportContext {
        loader: &Dir,
        free_space: &free,
        now: &now,
    };
    let r = import_logs(
        &ctx,
        &log.to_string_lossy(),
        &root.to_string_lossy(),
        ImportOptions::default(),
    );
    r["source"].as_str().unwrap().to_string()
}

#[test]
fn verify_mode_uses_pinned_range_lists_and_reports_the_method() {
    let tmp = tempfile::tempdir().unwrap();
    let source = artifact(tmp.path());
    let ranges = tmp.path().join("ranges");
    std::fs::create_dir_all(&ranges).unwrap();
    std::fs::write(ranges.join("gptbot.json"), r#"["20.171.206.0/24"]"#).unwrap();
    let out = logs_bots(&json!({
        "source": source, "verify_mode": "cached", "ranges_dir": ranges,
    }))
    .unwrap();
    let rows = out["rows"].as_array().unwrap();
    let by_ip = |ip: &str| -> &Value { rows.iter().find(|r| r["ip"] == ip).unwrap() };
    assert_eq!(by_ip("20.171.206.9")["status"], "verified");
    assert_eq!(by_ip("20.171.206.9")["method"], "ip_list");
    assert_eq!(by_ip("8.8.4.4")["status"], "spoofed");
    let gptbot = out["ranges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["provider"] == "gptbot")
        .unwrap();
    assert_eq!(gptbot["origin"], "pinned");

    let only = logs_bots(&json!({
        "source": source, "ranges_dir": ranges, "status": "spoofed", "group": "all_ai_bots",
    }))
    .unwrap();
    assert_eq!(only["total"], 1);
}

#[test]
fn bad_verify_mode_is_refused_and_the_classic_path_is_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let source = artifact(tmp.path());
    assert!(logs_bots(&json!({"source": source, "verify_mode": "turbo"})).is_err());
    let classic = logs_bots(&json!({"source": source})).unwrap();
    assert!(classic.get("status_counts").is_none());
    assert_eq!(classic["ok"], true);
}

#[test]
fn summary_exposes_the_derived_views() {
    let tmp = tempfile::tempdir().unwrap();
    let source = artifact(tmp.path());
    let types = logs_summary(&json!({"source": source, "view": "file_types"})).unwrap();
    let names: Vec<&str> = types["file_types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["file_type"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["html", "pdf"]);
}
