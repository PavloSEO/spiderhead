//! Tests ported from the reference baseline suite. Each carries a
//! `// from tests/<file>the reference suite::<test>` marker; fixture-backed behaviour lives in
//! `parity_oracle.rs` instead of being duplicated here.

use serde_json::{json, Value as Json};
use spiderhead_core::net::{whois_field, whois_server_for_tld, WHOIS_REFERRAL_KEYS};
use spiderhead_recon::domain::{profile_domain_with, whois_record_is_about, DomainProbes};
use spiderhead_recon::net_client::{whois_text_with, Cookie, Response};
use spiderhead_recon::*;

const ZONE_RECORD: &str = "\
% By submitting a query you agree not to use the information
domain:      RU
nserver:     a.dns.ripn.net
created:     1994-04-07T00:00:00Z
";

const DOMAIN_RECORD: &str = "\
domain:      EXAMPLE.RU
registrar:   REGRU-RU
created:     2013-09-25T20:41:47Z
nserver:     ns1.example.ru
";

#[test]
fn zone_record_is_rejected_for_a_domain_query() {
    assert!(!whois_record_is_about(ZONE_RECORD, "example.ru"));
}

#[test]
fn matching_record_is_accepted_case_insensitively() {
    assert!(whois_record_is_about(DOMAIN_RECORD, "example.ru"));
}

#[test]
fn trailing_dot_does_not_break_the_match() {
    assert!(whois_record_is_about("domain: example.ru.\n", "example.ru"));
}

#[test]
fn record_without_an_identity_field_is_rejected() {
    assert!(!whois_record_is_about(
        "created: 2013-09-25\nnserver: ns1.example.ru\n",
        "example.ru"
    ));
}

#[test]
fn gtld_style_domain_name_key_is_understood() {
    assert!(whois_record_is_about(
        "Domain Name: EXAMPLE.COM\n",
        "example.com"
    ));
}

#[test]
fn a_different_domain_is_rejected() {
    assert!(!whois_record_is_about("domain: other.ru\n", "example.ru"));
}

/// The `_profile_with_whois` helper: every network boundary is an offline
/// fixture through [`DomainProbes`].
fn profile_with_whois(domain: &str, text: &'static str) -> Json {
    let probes = DomainProbes {
        rdap: &|_| json!({"supported": false, "error": "absent"}),
        doh: &|_, _| Vec::new(),
        whois: &|_| (Some(text.to_string()), Some("whois.example".to_string())),
        tls: &|_| Json::Null,
    };
    profile_domain_with(domain, false, &probes)
}

#[test]
fn rate_limited_whois_does_not_claim_a_zone_record() {
    let profile = profile_with_whois(
        "example.ru",
        "Your connection limit exceeded. Try again after 5 minutes.",
    );
    assert_eq!(profile["registration"]["source"], "none");
    assert_eq!(
        profile["registration"]["whois_note"],
        "WHOIS response did not identify the requested domain; no registration data was taken from it"
    );
    let flags = profile["flags"].to_string().to_lowercase();
    assert!(!flags.contains("zone"));
}

#[test]
fn whitespace_whois_response_is_not_described_as_another_record() {
    let profile = profile_with_whois("example.ru", " \n\t ");
    assert_eq!(profile["registration"]["source"], "none");
    assert!(profile["registration"]["whois_note"]
        .as_str()
        .unwrap()
        .starts_with("WHOIS response did not identify"));
    let flags = profile["flags"].to_string().to_lowercase();
    assert!(!flags.contains("another object"));
}

#[test]
fn zone_record_is_reported_without_naming_an_unobserved_cause() {
    let profile = profile_with_whois("example.ru", ZONE_RECORD);
    assert_eq!(profile["registration"]["source"], "none");
    assert!(profile["registration"]["whois_note"]
        .as_str()
        .unwrap()
        .starts_with("WHOIS response did not identify"));
    let flags = profile["flags"].to_string().to_lowercase();
    assert!(!flags.contains("zone"));
}

#[test]
fn punycode_whois_identity_still_supplies_registration_data() {
    let profile = profile_with_whois(
        "пример.рф",
        "domain: XN--E1AFMKFD.XN--P1AI\ncreated: 2013-09-25\n",
    );
    assert_eq!(profile["domain"], "xn--e1afmkfd.xn--p1ai");
    assert_eq!(profile["registration"]["source"], "whois");
    assert!(profile["registration"].get("whois_note").is_none());
}

