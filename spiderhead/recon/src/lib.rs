//! Read-only site reconnaissance checks: AI-bot access, backlinks, CDN, domain,
//! hreflang, links, llms.txt, redirect chains, robots, network helpers, regional
//! structure, security headers and technology detection.

/// AI-crawler access audit.
pub mod ai_bots;
/// Backlink profile merge over explicit evidence.
pub mod backlink_profile;
/// Donor-list backlink verification.
pub mod backlinks;
/// CDN and cache-behavior inspection.
pub mod cdn;
pub(crate) mod dates;
/// Domain registration/DNS/hosting/TLS profile.
pub mod domain;
pub mod headers_check;
/// Hreflang alternates review.
pub mod hreflang_check;
/// On-page link checker.
pub mod links_check;
/// `llms.txt` scorer.
pub mod llms_txt;
/// Guarded HTTP client, DoH/RDAP/WHOIS and URL/domain helpers.
pub mod net_client;
/// Live redirect-chain checker.
pub mod redirects_check;
/// Regional structure audit.
pub mod regions;
/// robots.txt fetch and rule review.
pub mod robots_check;
pub(crate) mod robots_txt;
/// Security-header review.
pub mod security;
/// Technology detection.
pub mod tech;
/// External technology fingerprint database.
pub mod tech_db;
