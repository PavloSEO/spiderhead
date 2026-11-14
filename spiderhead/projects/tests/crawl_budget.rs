//! competitor candidate normalisation. The settings/budget and live-crawl cases
//! of that file belong to the core/crawl slices, not to `projects`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::{json, Map, Value};
use spiderhead_core::crawl_settings::{bind_budgets_to_page_budget, load, LoadInput};
use spiderhead_core::host::FakeHost;
use spiderhead_projects::runtime;
use spiderhead_projects::workspace;

/// Per-test scratch directory, removed on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("sh-proj-budget-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

/// reference `crawl_settings.load(overrides=...)` + `bind_budgets_to_page_budget`.
fn settings(pairs: &[(&str, Value)], explicit: &[&str]) -> Value {
    let mut config = load(
        &LoadInput {
            overrides: pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect::<Map<_, _>>(),
            ..LoadInput::default()
        },
        &FakeHost::default(),
    )
    .expect("settings should load");
    bind_budgets_to_page_budget(
        &mut config,
        &explicit
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<BTreeSet<_>>(),
    );
    config
}

#[test]
fn small_crawls_are_admitted_without_approval_and_large_ones_are_not() {
    let dir = scratch("admission");
    let project = dir.0.join("site");
    workspace::create_project(&project, "https://example.test/", None, None, None, None).unwrap();
    for (pages, admitted) in [(50, true), (300, true), (1000, false)] {
        let config = settings(&[("limits.max_urls", json!(pages))], &["limits.max_urls"]);
        assert_eq!(
            runtime::admission(&project, &config, false).unwrap()["ok"],
            json!(admitted),
            "pages={pages}"
        );
    }
}

#[test]
fn competitor_url_keeps_the_host_as_entered() {
    assert_eq!(
        runtime::candidate(&json!("https://www.iana.org/")).unwrap()["url"],
        json!("https://www.iana.org/")
    );
    let row = runtime::candidate(&json!({
        "url": "HTTPS://WWW.Example.test/shop",
        "source": "list",
        "observed_at": null,
    }))
    .unwrap();
    assert_eq!(row["url"], json!("https://www.example.test/shop"));
}
