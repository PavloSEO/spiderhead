//! Consolidation of one site's address variants into a single canonical URL.
//!
//! Port of the pure part of `spiderhead.recon.mirrors`: redirect-chain collection through a
//! [`HopClient`], the analysis of the collected chains, and the findings text. The variant list
//! comes from `spiderhead_core::url_variants::build_variants`. Network access (the HTTP client
//! and the DNS-over-HTTPS lookup of `www`) is supplied by the caller, so the analysis runs
//! without a network. The live client belongs to the recon-net slice.

use serde::Serialize;
use spiderhead_core::net::normalize_url;
use spiderhead_core::url_variants::{build_variants, UrlVariant};
use spiderhead_parse::py_url::{urljoin, urlsplit, urlunsplit, UrlError};

/// Redirect hops followed before a chain is reported as a suspected loop.
pub const MAX_HOPS: usize = 6;

/// Status codes whose `Location` header is followed by the matrix walk.
const REDIRECT_STATUSES: &[u16] = &[301, 302, 303, 307, 308];

/// Longest network error text kept in a result, in code points.
const ERROR_TEXT_LIMIT: usize = 200;

/// One response of a single hop, fetched without following redirects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HopResponse {
    /// HTTP status code.
    pub status: u16,
    /// The `Location` header, when present.
    pub location: Option<String>,
}

/// Fetches one address without following redirects.
///
/// `Err` carries the text of a network failure, which becomes result data, not a raised error.
pub trait HopClient {
    /// Request `url` and return its status and `Location` header.
    fn get(&self, url: &str) -> Result<HopResponse, String>;
}

/// One followed redirect: the address that answered, its status, and where it pointed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hop {
    /// Address that returned the redirect.
    pub url: String,
    /// Redirect status code.
    pub status: u16,
    /// Absolute target of the redirect.
    pub location: String,
}

/// The collected chain of one variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Chain {
    /// The variant as written by its rule (origin, index, path or case form).
    pub variant: String,
    /// The rule that produced the variant: `origin`, `index`, `path` or `case`.
    pub group: String,
    /// The address that was requested first.
    pub url: String,
    /// Whether any response was received.
    pub reachable: bool,
    /// The network error text, for an unreachable variant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Redirects followed, in order.
    pub hops: Vec<Hop>,
    /// Status of the last response, when the walk ended on one.
    pub status: Option<u16>,
    /// Last address reached.
    pub final_url: Option<String>,
    /// Number of redirects followed, for a reachable variant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirects: Option<usize>,
    /// Set when the walk stopped at [`MAX_HOPS`] redirects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_suspect: Option<bool>,
}

/// DNS facts for the `www` host, from DNS over HTTPS.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct WwwDns {
    /// Whether an A or CNAME record exists.
    pub resolvable: bool,
    /// A records.
    pub a: Vec<String>,
    /// CNAME records.
    pub cname: Vec<String>,
    /// Where the answer came from (`doh`).
    pub source: String,
}

/// A variant that could not be reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unreachable {
    /// The variant.
    pub variant: String,
    /// The network error text.
    pub error: Option<String>,
}

/// A chain whose redirect count exceeds one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LongChain {
    /// The variant.
    pub variant: String,
    /// Redirect count.
    pub hops: usize,
    /// Addresses visited, ending with the final address.
    pub chain: Vec<Option<String>>,
}

/// The consolidation report for one site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MirrorReport {
    /// Always `true` for a report that was produced.
    pub ok: bool,
    /// The normalized input URL.
    pub url: String,
    /// The single origin all variants reach, when they consolidate.
    pub canonical_origin: Option<String>,
    /// Whether every origin mirror reaches the same final address.
    pub consolidated: bool,
    /// DNS facts for `www`.
    pub www_dns: WwwDns,
    /// Every collected chain.
    pub variants: Vec<Chain>,
    /// Variants that answer 200 independently of the canonical address.
    pub duplicates_200: Vec<String>,
    /// Redirect hops that step from HTTPS down to HTTP.
    pub downgrade_redirects: Vec<String>,
    /// Chains with more than one redirect.
    pub long_chains: Vec<LongChain>,
    /// Variants that could not be reached.
    pub unreachable: Vec<Unreachable>,
    /// Human-readable findings, or the single all-clear line.
    pub findings: Vec<String>,
}

/// reference `_norm_final`: lower-case the netloc and strip trailing slashes from the path.
///
/// `None` and the empty string both give `None`. A root path becomes `/`.
pub fn norm_final(url: Option<&str>) -> Result<Option<String>, UrlError> {
    let Some(url) = url.filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let parts = urlsplit(url)?;
    let path = parts.path.trim_end_matches('/');
    let path = if path.is_empty() { "/" } else { path };
    Ok(Some(urlunsplit(
        &parts.scheme,
        &parts.netloc.to_lowercase(),
        path,
        "",
        "",
    )))
}

