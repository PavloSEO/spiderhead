//! Shared network guardrails: private-address policy, URL validation, domains, proxies, WAF
//! block classification and WHOIS parsing.
//!
//! Every network call goes through [`crate::host::Host`], so DNS answers and environment are
//! injected and tests never touch the network. The DNS-over-HTTPS client, RDAP client and system
//! `whois` runner are not implemented yet: they need an HTTP and process layer.

mod address;
mod domain;
mod idna_codec;
mod nameprep_data;
mod proxy;
mod waf;
mod whois;

pub use address::{
    allowed_private_hosts, allows_private_range, is_public_address, pinned_target,
    private_networks_enabled, resolve_socket_addresses, validate_url, PinnedTarget,
};
pub use domain::{normalize_domain, normalize_url, registrable_domain, try_normalize_domain};
pub use idna_codec::{encode_idna, IdnaError};
pub use proxy::{resolve_proxy_route, ProxyRoute};
pub use waf::{waf_block_reason, BLOCKED_BY_WAF};
pub use whois::{is_valid_whois_server, whois_field, whois_server_for_tld, WHOIS_REFERRAL_KEYS};

/// Environment variable that opens every private range; the `SPIDERHEAD_` spelling is also read.
pub const PRIVATE_NETWORK_ENV: &str = "SEOHEAD_ALLOW_PRIVATE_NETWORKS";
/// Environment variable naming exact private hostnames (comma-separated).
pub const PRIVATE_HOST_ALLOWLIST_ENV: &str = "SEOHEAD_ALLOW_PRIVATE_HOSTS";