#[test]
fn referral_field_is_extracted() {
    let text = "domain: RU\nwhois: whois.tcinet.ru\n";
    assert_eq!(
        whois_field(text, WHOIS_REFERRAL_KEYS).as_deref(),
        Some("whois.tcinet.ru")
    );
}

#[test]
fn comment_lines_are_not_parsed_as_fields() {
    let text = "% whois: evil.example\ndomain: EXAMPLE.RU\n";
    assert!(whois_field(text, WHOIS_REFERRAL_KEYS).is_none());
}

#[test]
fn ru_is_mapped_to_the_registry_server() {
    assert_eq!(whois_server_for_tld("ru"), Some("whois.tcinet.ru"));
    assert_eq!(whois_server_for_tld("xn--p1ai"), Some("whois.tcinet.ru"));
}

#[test]
fn a_referral_that_is_not_a_hostname_is_never_executed() {
    // The server comes from a registry response, so it is untrusted input.
    let calls = std::cell::RefCell::new(Vec::<Vec<String>>::new());
    let which = |_: &str| Some("/usr/bin/whois".to_string());
    let run = |argv: &[String], _t: f64| {
        calls.borrow_mut().push(argv.to_vec());
        Some("domain: EXAMPLE.RU\n".to_string())
    };
    assert_eq!(
        whois_text_with(
            "example.ru",
            1.0,
            Some("evil.example; rm -rf /"),
            &which,
            &run
        ),
        None
    );
    assert!(calls.borrow().is_empty());
}

#[test]
fn a_valid_server_is_passed_as_arguments_not_through_a_shell() {
    let calls = std::cell::RefCell::new(Vec::<Vec<String>>::new());
    let which = |_: &str| Some("/usr/bin/whois".to_string());
    let run = |argv: &[String], _t: f64| {
        calls.borrow_mut().push(argv.to_vec());
        Some("domain: EXAMPLE.RU\n".to_string())
    };
    whois_text_with("example.ru", 1.0, Some("whois.tcinet.ru"), &which, &run);
    assert_eq!(
        calls.borrow()[0],
        vec![
            "/usr/bin/whois".to_string(),
            "-h".to_string(),
            "whois.tcinet.ru".to_string(),
            "example.ru".to_string()
        ]
    );
}

// ── regions (the reference suite) ──────────────────────────────────────────

#[test]
fn translit_variants_collapse_into_one_region() {
    for token in ["msk", "moskva", "moscow"] {
        assert_eq!(regions::detect_region(token).as_deref(), Some("Москва"));
    }
}

#[test]
fn region_is_found_inside_a_compound_token() {
    assert_eq!(
        regions::detect_region("ru-moscow").as_deref(),
        Some("Москва")
    );
}

#[test]
fn unknown_token_is_none_not_a_guess() {
    assert!(regions::detect_region("zxq").is_none());
}

#[test]
fn service_subdomains_are_not_regions() {
    for token in ["mail", "api", "blog"] {
        assert!(regions::detect_region(token).is_none());
    }
}

#[test]
fn registrable_domain_handles_compound_zones() {
    let out = regions::classify_url("https://spb.example.co.uk", "example.co.uk");
    assert_eq!(out["scheme"], "subdomain");
    assert_eq!(out["region"], "Санкт-Петербург");
}

// test_separate_domain_is_a_satellite / test_www_and_ordinary_pages_are_the_main_site /
// test_common_first_segments_are_not_regions
#[test]
fn classify_url_schemes() {
    let cases = [
        (
            "https://msk.example.ru/",
            "example.ru",
            "subdomain",
            "Москва",
        ),
        (
            "https://example.ru/spb/",
            "example.ru",
            "folder",
            "Санкт-Петербург",
        ),
        (
            "https://site-ekb.ru/",
            "example.ru",
            "domain",
            "Екатеринбург",
        ),
        ("https://www.example.ru/", "example.ru", "main", ""),
        ("https://example.ru/", "example.ru", "main", ""),
        ("https://example.ru/blog/msk", "example.ru", "main", ""),
    ];
    for (url, main, scheme, region) in cases {
        let out = regions::classify_url(url, main);
        assert_eq!(out["scheme"], scheme, "{url}");
        assert_eq!(out["region"].as_str().unwrap_or(""), region, "{url}");
    }
}

