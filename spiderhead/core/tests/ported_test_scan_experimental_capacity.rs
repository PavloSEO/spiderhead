//! capacity-profile refusal. The NativeScan lifecycle, resume fingerprint and live crawl
//! cases are deferred (see the slice report).

use serde_json::{json, Map, Value};
use spiderhead_core::crawl_settings::{checked_url_budget, load, manifest, LoadInput};
use spiderhead_core::host::FakeHost;

/// Load default settings with the given dotted overrides.
fn load_with(overrides: Value) -> Result<Value, String> {
    let overrides: Map<String, Value> = overrides.as_object().cloned().unwrap_or_default();
    let input = LoadInput {
        file: None,
        overrides,
        base_overrides: Map::new(),
    };
    load(&input, &FakeHost::default()).map_err(|error| error.0)
}

/// Settings with the experimental synthetic profile and a URL limit.
fn experimental(pages: u64) -> Result<Value, String> {
    load_with(json!({
        "limits.max_urls": pages,
        "storage.capacity_profile": "experimental_synthetic",
    }))
}

#[test]
fn stable_ceiling_and_experimental_admission_are_explicit() {
    for pages in [50_001_u64, 100_000, 1_000_000] {
        let loaded = load_with(json!({ "limits.max_urls": pages })).expect("stable load");
        assert_eq!(loaded["limits"]["max_urls"], json!(pages));
        assert_eq!(checked_url_budget(pages, false).expect("budget"), pages);

        let resolved = experimental(pages).expect("experimental load");
        assert_eq!(resolved["limits"]["max_urls"], json!(pages));
        let fields = manifest(&resolved, &FakeHost::default()).expect("manifest");
        assert_eq!(
            fields["storage.capacity_profile"],
            json!("experimental_synthetic")
        );
    }

    let above = experimental(1_000_001).expect_err("above ceiling refused");
    assert!(above.contains("ceiling"), "error: {above}");

    let budget = checked_url_budget(1_000_001, false).expect_err("budget refused");
    assert!(budget.0.contains("ceiling"), "error: {}", budget.0);

    let profile = load_with(json!({ "storage.capacity_profile": "unbounded" }))
        .expect_err("unknown profile refused");
    assert!(
        profile.contains("storage.capacity_profile"),
        "error: {profile}"
    );
}
