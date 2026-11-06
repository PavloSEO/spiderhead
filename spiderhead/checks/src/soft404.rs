//! Active soft-404 probes for one origin, and the stored-body classifier.
//!
//! A crawl only requests known URLs, so it cannot see that a server answers
//! **200 OK** for a nonexistent address. The probe builds two deterministic
//! nonexistent URLs from the origin's SHA-256 hash at ordinary root paths,
//! requests both with redirects followed (the final status is the evidence),
//! and classifies with strict AND logic: two 2xx/3xx answers confirm a soft
//! 404, two genuine 404/410 answers pass, and any disagreement is ``unknown``.
//! A probe whose redirect was refused by the caller's own network guard is
//! reported as ``refused``: the site tried to send the probe somewhere the
//! guard declines to go, which is a different fact from inconclusive answers.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use spiderhead_parse::py_url::{urlsplit, urlunsplit};

/// Probes requested per check; classification needs agreement of all of them.
pub const PROBE_COUNT: usize = 2;

/// Both probes must agree on a thin-body verdict only when the page is below
/// this whitespace-separated word count.
const THIN_TEXT_WORDS: usize = 50;

/// Bits out of 64 that may differ between a page and the 404 template before
/// the two are considered the same document.
const TEMPLATE_HAMMING_MAX: u32 = 3;

/// Error phrases a templated "nothing found" page carries.
const ERROR_PHRASES: [&str; 6] = [
    "страница не найдена",
    "ничего не найдено",
    "товар не найден",
    "page not found",
    "nothing found",
    "404 not found",
];

/// One probe answer, as [`classify_soft404`] reads it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Probe {
    /// The requested nonexistent URL.
    pub url: String,
    /// Final status after redirects, when a response was received.
    pub status: Option<u16>,
    /// The URL the response ended on.
    pub final_url: Option<String>,
    /// Whether the response URL differs from the requested one.
    pub redirected: bool,
    /// The caller's own guard refused the probe's redirect target.
    pub blocked_by_guard: bool,
    /// The page marked itself as access-blocked, so its status is not evidence.
    pub access_blocked: bool,
    /// Transport error text, when no response was received.
    pub error: Option<String>,
}

/// The verdict of [`classify_soft404`].
pub const VERDICTS: [&str; 4] = ["pass", "warning", "refused", "unknown"];

/// Build two deterministic nonexistent URLs from the origin of `start_url`.
///
/// Hashing the origin keeps the pair stable across runs, which report caching
/// relies on. The paths are ordinary root routes so static middleware that
/// owns `/.well-known/` cannot bypass an application's fallback route.
pub fn probe_urls(start_url: &str) -> Vec<String> {
    let with_scheme = if start_url.contains("://") {
        start_url.to_owned()
    } else {
        format!("https://{start_url}")
    };
    let origin = urlsplit(&with_scheme)
        .map(|parts| {
            urlunsplit(
                if parts.scheme.is_empty() {
                    "https"
                } else {
                    &parts.scheme
                },
                &parts.netloc,
                "",
                "",
                "",
            )
        })
        .unwrap_or_default();
    let seed = hex_prefix(&origin, 12);
    (1..=PROBE_COUNT as u32)
        .map(|i| format!("{origin}/seo-audit-not-found-{seed}-{i}"))
        .collect()
}

/// First `width` hex digits of the SHA-256 digest of `text`.
fn hex_prefix(text: &str, width: usize) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    hex[..width].to_owned()
}

/// Classify probe answers: `pass`, `warning` (soft 404), `refused`, or `unknown`.
///
/// Strict AND logic requires both probes to agree; disagreement does not
/// support a verdict. A guard refusal is checked first and independently:
/// a probe the guard declined never got a final status at all.
pub fn classify_soft404(probes: &[Probe]) -> &'static str {
    if probes.iter().any(|p| p.blocked_by_guard) {
        return "refused";
    }
    let conclusive: Vec<&Probe> = probes
        .iter()
        .filter(|p| p.status.is_some() && !p.access_blocked)
        .collect();
    let successful = conclusive
        .iter()
        .filter(|p| matches!(p.status, Some(status) if (200..400).contains(&status)))
        .count();
    let correct_missing = conclusive
        .iter()
        .filter(|p| matches!(p.status, Some(404) | Some(410)))
        .count();
    if conclusive.len() != PROBE_COUNT {
        return "unknown";
    }
    if successful == PROBE_COUNT {
        return "warning";
    }
    if correct_missing == PROBE_COUNT {
        return "pass";
    }
    "unknown"
}