#[test]
fn switcher_links_are_found_and_deduplicated() {
    let html = r#"<a href="https://msk.example.ru/">Москва</a>
                  <a href="https://msk.example.ru/">Москва повтор</a>"#;
    let links = regions::discover_regional_links(html, "https://example.ru/");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["region"], "Москва");
}

// ── ai_bots (the reference suite) ──────────────────────────────────────────

#[test]
fn gptbot_blocked_by_disallow_root() {
    let out = ai_bots::check_ai_access("User-agent: GPTBot\nDisallow: /\n");
    let gpt = out["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["token"] == "GPTBot")
        .unwrap();
    assert_eq!(gpt["status"], "blocked");
}

#[test]
fn bot_allowed_by_default_when_not_declared() {
    let out = ai_bots::check_ai_access("User-agent: *\nDisallow: /admin\n");
    let gpt = out["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["token"] == "GPTBot")
        .unwrap();
    assert_eq!(gpt["status"], "allowed_default");
}

#[test]
fn bot_explicitly_allowed() {
    let out =
        ai_bots::check_ai_access("User-agent: *\nDisallow: /\nUser-agent: GPTBot\nAllow: /\n");
    let gpt = out["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["token"] == "GPTBot")
        .unwrap();
    assert_eq!(gpt["status"], "allowed_explicit");
}

#[test]
fn summary_counts_match() {
    let out = ai_bots::check_ai_access("User-agent: GPTBot\nDisallow: /\n");
    let summary = &out["summary"];
    let blocked = summary["blocked"].as_u64().unwrap();
    let explicit = summary["allowed_explicit"].as_u64().unwrap();
    let default = summary["allowed_default"].as_u64().unwrap();
    assert_eq!(
        blocked + explicit + default,
        summary["total"].as_u64().unwrap()
    );
    assert!(blocked >= 1);
}

#[test]
fn all_bots_cover_training_retrieval_user() {
    let out = ai_bots::check_ai_access("");
    let kinds: std::collections::BTreeSet<&str> = out["bots"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["type"].as_str())
        .collect();
    for kind in ["training", "retrieval", "user"] {
        assert!(kinds.contains(kind), "missing bot type {kind}");
    }
}

// ── tech_db (the reference suite) ──────────────────────────────────────────

#[test]
fn parse_pattern_extracts_confidence_and_version() {
    let (re, confidence, version) = tech_db::_parse_pattern("foo\\;confidence:75\\;version:\\1");
    assert!(re.is_match("FOO"));
    assert_eq!(confidence, 75);
    assert_eq!(version.as_deref(), Some("\\1"));
}

// test_load_db_returns_none_when_not_a_db
#[test]
fn load_db_reads_categories_and_technologies() {
    let dir = std::env::temp_dir().join(format!("recon-load-db-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("categories.json"), r#"{"1": {"name": "CMS"}}"#).unwrap();
    std::fs::write(dir.join("a.json"), r#"{"Demo": {"cats": [1]}}"#).unwrap();
    let db = tech_db::load_db(dir.to_str().unwrap()).unwrap();
    assert_eq!(db.categories.get("1").map(String::as_str), Some("CMS"));
    assert!(db.technologies.contains_key("Demo"));
    let empty = std::env::temp_dir().join(format!("recon-load-empty-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    assert!(tech_db::load_db(empty.to_str().unwrap()).is_none());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

#[test]
fn implied_names_cover_string_list_and_negation() {
    assert_eq!(tech_db::_implied_names(&json!("PHP")), vec!["PHP"]);
    assert_eq!(
        tech_db::_implied_names(&json!(["or", "A", "B"])),
        vec!["A", "B"]
    );
    assert!(tech_db::_implied_names(&json!(["not", "A"])).is_empty());
    assert_eq!(
        tech_db::_implied_names(&json!(["A", ["or", "B"]])),
        vec!["A", "B"]
    );
}

// ── tech (the reference suite, the reference suite) ─────────

#[test]
fn signatures_have_valid_kinds_and_nonempty_markers() {
    let valid = ["header", "value", "cookie", "html", "script"];
    assert!(!tech::SIGNATURES.is_empty());
    for (_, _, kind, marker) in tech::SIGNATURES.iter() {
        assert!(valid.contains(&kind.as_str()), "kind {kind}");
        assert!(!marker.trim().is_empty());
    }
}

#[test]
fn signatures_have_no_exact_duplicates() {
    let mut seen = std::collections::BTreeSet::new();
    for sig in tech::SIGNATURES.iter() {
        assert!(seen.insert(sig), "duplicate {sig:?}");
    }
}

#[test]
fn analyze_tech_captures_identifier_not_just_name() {
    let html =
        r#"<script async src="https://www.googletagmanager.com/gtag/js?id=G-TEST123"></script>"#;
    let out = tech::analyze_tech(html, &tech::AnalyzeInput::default());
    let ga = out["technologies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| {
            t["name"]
                .as_str()
                .map(|n| n.contains("Google"))
                .unwrap_or(false)
        })
        .cloned()
        .unwrap_or(Json::Null);
    let ids: Vec<&str> = ga["identifiers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(ids.iter().any(|i| i.contains("G-TEST123")), "{ga}");
}

#[test]
fn analyze_tech_stamps_measurement_representation() {
    let static_out = tech::analyze_tech("<html></html>", &tech::AnalyzeInput::default());
    assert_eq!(static_out["measurement"]["representation"], "static_markup");
    let rendered = tech::AnalyzeInput {
        rendered: true,
        ..Default::default()
    };
    let dom_out = tech::analyze_tech("<html></html>", &rendered);
    assert_eq!(dom_out["measurement"]["representation"], "rendered_dom");
}

#[test]
fn tag_coverage_excludes_failed_fetches_from_denominator() {
    let page = tech::analyze_tech(
        r#"<script async src="https://www.googletagmanager.com/gtag/js?id=G-1"></script>"#,
        &tech::AnalyzeInput {
            url: "https://x.example/a".to_string(),
            ..Default::default()
        },
    );
    let failed = json!({"ok": false, "url": "https://x.example/b"});
    let out = tech::tag_coverage(&[page, failed], Some(&["Google Analytics 4".to_string()]));
    assert_eq!(out["pages_considered"], 1);
    assert_eq!(out["pages_excluded_fetch_failed"], 1);
    assert_eq!(out["tags"][0]["fraction"], 1.0);
}

#[test]
fn tag_coverage_reports_uniform_and_mixed_measurement_stamp() {
    let page = |rendered: bool| {
        let mut p = tech::analyze_tech(
            "<html></html>",
            &tech::AnalyzeInput {
                url: "https://x.example/".to_string(),
                rendered,
                ..Default::default()
            },
        );
        p["ok"] = json!(true);
        p
    };
    let uniform = tech::tag_coverage(&[page(false), page(false)], None);
    assert_eq!(uniform["measurement_stamp"], "static_markup");
    let mixed = tech::tag_coverage(&[page(false), page(true)], None);
    let stamps: Vec<&str> = mixed["measurement_stamp"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(stamps, vec!["rendered_dom", "static_markup"]);
}

// ── response cookies / entry points (the reference suite) ────────

// test_security_rejects_an_empty_url_without_a_request
#[test]
fn empty_urls_are_rejected_without_a_request() {
    assert_eq!(cdn::check_cdn("", 0.1)["ok"], false);
    assert_eq!(security::check_security("", false, 0.1)["ok"], false);
    assert_eq!(tech::detect_tech("", 0.1)["ok"], false);
    assert_eq!(cdn::check_cdn("", 0.1)["error"], "not a valid URL: ''");
}

#[test]
fn network_failure_is_result_data_not_a_panic() {
    let client = net_client::FakeClient::default();
    let out = cdn::check_cdn_with(&client, "https://down.example/", true);
    assert_eq!(out["ok"], false);
    assert!(out["error"].is_string());
}

#[test]
fn security_failed_redirect_probe_is_data_not_an_error() {
    let mut client = net_client::FakeClient::default();
    client.responses.insert(
        "https://example.com/".to_string(),
        Response {
            status_code: 200,
            url: "https://example.com/".to_string(),
            headers: vec![],
            content: b"<html></html>".to_vec(),
            cookies: vec![Cookie {
                name: "sid".to_string(),
                value: Some("x".to_string()),
            }],
            http_version: "HTTP/1.1".to_string(),
            elapsed: std::time::Duration::ZERO,
        },
    );
    let out = security::check_security_with(&client, "https://example.com/", false);
    assert_eq!(out["ok"], true);
    assert_eq!(out["grade"], "F");
    assert_eq!(out["https_redirect"]["checked"], false);
    assert!(out["https_redirect"]["reason"]
        .as_str()
        .unwrap()
        .contains("no route"));
}
