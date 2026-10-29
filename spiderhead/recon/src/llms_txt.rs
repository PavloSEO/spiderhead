//! `llms.txt` scorer (`llms-txt-check`): fetch a site's `llms.txt` manifest and
//! score it against nine binary signals of model usefulness.

use regex::Regex;
use serde_json::{json, Value as Json};
use spiderhead_parse::py_url::urlsplit;

use crate::net_client::{normalize_url, py_repr, ReconClient};

/// `60 KiB`: beyond this the file becomes cumbersome for model context windows.
const MAX_BYTES: usize = 60 * 1024;

/// Category words whose presence marks a product/service/teaser vocabulary.
/// ASCII keywords match whole words (optional plural `s`); Cyrillic stems match
/// as substrings.
const CATEGORY_HINTS: [&str; 13] = [
    "продукт",
    "product",
    "сервис",
    "service",
    "платформ",
    "platform",
    "инструмент",
    "tool",
    "решен",
    "solution",
    "saas",
    "api",
    "marketplace",
];

/// ASCII keyword → whole-word (optional plural) match; Cyrillic stem → substring.
fn mentions(text_low: &str, words: &[&str]) -> bool {
    words.iter().any(|word| {
        if word.is_ascii() {
            let pattern = format!(r"\b{}s?\b", regex::escape(word));
            match Regex::new(&pattern) {
                Ok(re) => re.is_match(text_low),
                Err(_) => false,
            }
        } else {
            text_low.contains(word)
        }
    })
}

