//! The `sources_db.sync` history test defers (sources_db is not ported), and
//! the cli/handler test defers to those slices.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::json;
use spiderhead_projects::bindings::{
    cloudflare_history_state, link, list_bindings, source_readiness, unlink,
};
use spiderhead_projects::workspace::{create_project, open_project};

fn project(dir: &testkit::Scratch) -> std::path::PathBuf {
    let root = dir.0.join("example");
    create_project(&root, "https://example.test/", None, None, None, None).expect("create");
    root
}

#[test]
fn link_list_and_reopen_keep_the_binding_in_place() {
    let dir = testkit::scratch("sources", "link-list");
    let root = project(&dir);
    let created = link(
        &root,
        "gsc",
        "sc-domain:example.test",
        Some("Main property"),
    )
    .expect("link");
    assert_eq!(created["created"], true);
    assert_eq!(created["binding"]["origin"], "manual");
    assert_eq!(created["binding"]["status"], "selected");

    let listed = list_bindings(&root, None).expect("list");
    assert_eq!(listed["count"], 1);
    assert_eq!(listed["bindings"][0]["resource"], "sc-domain:example.test");

    assert_eq!(open_project(&root, None).expect("open")["ok"], true);
    assert_eq!(
        list_bindings(&root, None).expect("list")["bindings"],
        listed["bindings"]
    );
}

#[test]
fn project_json_is_never_rewritten() {
    let dir = testkit::scratch("sources", "manifest-kept");
    let root = project(&dir);
    let before = std::fs::read(root.join("project.json")).expect("manifest");
    link(&root, "ga4", "123456", None).expect("link");
    assert_eq!(
        std::fs::read(root.join("project.json")).expect("manifest"),
        before
    );
}

#[test]
fn cloudflare_zone_id_can_be_bound_without_provider_discovery() {
    let dir = testkit::scratch("sources", "cloudflare");
    let root = project(&dir);
    let linked = link(&root, "cloudflare", "zone-123", Some("Example zone")).expect("link");
    assert_eq!(linked["binding"]["service"], "cloudflare");
    assert_eq!(linked["binding"]["resource"], "zone-123");
}

#[test]
fn binding_readiness_preserves_explicit_no_access_without_guessing_from_failures() {
    let as_of = jiff::civil::date(2026, 10, 12);
    // gsc lag_days is 3, matching `sources_db.SOURCES["gsc"].lag_days`.
    let end = as_of.checked_sub(jiff::Span::new().days(3)).expect("end");
    let start = end.checked_sub(jiff::Span::new().days(27)).expect("start");
    let days: Vec<String> = (0..28)
        .map(|offset| {
            start
                .checked_add(jiff::Span::new().days(offset))
                .unwrap()
                .to_string()
        })
        .collect();
    let mut entry = json!({
        "source": "gsc",
        "coverage": {"complete": days, "empty": []},
        "last_attempts": {days[27].clone(): "no_access"},
    });

    let readiness = source_readiness(&entry, Some(as_of)).expect("readiness");
    assert_eq!(readiness["state"], "no_access");
    assert_eq!(readiness["data_28d"], true);
    assert_eq!(readiness["access_state"], "no_access");

    entry["last_attempts"] = json!({days[27].clone(): "failed"});
    let unknown = source_readiness(&entry, Some(as_of)).expect("readiness");
    assert_eq!(unknown["access_state"], "unknown");
}

#[test]
fn cloudflare_history_readiness_keeps_partial_snapshots_unavailable_for_full_coverage() {
    let as_of = jiff::civil::date(2026, 10, 12);
    let mut row = json!({
        "zone_id": "zone-123",
        "zone": "example.test",
        "latest_observed_at": "2026-10-12T08:00:00Z",
        "latest_period": {"from": "2026-09-15", "to": "2026-10-11"},
        "state": "complete",
        "sampled": false,
        "truncated": false,
    });

    let complete = cloudflare_history_state(&row, as_of);
    row["sampled"] = json!(true);
    let partial = cloudflare_history_state(&row, as_of);

    assert_eq!(complete["history_state"], "data_28d");
    assert_eq!(complete["coverage_28d"], true);
    assert_eq!(partial["history_state"], "partial_28d");
    assert_eq!(partial["access_state"], "unknown");
}

#[test]
fn relinking_the_same_pair_is_idempotent() {
    let dir = testkit::scratch("sources", "relink");
    let root = project(&dir);
    link(&root, "metrika", "42", None).expect("link");
    let again = link(&root, "metrika", "42", None).expect("relink");
    assert_eq!(again["created"], false);
    assert_eq!(list_bindings(&root, None).expect("list")["count"], 1);
}

#[test]
fn unlink_removes_and_missing_pair_is_a_clear_error() {
    let dir = testkit::scratch("sources", "unlink");
    let root = project(&dir);
    link(&root, "bing", "https://example.test/", None).expect("link");
    let removed = unlink(&root, "bing", "https://example.test/").expect("unlink");
    assert_eq!(removed["removed"]["service"], "bing");
    assert_eq!(list_bindings(&root, None).expect("list")["count"], 0);
    let err = unlink(&root, "bing", "https://example.test/").expect_err("missing pair");
    assert!(
        err.to_string().contains("no such source binding"),
        "got: {err}"
    );
}

#[test]
fn unknown_service_and_unsafe_resources_are_refused() {
    let dir = testkit::scratch("sources", "refusals");
    let root = project(&dir);
    let err = link(&root, "yandex", "1", None).expect_err("unknown service");
    assert!(err.to_string().contains("unknown service"), "got: {err}");
    for resource in ["", &"x".repeat(513), "line\nbreak"] {
        let err = link(&root, "gsc", resource, None).expect_err("unsafe resource");
        assert!(err.to_string().contains("resource"), "got: {err}");
    }
    let err = link(&root, "gsc", "a", Some("   ")).expect_err("blank label");
    assert!(err.to_string().contains("label"), "got: {err}");
    assert!(!root.join("sources.json").exists());
}

#[cfg(unix)]
#[test]
fn symlinked_sources_file_is_refused_on_read_and_write() {
    let dir = testkit::scratch("sources", "symlink");
    let root = project(&dir);
    let outside = dir.0.join("outside.json");
    std::fs::write(&outside, "{}").expect("outside");
    std::os::unix::fs::symlink(&outside, root.join("sources.json")).expect("symlink");
    let err = list_bindings(&root, None).expect_err("symlinked list refuses");
    assert!(err.to_string().contains("unsafe"), "got: {err}");
    let err = link(&root, "gsc", "a", None).expect_err("symlinked link refuses");
    assert!(
        err.to_string().contains("unsafe") || err.to_string().contains("symlink"),
        "got: {err}"
    );
    assert_eq!(std::fs::read_to_string(&outside).expect("outside"), "{}");
}

#[test]
fn invalid_project_is_refused_before_any_write() {
    let dir = testkit::scratch("sources", "invalid-project");
    let root = project(&dir);
    std::fs::write(root.join("project.json"), "{\"version\": 999}").expect("bad manifest");
    link(&root, "gsc", "a", None).expect_err("invalid project refuses");
    assert!(!root.join("sources.json").exists());
}