/// The transport seam [`check_soft404_with`] probes through.
pub trait ProbeClient {
    /// GET `url` with redirects followed: the final status is the evidence.
    fn get(&self, url: &str) -> Result<ProbeAnswer, ProbeRefusal>;
}

/// One completed probe request.
pub struct ProbeAnswer {
    /// Final status code after redirects.
    pub status: u16,
    /// Final URL after redirects.
    pub final_url: String,
}

/// One failed probe. A refused redirect carries the real response's status and
/// the refused target, so classification can tell it apart from a dead host.
pub struct ProbeRefusal {
    /// The failure message, as Python renders `str(exc)`.
    pub message: String,
    /// The caller's own guard refused the next hop (rather than transport death).
    pub blocked_by_guard: bool,
    /// Status of the response whose redirect was refused.
    pub status_code: u16,
    /// The refused absolute redirect target.
    pub location: String,
}

/// Probe `url` through `client` and classify the answers.
pub fn check_soft404_with(url: &str, client: &dyn ProbeClient) -> Value {
    let targets = probe_urls(url);
    let mut probes = Vec::new();
    for target in &targets {
        match client.get(target) {
            Ok(answer) => probes.push(json!({
                "url": target,
                "status": answer.status,
                "final_url": answer.final_url,
                "redirected": answer.final_url != *target,
            })),
            Err(failure) if failure.blocked_by_guard => probes.push(json!({
                "url": target,
                "status": failure.status_code,
                "final_url": failure.location,
                "redirected": true,
                "blocked_by_guard": true,
                "error": failure.message,
            })),
            Err(failure) => probes.push(json!({"url": target, "error": failure.message})),
        }
    }
    let classified: Vec<Probe> = probes
        .iter()
        .map(|p| Probe {
            url: p["url"].as_str().unwrap_or_default().to_owned(),
            status: p["status"].as_u64().map(|s| s as u16),
            final_url: p["final_url"].as_str().map(str::to_owned),
            redirected: p["redirected"].as_bool().unwrap_or(false),
            blocked_by_guard: p["blocked_by_guard"].as_bool().unwrap_or(false),
            access_blocked: false,
            error: p["error"].as_str().map(str::to_owned),
        })
        .collect();
    let verdict = classify_soft404(&classified);
    let findings = match verdict {
        "pass" => "the site returns genuine 404/410 responses for nonexistent URLs",
        "warning" => {
            "soft 404: nonexistent URLs return 200/3xx responses and may create \
             indexable low-value pages"
        }
        "refused" => {
            "a probe's redirect target was refused by our own network guard \
             (it pointed at a private or non-public address); no soft-404 verdict is possible"
        }
        _ => "the probes disagreed or did not complete; no verdict is available",
    };
    json!({
        "ok": true,
        "url": url,
        "verdict": verdict,
        "probes": probes,
        "findings": [findings],
    })
}

/// Classify one stored 200 body as a soft 404 from the saved text and simhashes.
///
/// Verdicts reuse [`classify_soft404`]'s vocabulary. A non-200 status is never
/// a soft 404 here; the page is `warning` when its fingerprint matches the 404
/// template, when it carries no words at all, or when it is thin and names a
/// known error phrase.
pub fn classify_stored_soft404(
    status: u16,
    text: &str,
    template_simhash: Option<u64>,
    page_simhash: Option<u64>,
) -> Value {
    use spiderhead_parse::duplicate::hamming;
    if status != 200 {
        return json!({"verdict": "pass", "reason": "none"});
    }
    if let (Some(template), Some(page)) = (template_simhash, page_simhash) {
        if hamming(page, template) <= TEMPLATE_HAMMING_MAX {
            return json!({"verdict": "warning", "reason": "template_match"});
        }
    }
    let words = text.split_whitespace().count();
    if words == 0 {
        return json!({"verdict": "warning", "reason": "thin_text"});
    }
    let lowered = text.to_lowercase();
    if words < THIN_TEXT_WORDS && ERROR_PHRASES.iter().any(|phrase| lowered.contains(phrase)) {
        return json!({"verdict": "warning", "reason": "error_phrase"});
    }
    json!({"verdict": "pass", "reason": "none"})
}

#[cfg(test)]
mod tests {
    use super::*;