/// Score `llms.txt` content against nine checks without network access.
///
/// `brand` supplies the project name for checks one and four. Without it,
/// check four is deliberately reported as failed rather than inferred, and
/// check one scores mere heading presence.
pub fn score_llms_txt(content: &str, brand: Option<&str>) -> Json {
    if content.is_empty() {
        return json!({"ok": false, "error": "llms.txt is empty or missing"});
    }
    let brand = brand.filter(|b| !b.is_empty());
    let text = content;
    let text_low = text.to_lowercase();
    // Blank segments between CR and LF never match a heading, so extra empty
    // lines next to line splitting cannot change any counter.
    let lines: Vec<&str> = text.split(['\n', '\r']).collect();

    // First heading line, `# ` after optional leading whitespace, non-empty name.
    let h1_text = lines.iter().find_map(|ln| {
        let stripped = ln.trim_start();
        if !stripped.starts_with("# ") {
            return None;
        }
        let name = stripped.trim_matches(|c| c == '#' || c == ' ');
        (!name.is_empty()).then(|| name.to_string())
    });
    let (h1_check_name, has_h1) = match brand {
        Some(brand) => (
            format!("H1 heading names the project ({brand})"),
            h1_text
                .as_ref()
                .map(|h| h.to_lowercase().contains(&brand.to_lowercase()))
                .unwrap_or(false),
        ),
        None => ("H1 heading is present".to_string(), h1_text.is_some()),
    };
    let sections = lines
        .iter()
        .filter(|ln| ln.trim_start().starts_with("## "))
        .count();
    let link_re = Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").ok();
    let links: Vec<(String, String)> = link_re
        .map(|re| {
            re.captures_iter(text)
                .map(|c| (c[1].to_string(), c[2].to_string()))
                .collect()
        })
        .unwrap_or_default();

    let mentions_brand = brand
        .map(|b| text_low.contains(&b.to_lowercase()))
        .unwrap_or(false);
    let mentions_category = mentions(&text_low, &CATEGORY_HINTS);

    let link_text_low = format!(
        "{} {}",
        links
            .iter()
            .map(|(anchor, _)| anchor.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        text_low
    );
    let has_product = mentions(
        &link_text_low,
        &["product", "pricing", "продукт", "тариф", "цен"],
    );
    let has_proof = mentions(
        &link_text_low,
        &[
            "case",
            "testimonial",
            "review",
            "customer",
            "кейс",
            "отзыв",
            "клиент",
        ],
    );
    let has_docs = mentions(
        &link_text_low,
        &[
            "docs",
            "documentation",
            "api",
            "guide",
            "документац",
            "руководств",
        ],
    );

    let size_bytes = content.len();
    let size_ok = size_bytes <= MAX_BYTES;

    let checks = json!([
        {"name": h1_check_name, "passed": has_h1},
        {"name": "At least 3 H2 sections", "passed": sections >= 3},
        {"name": "At least 3 Markdown links", "passed": links.len() >= 3},
        {
            "name": match brand {
                Some(b) => format!("Brand mention ({b})"),
                None => "Brand mention".to_string(),
            },
            "passed": mentions_brand,
        },
        {"name": "Product or service category mention", "passed": mentions_category},
        {"name": "Product or pricing page", "passed": has_product},
        {"name": "Proof: cases, testimonials, reviews, or customers", "passed": has_proof},
        {"name": "Documentation, API, or guide", "passed": has_docs},
        {"name": format!("Size at most {} KiB", MAX_BYTES / 1024), "passed": size_ok},
    ]);
    let passed = checks
        .as_array()
        .map(|list| list.iter().filter(|c| c["passed"] == json!(true)).count())
        .unwrap_or(0);
    let total = checks.as_array().map(Vec::len).unwrap_or(0);
    // passed/total * 10 rounded to one decimal; no value lands on a .x5 tie,
    // so round-half-up matches the reference rounding here.
    let score = ((passed as f64 / total as f64) * 10.0 * 10.0).round() / 10.0;
    let grade = if score >= 8.0 {
        "A"
    } else if score >= 6.0 {
        "B"
    } else if score >= 4.0 {
        "C"
    } else if score >= 2.0 {
        "D"
    } else {
        "F"
    };

    json!({
        "ok": true,
        "score": score,
        "grade": grade,
        "passed": passed,
        "total": total,
        "checks": checks,
        "stats": {
            "h1": has_h1,
            "sections": sections,
            "links": links.len(),
            "mentions_brand": mentions_brand,
            "size_bytes": size_bytes,
            "size_ok": size_ok,
        },
    })
}

/// Fetch and score a site's `/llms.txt` over the given client (parity seam).
pub fn check_llms_txt_with(client: &dyn ReconClient, url: &str, brand: Option<&str>) -> Json {
    let base = normalize_url(url);
    if base.is_empty() {
        return json!({"ok": false, "error": format!("Not a valid HTTP(S) URL: {}", py_repr(url))});
    }
    // llms.txt belongs at the origin root, so the supplied path is discarded.
    let parts = urlsplit(&base).unwrap_or_default();
    let llms_url = format!("{}://{}/llms.txt", parts.scheme, parts.netloc);
    let response = match client.get(&llms_url, &[], &[]) {
        Ok(response) => response,
        Err(error) => return json!({"ok": false, "url": llms_url, "error": error.to_string()}),
    };
    let status = response.status_code;
    if status == 404 {
        // A missing file is a measured result, not a tool failure: ok stays true
        // so the orchestrator can tell absence from an unavailable check.
        return json!({
            "ok": true,
            "url": llms_url,
            "status_code": status,
            "exists": false,
            "score": 0,
            "grade": "F",
            "passed": 0,
            "findings": [
                "llms.txt is missing, so the site provides no curated context for AI systems; models must infer the project from whichever pages they encounter"
            ],
        });
    }
    if status >= 400 {
        // A gate, a rate limit or an upstream fault withheld the body the same
        // way a transport error would: nothing was measured, so "exists" stays
        // out of the answer entirely.
        return json!({
            "ok": false,
            "url": llms_url,
            "status_code": status,
            "error": format!("llms.txt could not be measured: server returned {status}"),
        });
    }
    let scored = score_llms_txt(&response.text(), brand);
    let mut out = serde_json::Map::new();
    out.insert("url".into(), json!(llms_url));
    out.insert("status_code".into(), json!(status));
    out.insert("exists".into(), json!(true));
    if let Some(scored_obj) = scored.as_object() {
        for (key, value) in scored_obj {
            out.insert(key.clone(), value.clone());
        }
    }
    Json::Object(out)
}

/// Fetch and score a site's `/llms.txt` live.
pub fn check_llms_txt(url: &str, brand: Option<&str>, timeout: f64) -> Json {
    match crate::net_client::http_client(timeout) {
        Ok((client, _)) => check_llms_txt_with(&client, url, brand),
        Err(error) => json!({"ok": false, "url": url, "error": error.to_string()}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net_client::{FakeClient, Response};
    use std::collections::HashMap;

    const GOOD: &str = "# Acme Analytics Platform\n\n> Acme helps teams understand user behavior through real-time dashboards,\n> audience segments, and API reports.\n\n## Product\n- [Acme Analytics](https://example.com/product): the core analytics platform\n- [Pricing](https://example.com/pricing): plans for teams of every size\n\n## Evidence\n- [Customer cases](https://example.com/cases): how Acme accelerates growth\n\n## Documentation\n- [Docs](https://example.com/docs): integration guide\n- [API](https://example.com/api): API reference\n";

    #[test]
    fn good_llms_txt_scores_high() {
        let r = score_llms_txt(GOOD, Some("Acme"));
        assert_eq!(r["ok"], json!(true));
        assert!(r["score"].as_f64().unwrap_or(0.0) >= 7.0);
        assert!(r["stats"]["sections"].as_u64().unwrap_or(0) >= 3);
        assert!(r["stats"]["links"].as_u64().unwrap_or(0) >= 3);
        assert_eq!(r["stats"]["mentions_brand"], json!(true));
    }

    #[test]
    fn empty_content_returns_error() {
        let r = score_llms_txt("", None);
        assert_eq!(r["ok"], json!(false));
    }

    #[test]
    fn missing_sections_lower_score() {
        let bare = "# Just a title\n\nOne link [home](https://example.org).\n";
        let r = score_llms_txt(bare, Some("X"));
        assert!(r["score"].as_f64().unwrap_or(10.0) < 5.0);
        assert_eq!(r["stats"]["sections"], json!(0));
    }

    #[test]
    fn brand_mention_check() {
        let r = score_llms_txt(GOOD, Some("Acme"));
        let brand_check = &r["checks"][3];
        assert!(brand_check["name"].as_str().unwrap().contains("Acme"));
        assert_eq!(brand_check["passed"], json!(true));
    }

    #[test]
    fn oversized_content_fails_size_check() {
        let big = format!("# T\n\n{}", "x".repeat(61 * 1024));
        let r = score_llms_txt(&big, None);
        assert_eq!(r["stats"]["size_ok"], json!(false));
        let size_check = r["checks"].as_array().unwrap().last().unwrap();
        assert_eq!(size_check["passed"], json!(false));
    }

    #[test]
    fn grade_thresholds() {
        assert!(matches!(
            score_llms_txt(GOOD, Some("Acme"))["grade"]
                .as_str()
                .unwrap_or(""),
            "A" | "B"
        ));
        assert!(matches!(
            score_llms_txt("# x\n", None)["grade"]
                .as_str()
                .unwrap_or(""),
            "D" | "F"
        ));
    }

    fn client_of_one(url: &str, status: u16, body: &str) -> FakeClient {
        let mut responses = HashMap::new();
        responses.insert(
            url.to_string(),
            Response {
                status_code: status,
                url: url.to_string(),
                headers: Vec::new(),
                content: body.as_bytes().to_vec(),
                cookies: Vec::new(),
                http_version: "HTTP/1.1".into(),
                elapsed: std::time::Duration::ZERO,
            },
        );
        FakeClient {
            responses,
            ..FakeClient::default()
        }
    }

    #[test]
    fn missing_file_is_a_measured_result_not_a_tool_failure() {
        let client = client_of_one("https://site.test/llms.txt", 404, "gone");
        let r = check_llms_txt_with(&client, "https://site.test", None);
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["exists"], json!(false));
        assert_eq!(r["score"], json!(0));
        assert!(r["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str().unwrap_or("").contains("llms.txt")));
    }

    #[test]
    fn gated_statuses_are_unmeasured_errors_not_missing_files() {
        for status in [401u16, 403, 429, 500] {
            let url = "https://site.test/llms.txt".to_string();
            let client = client_of_one(&url, status, "no");
            let r = check_llms_txt_with(&client, "https://site.test", None);
            assert_eq!(r["ok"], json!(false), "status {status}");
            assert_eq!(r["status_code"], json!(status), "status {status}");
            assert!(r.get("exists").is_none(), "status {status}");
        }
    }

    #[test]
    fn h1_check_is_named_as_mere_presence_without_a_brand() {
        let r = score_llms_txt("# x\n", None);
        let h1_check = &r["checks"][0];
        assert!(!h1_check["name"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("project name"));
        assert_eq!(h1_check["passed"], json!(true));
    }

    #[test]
    fn h1_check_requires_the_brand_when_one_is_supplied() {
        let r = score_llms_txt("# Widgets and more\n\ncontent", Some("Acme"));
        let h1_check = &r["checks"][0];
        assert!(h1_check["name"].as_str().unwrap().contains("Acme"));
        assert_eq!(h1_check["passed"], json!(false));
    }

    #[test]
    fn h1_check_passes_when_the_brand_is_in_the_heading() {
        let r = score_llms_txt("# Acme things\n\ncontent", Some("Acme"));
        assert_eq!(r["checks"][0]["passed"], json!(true));
    }

    #[test]
    fn network_failure_still_reports_not_measured() {
        let client = FakeClient::default();
        let r = check_llms_txt_with(&client, "https://down.test", None);
        assert_eq!(r["ok"], json!(false));
        assert!(r.get("error").is_some());
        assert!(r.get("exists").is_none());
    }

    #[test]
    fn keyword_checks_match_words_not_substrings() {
        // 'api' in 'rapid', 'case' in 'Showcase' and 'tool' in 'Toolbar' are
        // substring hits only; whole-word matching must not fire.
        let content = "# Acme\n\n## A\n## B\n## C\n[Toolbar](https://example.com/toolbar) [Showcase](https://example.com/showcase) [rapid](https://example.com/rapid)\n";
        let r = score_llms_txt(content, Some("Acme"));
        assert_eq!(r["checks"][4]["passed"], json!(false)); // category
        assert_eq!(r["checks"][6]["passed"], json!(false)); // proof
        assert_eq!(r["checks"][7]["passed"], json!(false)); // docs
    }

    #[test]
    fn invalid_url_is_reported_with_repr() {
        let client = client_of_one("unused", 200, "");
        let r = check_llms_txt_with(&client, "ftp://x.test", None);
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("Not a valid HTTP(S) URL: 'ftp://x.test'"));
    }
}
