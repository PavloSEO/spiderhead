//! Tests for the user-agent identity that depend only on this module.

use super::*;

// The cross-module half, against the check and recon identities, is tested in those modules.
#[test]
fn crawler_identity_is_the_toolkit_and_distinct_from_check_identities() {
    assert!(DEFAULT_USER_AGENT.starts_with("SpiderHeadBot/"));
    assert_ne!(DEFAULT_USER_AGENT, CHECKS_PARSER_USER_AGENT);
    assert_ne!(DEFAULT_USER_AGENT, CHECKS_DOWNLOADER_USER_AGENT);
    assert_ne!(DEFAULT_USER_AGENT, RECON_USER_AGENT);
}

#[test]
fn check_and_recon_identities_are_pinned_byte_for_byte() {
    assert_eq!(
        CHECKS_DOWNLOADER_USER_AGENT,
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"
    );
    assert_eq!(
        CHECKS_PARSER_USER_AGENT,
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
         (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"
    );
    assert_eq!(
        RECON_USER_AGENT,
        "Mozilla/5.0 (compatible; SEOHEAD-Tools/3.0; +https://seohead.tech/seotools)"
    );
}

#[test]
fn presets_custom_and_default_identity() {
    let default = resolve(None).expect("default resolves");
    assert_eq!(default.user_agent, DEFAULT_USER_AGENT);
    assert_eq!(default.robots_token, DEFAULT_TOKEN);
    assert!(DEFAULT_USER_AGENT.starts_with("SpiderHeadBot/"));
    assert!(DEFAULT_USER_AGENT.contains("github.com/PavloSEO/spiderhead"));

    let google = resolve(Some("googlebot")).expect("preset resolves");
    assert_eq!(google.robots_token, "Googlebot");
    assert!(google.user_agent.contains("Googlebot/2.1"));

    let bing = resolve(Some("bingbot")).expect("preset resolves");
    assert_eq!(bing.robots_token, "bingbot");

    let custom = resolve(Some("My-Crawler/2.0 (+https://example.test)")).expect("custom resolves");
    assert_eq!(custom.user_agent, "My-Crawler/2.0 (+https://example.test)");
    assert_eq!(custom.robots_token, "My-Crawler");

    assert_eq!(resolve(Some("bad\r\nheader")), Err(UserAgentError));
}

// Identity resolution only; loading it into the crawl settings is tested separately.
#[test]
fn default_identity_fills_empty_fields_and_custom_agent_sets_token() {
    let mut http = Map::new();
    http.insert("user_agent".to_owned(), Value::String(String::new()));
    let mut robots = Map::new();
    robots.insert("user_agent_token".to_owned(), Value::String(String::new()));
    assert!(apply_defaults(&mut http, &mut robots).is_ok());
    assert_eq!(http["user_agent"], DEFAULT_USER_AGENT);
    assert_eq!(robots["user_agent_token"], DEFAULT_TOKEN);

    let mut http = Map::new();
    http.insert(
        "user_agent".to_owned(),
        Value::String("MyBot/1.0 (x)".to_owned()),
    );
    let mut robots = Map::new();
    assert!(apply_defaults(&mut http, &mut robots).is_ok());
    assert_eq!(robots["user_agent_token"], "MyBot");
}

#[test]
fn legacy_token_aliases_only_for_the_default_identity() {
    assert!(same_robots_identity("SpiderHeadBot", "seohead-tools"));
    assert!(same_robots_identity("spiderheadbot", "SEOHEAD-TOOLS"));
    assert!(!same_robots_identity("Googlebot", "SpiderHeadBot"));
    assert_eq!(
        token_aliases("SpiderHeadBot")
            .into_iter()
            .collect::<Vec<_>>(),
        vec!["seohead-tools".to_owned(), "spiderheadbot".to_owned()]
    );
}

#[test]
fn preset_table_is_complete_and_sorted_names_are_unique() {
    let names = preset_names();
    assert_eq!(names.len(), 19);
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(names.contains(&DEFAULT_PRESET));
}
