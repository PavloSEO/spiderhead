//! Bot-protection block classification.
//!
//! Detection and honest reporting only: a 403 or 429 that carries WAF markers is recorded as
//! a block, never bypassed. Plain origin errors and every 2xx response are not blocks.

/// Result state recorded when the edge refused the client instead of the site answering.
pub const BLOCKED_BY_WAF: &str = "blocked_by_waf";

/// `cf-mitigated` values that mean Cloudflare itself answered with a block.
const CF_MITIGATED_STATES: &[&str] = &["challenge", "block", "managed"];

/// Literals a challenge body carries and a normal error page does not.
const CHALLENGE_BODY_MARKERS: &[&str] = &[
    "cdn-cgi/challenge-platform",
    "cf_chl_",
    "cf-browser-verification",
    "attention required! | cloudflare",
    "just a moment...",
    "checking your browser before accessing",
    "enable javascript and cookies to continue",
];

/// Only this many leading bytes of a body are inspected.
const CHALLENGE_SNIFF_BYTES: usize = 8192;

/// Classify a response as a bot-protection block, or `None`.
///
/// `headers` are `(name, value)` pairs; names are matched case-insensitively. `body` is the
/// response text, or empty when the body was never read.
pub fn waf_block_reason(status: u16, headers: &[(&str, &str)], body: &str) -> Option<String> {
    if status != 403 && status != 429 {
        return None;
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim().to_lowercase())
            .unwrap_or_default()
    };
    let mitigated = header("cf-mitigated");
    if CF_MITIGATED_STATES.contains(&mitigated.as_str()) {
        return Some(format!("cf-mitigated: {mitigated}"));
    }
    if header("server") == "cloudflare" {
        return Some("cloudflare edge".into());
    }
    let mut end = body.len().min(CHALLENGE_SNIFF_BYTES);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    let snippet = body[..end].to_lowercase();
    CHALLENGE_BODY_MARKERS
        .iter()
        .find(|marker| snippet.contains(**marker))
        .map(|marker| format!("challenge body: {marker}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Behaviour of waf_block_reason: a Cloudflare challenge header is a block.
    #[test]
    fn cf_mitigated_header_is_a_block() {
        let reason = waf_block_reason(403, &[("cf-mitigated", "Challenge")], "");
        assert_eq!(reason.as_deref(), Some("cf-mitigated: challenge"));
    }

    // Behaviour of waf_block_reason: origin 403 with no markers is not a WAF block.
    #[test]
    fn plain_origin_403_is_not_a_block() {
        assert_eq!(
            waf_block_reason(403, &[("server", "nginx")], "forbidden"),
            None
        );
    }

    // Behaviour of waf_block_reason: 2xx responses are never blocks.
    #[test]
    fn success_is_never_a_block() {
        assert_eq!(
            waf_block_reason(200, &[("cf-mitigated", "block")], ""),
            None
        );
    }

    // Behaviour of waf_block_reason: challenge body marker on 429.
    #[test]
    fn challenge_body_marker_is_a_block() {
        let body = "<title>Just a moment...</title>";
        assert_eq!(
            waf_block_reason(429, &[], body).as_deref(),
            Some("challenge body: just a moment...")
        );
    }

    // Behaviour of waf_block_reason: Cloudflare edge server header on 403.
    #[test]
    fn cloudflare_edge_server_header_is_a_block() {
        assert_eq!(
            waf_block_reason(403, &[("Server", "cloudflare")], "").as_deref(),
            Some("cloudflare edge")
        );
    }
}