    // from tests/test_soft404.py::test_probe_urls_deterministic_per_origin
    #[test]
    fn probe_urls_deterministic_per_origin() {
        let a = probe_urls("https://shop.example.com/page");
        let b = probe_urls("https://shop.example.com/other");
        assert_eq!(a, b, "probes are deterministic by origin, not by path");
        assert_eq!(a.len(), PROBE_COUNT);
        assert!(a.iter().all(|url| {
            let parts = urlsplit(url).expect("split");
            parts.path.starts_with("/seo-audit-not-found-") && parts.path.matches('/').count() == 1
        }));
    }

    // from tests/test_soft404.py::test_probe_urls_differ_per_origin
    #[test]
    fn probe_urls_differ_per_origin() {
        assert_ne!(
            probe_urls("https://shop.example.com"),
            probe_urls("https://other.example.org")
        );
    }

    /// Stub answering 404 only for `/.well-known/` routes; matches the oracle
    /// test's monkeypatched client.
    struct WellKnownStub;

    impl ProbeClient for WellKnownStub {
        fn get(&self, url: &str) -> Result<ProbeAnswer, ProbeRefusal> {
            let status = if url.contains("/.well-known/") {
                404
            } else {
                200
            };
            Ok(ProbeAnswer {
                status,
                final_url: url.to_owned(),
            })
        }
    }

    struct StatusStub(u16);

    impl ProbeClient for StatusStub {
        fn get(&self, url: &str) -> Result<ProbeAnswer, ProbeRefusal> {
            Ok(ProbeAnswer {
                status: self.0,
                final_url: url.to_owned(),
            })
        }
    }

    struct GuardStub {
        calls: std::cell::RefCell<Vec<String>>,
    }

    impl ProbeClient for GuardStub {
        fn get(&self, url: &str) -> Result<ProbeAnswer, ProbeRefusal> {
            self.calls.borrow_mut().push(url.to_owned());
            Err(ProbeRefusal {
                message: "private or non-public network target blocked".to_owned(),
                blocked_by_guard: true,
                status_code: 301,
                location: "http://169.254.169.254/".to_owned(),
            })
        }
    }

    fn probe(url: &str, status: Option<u16>) -> Probe {
        Probe {
            url: url.to_owned(),
            status,
            ..Probe::default()
        }
    }

    // from tests/test_soft404.py::test_check_soft404_uses_ordinary_root_paths_not_a_well_known_middleware_route
    #[test]
    fn check_uses_ordinary_root_paths_not_a_well_known_route() {
        let result = check_soft404_with("https://shop.example.com", &WellKnownStub);
        assert_eq!(result["verdict"], "warning");
        for probe in result["probes"].as_array().expect("probes") {
            assert!(!probe["url"]
                .as_str()
                .expect("url")
                .contains("/.well-known/"));
            assert_eq!(probe["status"], 200);
        }
    }

    // from tests/test_soft404.py::test_check_soft404_passes_when_ordinary_nonexistent_paths_return_404
    #[test]
    fn check_passes_when_ordinary_nonexistent_paths_return_404() {
        let result = check_soft404_with("https://shop.example.com", &StatusStub(404));
        assert_eq!(result["verdict"], "pass");
        for probe in result["probes"].as_array().expect("probes") {
            assert_eq!(probe["status"], 404);
        }
    }

    // from tests/test_soft404.py::test_classify_pass_when_both_404
    #[test]
    fn classify_pass_when_both_404() {
        assert_eq!(
            classify_soft404(&[probe("", Some(404)), probe("", Some(410))]),
            "pass"
        );
    }

    // from tests/test_soft404.py::test_classify_warning_when_both_200
    #[test]
    fn classify_warning_when_both_200() {
        assert_eq!(
            classify_soft404(&[probe("", Some(200)), probe("", Some(301))]),
            "warning"
        );
    }

    // from tests/test_soft404.py::test_classify_unknown_on_mixed
    #[test]
    fn classify_unknown_on_mixed() {
        assert_eq!(
            classify_soft404(&[probe("", Some(200)), probe("", Some(404))]),
            "unknown"
        );
    }

    // from tests/test_soft404.py::test_classify_unknown_when_probe_errored
    #[test]
    fn classify_unknown_when_probe_errored() {
        let errored = Probe {
            url: String::new(),
            status: None,
            error: Some("timeout".to_owned()),
            ..Probe::default()
        };
        assert_eq!(
            classify_soft404(&[probe("", Some(404)), errored]),
            "unknown"
        );
    }

