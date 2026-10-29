//! Parity gate: compare every recon module against fixtures emitted by the
//! reference baseline (the reference suite).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value as Json};
use spiderhead_recon::net_client::{Cookie, Response};
use spiderhead_recon::*;

fn fixture(name: &str) -> Json {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn client_of(responses: &Json) -> net_client::FakeClient {
    let mut client = net_client::FakeClient::default();
    if let Some(map) = responses.as_object() {
        for (url, spec) in map {
            let headers: Vec<(String, String)> = spec["headers"]
                .as_object()
                .map(|h| {
                    h.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let cookies = headers
                .iter()
                .filter(|(k, _)| k == "set-cookie")
                .map(|(_, v)| {
                    let pair = v.split(';').next().unwrap_or("").trim().to_string();
                    match pair.split_once('=') {
                        Some((n, val)) => Cookie {
                            name: n.trim().to_string(),
                            value: Some(val.trim().to_string()),
                        },
                        None => Cookie {
                            name: pair,
                            value: None,
                        },
                    }
                })
                .collect();
            if spec["raise"].as_str() == Some("timeout") {
                // The oracle's fake raises for this route instead of answering;
                // the key already carries any "METHOD|" prefix, so mirror it.
                client.timeouts.insert(url.clone());
                continue;
            }
            client.responses.insert(
                url.clone(),
                Response {
                    status_code: spec["status_code"].as_u64().unwrap_or(0) as u16,
                    url: spec["url"].as_str().unwrap_or(url).to_string(),
                    headers,
                    content: spec["body"].as_str().unwrap_or("").as_bytes().to_vec(),
                    cookies,
                    http_version: spec["http_version"]
                        .as_str()
                        .unwrap_or("HTTP/1.1")
                        .to_string(),
                    elapsed: std::time::Duration::ZERO,
                },
            );
        }
    }
    client
}

/// Drop fields the fake transport cannot reproduce (mirrors the gen's `_clean`).
fn clean(value: &Json) -> Json {
    match value {
        Json::Object(map) => Json::Object(
            map.iter()
                .filter(|(k, _)| {
                    !matches!(k.as_str(), "ttfb_first_ms" | "ttfb_second_ms" | "elapsed")
                })
                .map(|(k, v)| (k.clone(), clean(v)))
                .collect(),
        ),
        Json::Array(items) => Json::Array(items.iter().map(clean).collect()),
        other => other.clone(),
    }
}

fn assert_eq_diff(case: &str, expected: &Json, got: &Json) {
    if expected != got {
        panic!(
            "case {case} diverged\nexpected: {}\ngot:      {}",
            serde_json::to_string_pretty(expected).unwrap(),
            serde_json::to_string_pretty(got).unwrap()
        );
    }
}

#[test]
fn tech_parity() {
    for case in fixture("tech_oracle.json")["cases"].as_array().unwrap() {
        let i = &case["input"];
        let input = tech::AnalyzeInput {
            headers: i["headers"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            cookies: i
                .get("cookies")
                .and_then(|c| c.as_object())
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            malformed_cookies: i
                .get("malformed_cookies")
                .and_then(|m| m.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            url: i["url"]
                .as_str()
                .unwrap_or("https://example.com/")
                .to_string(),
            final_url: None,
            status_code: None,
            rendered: i["rendered"].as_bool().unwrap_or(false),
        };
        let got = tech::analyze_tech(i["html"].as_str().unwrap_or(""), &input);
        assert_eq_diff(case["name"].as_str().unwrap(), &case["expected"], &got);
    }
}

#[test]
fn tech_db_parity() {
    let dir = std::env::temp_dir().join(format!("recon-techdb-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for case in fixture("tech_db_oracle.json")["cases"].as_array().unwrap() {
        let cat = dir.join("categories.json");
        let techs = dir.join("db.json");
        std::fs::write(
            &cat,
            serde_json::to_string(&case["db"]["categories"]).unwrap(),
        )
        .unwrap();
        std::fs::write(
            &techs,
            serde_json::to_string(&case["db"]["technologies"]).unwrap(),
        )
        .unwrap();
        let path = dir.clone();
        let i = &case["input"];
        let headers: BTreeMap<String, String> = i["headers"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let cookies: BTreeMap<String, String> = i
            .get("cookies")
            .and_then(|c| c.as_object())
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let got = tech_db::detect_external(
            i["html"].as_str().unwrap_or(""),
            &headers,
            &cookies,
            &[],
            i["url"].as_str().unwrap_or(""),
            Some(path.to_str().unwrap()),
        )
        .unwrap();
        let mut expected = case["expected"].clone();
        expected["db_path"] = got["db_path"].clone();
        assert_eq_diff(case["name"].as_str().unwrap(), &expected, &got);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_bots_parity() {
    for case in fixture("ai_bots_oracle.json")["cases"].as_array().unwrap() {
        let got = ai_bots::check_ai_access(case["robots"].as_str().unwrap());
        assert_eq_diff(case["name"].as_str().unwrap(), &case["expected"], &got);
    }
}

#[test]
fn regions_parity() {
    for case in fixture("regions_oracle.json")["cases"].as_array().unwrap() {
        let got = match case["kind"].as_str().unwrap() {
            "detect_region" => regions::detect_region(case["token"].as_str().unwrap())
                .map(|s| json!(s))
                .unwrap_or(Json::Null),
            "detect_region_name" => regions::detect_region_name(case["text"].as_str().unwrap())
                .map(|s| json!(s))
                .unwrap_or(Json::Null),
            "classify_url" => regions::classify_url(
                case["url"].as_str().unwrap(),
                case["main_host"].as_str().unwrap(),
            ),
            "discover_regional_links" => json!(regions::discover_regional_links(
                case["html"].as_str().unwrap(),
                case["base_url"].as_str().unwrap()
            )),
            other => panic!("unknown kind {other}"),
        };
        assert_eq_diff(case["name"].as_str().unwrap(), &case["expected"], &got);
    }
}

#[test]
fn security_parity() {
    for case in fixture("security_oracle.json")["cases"].as_array().unwrap() {
        let url = case["url"].as_str().unwrap();
        if url.is_empty() {
            continue;
        }
        let client = client_of(&case["responses"]);
        let got = security::check_security_with(
            &client,
            url,
            case["probe_paths"].as_bool().unwrap_or(false),
        );
        assert_eq_diff(case["name"].as_str().unwrap(), &case["expected"], &got);
    }
}

#[test]
fn cdn_parity() {
    for case in fixture("cdn_oracle.json")["cases"].as_array().unwrap() {
        let url = case["url"].as_str().unwrap();
        let client = client_of(&case["responses"]);
        let got = if url.is_empty() {
            cdn::check_cdn(url, 1.0)
        } else {
            cdn::check_cdn_with(&client, url, true)
        };
        assert_eq_diff(
            case["name"].as_str().unwrap(),
            &case["expected"],
            &clean(&got),
        );
    }
}

#[test]
fn backlinks_parity() {
    for case in fixture("backlinks_oracle.json")["cases"]
        .as_array()
        .unwrap()
    {
        let client = client_of(&case["responses"]);
        let target_domain = net_client::normalize_domain(case["target"].as_str().unwrap());
        let urls: Vec<String> = case["donors"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| {
                let u = net_client::normalize_url(d.as_str().unwrap_or(""));
                (!u.is_empty()).then_some(u)
            })
            .collect();
        let dropped = case["donors"].as_array().unwrap().len() - urls.len();
        let got = backlinks::check_backlinks_with(&client, &target_domain, None, &urls, dropped);
        assert_eq_diff(case["name"].as_str().unwrap(), &case["expected"], &got);
    }
}

#[test]
fn backlink_profile_parity() {
    for case in fixture("backlink_profile_oracle.json")["cases"]
        .as_array()
        .unwrap()
    {
        let got = if case["kind"].as_str() == Some("disavow") {
            let domains: Vec<String> = case["domains"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d.as_str().unwrap().to_string())
                .collect();
            let urls: Vec<String> = case["urls"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d.as_str().unwrap().to_string())
                .collect();
            backlink_profile::export_disavow(Some(&domains), Some(&urls), None).unwrap()
        } else {
            backlink_profile::build_backlink_profile(
                case["domain"].as_str().unwrap(),
                None,
                case["evidence"].as_array().map(|a| a.as_slice()),
                None,
                None,
                case["observed_at"].as_str(),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                false,
                None,
                1000,
                0,
                None,
            )
            .unwrap()
        };
        assert_eq_diff(case["name"].as_str().unwrap(), &case["expected"], &got);
    }
}

#[test]
fn domain_parity() {
    for case in fixture("domain_oracle.json")["cases"].as_array().unwrap() {
        let rdap_result = case["rdap"].clone();
        let doh_map = case["doh"].clone();
        let whois_text = case["whois"][0].as_str().map(str::to_string);
        let whois_server = case["whois"][1].as_str().map(str::to_string);
        let tls = case["tls"].clone();
        let probes = domain::DomainProbes {
            rdap: &move |_| rdap_result.clone(),
            doh: &move |name, rtype| {
                doh_map
                    .get(format!("{name}|{rtype}").as_str())
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default()
            },
            whois: &move |_| (whois_text.clone(), whois_server.clone()),
            tls: &move |_| tls.clone(),
        };
        let got = domain::profile_domain_with(
            case["domain"].as_str().unwrap(),
            !case["tls"].is_null(),
            &probes,
        );
        let mut expected = case["expected"].clone();
        // Day counts depend on "today"; verify structurally instead.
        for key in ["age_days", "age_years", "expires_in_days"] {
            if let (Some(e), Some(g)) = (
                expected["registration"].get(key),
                got["registration"].get(key),
            ) {
                if !e.is_null() && !g.is_null() {
                    expected["registration"][key] = g.clone();
                }
            }
        }
        assert_eq_diff(case["name"].as_str().unwrap(), &expected, &got);
    }
}

#[test]
fn robots_parity() {
    for case in fixture("robots_oracle.json")["cases"].as_array().unwrap() {
        let client = client_of(&case["responses"]);
        let paths: Vec<String> = case["paths"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let got = robots_check::check_robots_with(
            &client,
            case["url"].as_str().unwrap(),
            case["user_agent"].as_str().unwrap(),
            &paths,
        );
        assert_eq_diff(case["name"].as_str().unwrap(), &case["expected"], &got);
    }
}

#[test]
fn links_parity() {
    for case in fixture("links_oracle.json")["cases"].as_array().unwrap() {
        let client = client_of(&case["responses"]);
        let got = links_check::check_links_with(
            &client,
            case["url"].as_str().unwrap(),
            case["internal_only"].as_bool().unwrap_or(false),
            case["limit"].as_u64().unwrap_or(200) as usize,
        );
        assert_eq_diff(case["name"].as_str().unwrap(), &case["expected"], &got);
    }
}

#[test]
fn hreflang_parity() {
    for case in fixture("hreflang_oracle.json")["cases"].as_array().unwrap() {
        let client = client_of(&case["responses"]);
        let got = hreflang_check::check_hreflang_with(&client, case["url"].as_str().unwrap());
        assert_eq_diff(case["name"].as_str().unwrap(), &case["expected"], &got);
    }
}

#[test]
fn redirects_parity() {
    for case in fixture("redirects_oracle.json")["cases"]
        .as_array()
        .unwrap()
    {
        let client = client_of(&case["responses"]);
        let got = redirects_check::check_chain_with(
            &client,
            case["url"].as_str().unwrap(),
            case.get("options").filter(|v| !v.is_null()),
        );
        assert_eq_diff(
            case["name"].as_str().unwrap(),
            &case["expected"],
            &Json::Array(got),
        );
    }
}

#[test]
fn llms_txt_parity() {
    for case in fixture("llms_txt_oracle.json")["cases"].as_array().unwrap() {
        let client = client_of(&case["responses"]);
        let got = llms_txt::check_llms_txt_with(
            &client,
            case["url"].as_str().unwrap(),
            case["brand"].as_str(),
        );
        assert_eq_diff(case["name"].as_str().unwrap(), &case["expected"], &got);
    }
}
