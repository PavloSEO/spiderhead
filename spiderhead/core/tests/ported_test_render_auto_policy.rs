//! Ports of the reference suite that the core crate can express.
//!
//! The two `little_text` tests are deferred: `render_tool.little_text` lives in the checks
//! crate, which has no Rust module yet (see the slice report).

use serde_json::{json, Map, Value};
use spiderhead_core::crawl_settings::{load, LoadInput};
use spiderhead_core::host::FakeHost;

/// Resolve crawl settings from explicit dotted overrides only, with no environment.
fn resolve(pairs: &[(&str, Value)]) -> Value {
    let overrides: Map<String, Value> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect();
    load(
        &LoadInput {
            overrides,
            ..LoadInput::default()
        },
        &FakeHost::default(),
    )
    .expect("settings should load")
}

#[test]
fn auto_is_an_accepted_escalation_policy() {
    let resolved = resolve(&[("rendering.escalation.policy", json!("auto"))]);
    assert_eq!(resolved["rendering"]["escalation"]["policy"], "auto");
}

#[test]
fn sampled_stays_the_default_policy() {
    let resolved = resolve(&[]);
    assert_eq!(resolved["rendering"]["escalation"]["policy"], "sampled");
}