/// Keep a network error text to [`ERROR_TEXT_LIMIT`] code points, as the oracle truncates it.
fn truncate_error(text: &str) -> String {
    text.chars().take(ERROR_TEXT_LIMIT).collect()
}

/// Walk one variant's redirects one hop at a time, without following them automatically.
///
/// Stops at the first non-redirect response, at a redirect without a `Location`, or after
/// [`MAX_HOPS`] hops (then `loop_suspect` is set). Relative `Location` values are resolved
/// against the address that returned them. The caller sets `variant` and `group`.
pub fn fetch_chain(
    client: &dyn HopClient,
    variant: &str,
    group: &str,
    url: &str,
) -> Result<Chain, UrlError> {
    let mut hops: Vec<Hop> = Vec::new();
    let mut current = url.to_owned();
    for _ in 0..MAX_HOPS {
        let response = match client.get(&current) {
            Ok(response) => response,
            Err(error) => {
                return Ok(Chain {
                    variant: variant.to_owned(),
                    group: group.to_owned(),
                    url: url.to_owned(),
                    reachable: false,
                    error: Some(truncate_error(&error)),
                    hops,
                    status: None,
                    final_url: None,
                    redirects: None,
                    loop_suspect: None,
                });
            }
        };
        match response
            .location
            .filter(|location| REDIRECT_STATUSES.contains(&response.status) && !location.is_empty())
        {
            Some(location) => {
                let next = urljoin(&current, &location)?;
                hops.push(Hop {
                    url: current.clone(),
                    status: response.status,
                    location: next.clone(),
                });
                current = next;
            }
            None => {
                let redirects = Some(hops.len());
                return Ok(Chain {
                    variant: variant.to_owned(),
                    group: group.to_owned(),
                    url: url.to_owned(),
                    reachable: true,
                    error: None,
                    hops,
                    status: Some(response.status),
                    final_url: Some(current),
                    redirects,
                    loop_suspect: None,
                });
            }
        }
    }
    let redirects = Some(hops.len());
    Ok(Chain {
        variant: variant.to_owned(),
        group: group.to_owned(),
        url: url.to_owned(),
        reachable: true,
        error: None,
        hops,
        status: None,
        final_url: Some(current),
        redirects,
        loop_suspect: Some(true),
    })
}

/// Insert or overwrite a key in an insertion-ordered list, as a reference dict does.
fn upsert<T>(entries: &mut Vec<(String, T)>, key: &str, value: T) {
    match entries.iter_mut().find(|(existing, _)| existing == key) {
        Some(entry) => entry.1 = value,
        None => entries.push((key.to_owned(), value)),
    }
}

/// reference `f"{v}"` for an optional text: `None` renders as `None`.
fn text_or_none(value: Option<&str>) -> &str {
    value.unwrap_or("None")
}

