//! `crawl-site --project` error surfaces: a project whose run observation
//! document predates the current shape must fail loudly — `error: …` naming
//! the unsupported shape and exit code 1, never a silent success (issue #96).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use spiderhead_cli::{registry, run, Output};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sh-cli-projerr-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn run_cli(argv: &[String]) -> (i32, Output) {
    let tree = registry();
    let mut out = Output::default();
    let code = run(argv, &tree, &mut out).expect("cli run");
    (i32::from(code.0), out)
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

fn fresh_project(work: &Path) -> PathBuf {
    let project = work.join("p1");
    let (code, out) = run_cli(&argv(&[
        "project-new",
        "--directory",
        &project.to_string_lossy(),
        "--target",
        "https://example.org",
        "--label",
        "Demo",
    ]));
    assert_eq!(code, 0, "project-new: {}", out.stderr);
    project
}

/// An observer document in a legacy shape (an extra top-level key the current
/// validator does not know): the project crawl refuses before any request with
/// `error: …` and exit code 1, and the message names the unsupported shape
/// instead of a bare verdict.
#[test]
fn an_old_run_observation_shape_fails_the_project_crawl_loudly() {
    let work = scratch("old-shape");
    let project = fresh_project(&work);

    let project_json: Value = serde_json::from_str(
        &std::fs::read_to_string(project.join("project.json")).expect("project document"),
    )
    .expect("project json");
    let document = json!({
        "format": "spiderhead.run-observation.v1",
        "revision": 0,
        "project_uuid": project_json["project_uuid"],
        "runs": [],
        // A legacy writer's extra member: not part of the current shape.
        "legacy_extra": {"pid": 1},
    });
    std::fs::write(project.join("run-observation.json"), document.to_string())
        .expect("write observation");

    let (code, out) = run_cli(&argv(&[
        "crawl-site",
        "--project",
        &project.to_string_lossy(),
        "--approve-large-crawl",
    ]));

    assert_eq!(code, 1, "an unsupported observation shape is a failure");
    assert!(
        out.stderr
            .contains("error: run observation document has an unsupported shape"),
        "stderr: {}",
        out.stderr
    );
    // The message says which shape was expected and what was found.
    assert!(
        out.stderr
            .contains("expected top-level keys format/revision/project_uuid/runs"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("found: format, legacy_extra"),
        "stderr: {}",
        out.stderr
    );
    // Nothing was crawled: no artifact, no outcome line.
    assert!(
        !out.stderr.contains("URLs fetched"),
        "stderr: {}",
        out.stderr
    );
    let _ = std::fs::remove_dir_all(&work);
}

/// A healthy project (no observer document yet) gets past the observation
/// stage: with an unreachable origin the crawl stops its own way
/// (`robots_unavailable`) and no observation error is invented.
#[test]
fn a_healthy_project_passes_the_observation_stage() {
    let work = scratch("healthy");
    let project = fresh_project(&work);

    let (code, out) = run_cli(&argv(&[
        "crawl-site",
        "--project",
        &project.to_string_lossy(),
        "--url",
        "http://127.0.0.1:9/",
        "--approve-large-crawl",
    ]));

    assert_eq!(code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("stopped early (robots_unavailable)"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("error: run observation"),
        "stderr: {}",
        out.stderr
    );
    let _ = std::fs::remove_dir_all(&work);
}