    // from tests/test_soft404.py::test_classify_respects_access_blocked
    #[test]
    fn classify_respects_access_blocked() {
        let blocked = Probe {
            url: String::new(),
            status: Some(200),
            access_blocked: true,
            ..Probe::default()
        };
        // With only one conclusive probe, the result must remain unknown.
        assert_eq!(
            classify_soft404(&[probe("", Some(404)), blocked]),
            "unknown"
        );
    }

    // from tests/test_soft404.py::test_classify_refused_when_a_probe_was_blocked_by_our_own_guard
    #[test]
    fn classify_refused_when_a_probe_was_blocked_by_our_own_guard() {
        let guarded = Probe {
            url: String::new(),
            status: Some(301),
            final_url: Some("http://169.254.169.254/".to_owned()),
            blocked_by_guard: true,
            ..Probe::default()
        };
        assert_eq!(
            classify_soft404(&[probe("", Some(404)), guarded]),
            "refused"
        );
    }

    // from tests/test_soft404.py::test_classify_refused_takes_priority_over_a_conclusive_agreement
    #[test]
    fn classify_refused_takes_priority_over_a_conclusive_agreement() {
        let guarded = Probe {
            url: String::new(),
            status: Some(404),
            final_url: Some("http://127.0.0.1/".to_owned()),
            blocked_by_guard: true,
            ..Probe::default()
        };
        assert_eq!(
            classify_soft404(&[probe("", Some(404)), guarded]),
            "refused"
        );
    }

    // from tests/test_soft404.py::test_check_soft404_records_a_guard_refusal_without_requesting_the_target
    #[test]
    fn check_records_a_guard_refusal() {
        let stub = GuardStub {
            calls: std::cell::RefCell::new(Vec::new()),
        };
        let result = check_soft404_with("https://shop.example.com", &stub);
        assert_eq!(result["ok"], true);
        assert_eq!(result["verdict"], "refused");
        assert_eq!(stub.calls.borrow().len(), PROBE_COUNT);
        for probe in result["probes"].as_array().expect("probes") {
            assert_eq!(probe["blocked_by_guard"], true);
            assert_eq!(probe["status"], 301);
            assert_eq!(probe["final_url"], "http://169.254.169.254/");
        }
    }

    // from tests/test_soft404.py::test_stored_soft404_flags_empty_200_body
    #[test]
    fn stored_flags_empty_200_body() {
        assert_eq!(
            classify_stored_soft404(200, "", None, None),
            json!({"verdict": "warning", "reason": "thin_text"})
        );
    }

    // from tests/test_soft404.py::test_stored_soft404_flags_thin_text_with_error_phrase
    #[test]
    fn stored_flags_thin_text_with_error_phrase() {
        assert_eq!(
            classify_stored_soft404(
                200,
                "Страница не найдена. Вернуться на главную.",
                None,
                None
            ),
            json!({"verdict": "warning", "reason": "error_phrase"})
        );
    }

    // from tests/test_soft404.py::test_stored_soft404_does_not_flag_full_product_page
    #[test]
    fn stored_does_not_flag_full_product_page() {
        let full =
            "Centrifugal pump with full specifications, price and delivery terms; ".repeat(10);
        assert_eq!(
            classify_stored_soft404(200, &full, None, None),
            json!({"verdict": "pass", "reason": "none"})
        );
    }

    // from tests/test_soft404.py::test_stored_soft404_never_flags_non_200_status
    #[test]
    fn stored_never_flags_non_200_status() {
        assert_eq!(
            classify_stored_soft404(404, "page not found", None, None),
            json!({"verdict": "pass", "reason": "none"})
        );
    }

    // from tests/test_soft404.py::test_stored_soft404_matches_error_template_by_simhash
    #[test]
    fn stored_matches_error_template_by_simhash() {
        let full =
            "Centrifugal pump with full specifications, price and delivery terms. ".repeat(10);
        let template = spiderhead_parse::duplicate::simhash(&full, 3);
        assert_eq!(
            classify_stored_soft404(200, &full, Some(template), Some(template)),
            json!({"verdict": "warning", "reason": "template_match"})
        );
    }

    // from tests/test_soft404.py::test_stored_soft404_survives_missing_template_simhash
    #[test]
    fn stored_survives_missing_template_simhash() {
        let full =
            "Centrifugal pump with full specifications, price and delivery terms. ".repeat(10);
        assert_eq!(
            classify_stored_soft404(200, &full, None, Some(12345)),
            json!({"verdict": "pass", "reason": "none"})
        );
    }
}
