//! Page-budget binding of crawl request and time budgets.
//! loader. Admission, crawl-to-scan and `_candidate` need the projects and crawl crates.

use std::collections::BTreeSet;
use std::net::SocketAddr;

use serde_json::{json, Map, Value};
use spiderhead_core::crawl_settings::{bind_budgets_to_page_budget, load, LoadInput};
use spiderhead_core::host::Host;

/// A host with no environment variables and no resolver, so `load` is deterministic.
struct IsolatedHost;

impl Host for IsolatedHost {
    fn var(&self, _name: &str) -> Option<String> {
        None
    }
    fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<SocketAddr>, String> {
        Err("no resolver in tests".to_owned())
    }
    fn zstd_available(&self) -> bool {
        false
    }
}

fn overrides(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

fn set_of(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn page_budget_derives_request_and_time_budgets_for_small_crawls() {
    let input = LoadInput {
        overrides: overrides(&[("limits.max_urls", json!(300))]),
        ..LoadInput::default()
    };
    let mut settings = load(&input, &IsolatedHost).expect("load");
    bind_budgets_to_page_budget(&mut settings, &set_of(&["limits.max_urls"]));
    assert_eq!(settings.pointer("/limits/max_requests"), Some(&json!(900)));
    assert_eq!(
        settings.pointer("/limits/max_crawl_seconds"),
        Some(&json!(600))
    );
}

#[test]
fn explicit_request_and_time_budgets_are_kept() {
    let input = LoadInput {
        overrides: overrides(&[
            ("limits.max_urls", json!(50)),
            ("limits.max_requests", json!(4000)),
            ("limits.max_crawl_seconds", json!(30)),
        ]),
        ..LoadInput::default()
    };
    let mut settings = load(&input, &IsolatedHost).expect("load");
    bind_budgets_to_page_budget(
        &mut settings,
        &set_of(&[
            "limits.max_urls",
            "limits.max_requests",
            "limits.max_crawl_seconds",
        ]),
    );
    assert_eq!(settings.pointer("/limits/max_requests"), Some(&json!(4000)));
    assert_eq!(
        settings.pointer("/limits/max_crawl_seconds"),
        Some(&json!(30))
    );
}