/// Analyze collected chains without any network access.
///
/// Origin mirrors that converge on one final address define the canonical destination; without
/// convergence the bare-HTTPS address is used. A 200 at a non-canonical origin, at an index path
/// or at a case variant is a live duplicate. Two slash forms that both answer 200 are a
/// trailing-slash duplicate. Also reported: HTTPS-to-HTTP downgrades, chains longer than one
/// redirect, loops, an unresolvable `www`, and unreachable variants.
pub fn analyze(url: &str, results: &[Chain], www_dns: &WwwDns) -> Result<MirrorReport, UrlError> {
    let normalized = normalize_url(url);
    let parts = urlsplit(&normalized)?;
    let host = parts.netloc.to_lowercase();
    let bare = host.strip_prefix("www.").unwrap_or(&host).to_owned();
    let canon_https = norm_final(Some(&format!("https://{bare}/")))?.unwrap_or_default();

    let mut findings: Vec<String> = Vec::new();
    let mut origin_finals: Vec<(String, Option<String>)> = Vec::new();
    for chain in results
        .iter()
        .filter(|chain| chain.group == "origin" && chain.reachable)
    {
        let final_key = norm_final(chain.final_url.as_deref())?;
        upsert(&mut origin_finals, &chain.variant, final_key);
    }
    let finals: std::collections::BTreeSet<String> = origin_finals
        .iter()
        .filter_map(|(_, value)| value.clone())
        .collect();
    let consolidated = finals.len() == 1;
    let canonical_destination = if consolidated {
        finals
            .iter()
            .next()
            .cloned()
            .unwrap_or_else(|| canon_https.clone())
    } else {
        canon_https.clone()
    };

    let mut duplicates_200: Vec<String> = Vec::new();
    let mut downgrade: Vec<String> = Vec::new();
    let mut long_chains: Vec<LongChain> = Vec::new();
    let mut dead: Vec<Unreachable> = Vec::new();
    let mut path_200: Vec<(String, bool)> = Vec::new();

    for chain in results {
        if !chain.reachable {
            dead.push(Unreachable {
                variant: chain.variant.clone(),
                error: chain.error.clone(),
            });
            continue;
        }
        let answered_200_here = chain.status == Some(200) && chain.hops.is_empty();
        if chain.group == "origin"
            && answered_200_here
            && norm_final(Some(&chain.url))?.as_deref() != Some(canonical_destination.as_str())
        {
            // This mirror remains independently accessible.
            duplicates_200.push(chain.variant.clone());
        }
        if (chain.group == "index" || chain.group == "case") && answered_200_here {
            duplicates_200.push(chain.variant.clone());
        }
        if chain.group == "path" {
            upsert(&mut path_200, &chain.variant, answered_200_here);
        }
        for hop in &chain.hops {
            if hop.url.starts_with("https://") && hop.location.starts_with("http://") {
                downgrade.push(format!(
                    "{}: {} \u{2192} {}",
                    chain.variant, hop.url, hop.location
                ));
            }
        }
        let redirects = chain.redirects.unwrap_or(0);
        if redirects > 1 {
            let mut visited: Vec<Option<String>> =
                chain.hops.iter().map(|hop| Some(hop.url.clone())).collect();
            visited.push(chain.final_url.clone());
            long_chains.push(LongChain {
                variant: chain.variant.clone(),
                hops: redirects,
                chain: visited,
            });
        }
        if chain.loop_suspect == Some(true) {
            findings.push(format!(
                "{}: more than {MAX_HOPS} redirects; possible loop",
                chain.variant
            ));
        }
    }

    let answered: Vec<&String> = path_200
        .iter()
        .filter(|(_, ok)| *ok)
        .map(|(variant, _)| variant)
        .collect();
    if answered.len() > 1 {
        let mut keys: Vec<&str> = path_200
            .iter()
            .map(|(variant, _)| variant.as_str())
            .collect();
        keys.sort_unstable();
        let pair = keys.join(" and ");
        findings.push(format!(
            "trailing-slash duplicate: {pair} both return 200 without consolidation"
        ));
        duplicates_200.push(pair);
    }

    if !consolidated && finals.len() > 1 {
        let listing: Vec<String> = origin_finals
            .iter()
            .map(|(variant, value)| {
                format!("{variant} \u{2192} {}", text_or_none(value.as_deref()))
            })
            .collect();
        findings.push(format!(
            "origin mirrors do not consolidate to one address: {}",
            listing.join("; ")
        ));
    }
    for duplicate in &duplicates_200 {
        findings.push(format!(
            "{duplicate}: returns 200 instead of redirecting to the canonical URL; live duplicate"
        ));
    }
    for step in &downgrade {
        findings.push(format!("redirect downgrades HTTPS to HTTP: {step}"));
    }
    for chain in &long_chains {
        findings.push(format!(
            "{}: redirect chain contains {} hops",
            chain.variant, chain.hops
        ));
    }
    if !www_dns.resolvable {
        findings.push(
            "www hostname does not resolve (no A or CNAME record); the mirror is unreachable"
                .to_owned(),
        );
    }
    for item in &dead {
        findings.push(format!(
            "{}: unavailable ({})",
            item.variant,
            text_or_none(item.error.as_deref())
        ));
    }

    let canonical_origin = if consolidated {
        finals.iter().next().cloned()
    } else {
        None
    };
    if findings.is_empty() {
        findings.push("all checked variants consolidate correctly".to_owned());
    }
    Ok(MirrorReport {
        ok: true,
        url: normalized,
        canonical_origin,
        consolidated,
        www_dns: www_dns.clone(),
        variants: results.to_vec(),
        duplicates_200,
        downgrade_redirects: downgrade,
        long_chains,
        unreachable: dead,
        findings,
    })
}

/// Walk the full variant matrix of `url` through `client`, then analyze it.
///
/// Returns `Ok(None)` when the input does not normalize to a URL, as the oracle reports
/// `not a valid URL`. The DNS lookup for `www` is passed in, so the caller decides how it is
/// obtained. The live HTTP client and DoH resolver are outside this slice.
pub fn check_mirrors_with(
    url: &str,
    www_dns: &WwwDns,
    client: &dyn HopClient,
) -> Result<Option<MirrorReport>, UrlError> {
    let target = normalize_url(url);
    if target.is_empty() {
        return Ok(None);
    }
    let variants: Vec<UrlVariant> = build_variants(&target);
    let mut results = Vec::with_capacity(variants.len());
    for variant in &variants {
        results.push(fetch_chain(
            client,
            &variant.variant,
            &variant.group,
            &variant.url,
        )?);
    }
    analyze(&target, &results, www_dns).map(Some)
}
