//! WHOIS text parsing and the registry table.
//!
//! The network runner is not implemented yet: it spawns a system binary, which this crate does not
//! wire up. Only the pure parsing lives here.

use fancy_regex::Regex;
use std::sync::OnceLock;

/// Registries the default resolver does not reach; without these the zone record is returned
/// instead of the domain's own registration.
const WHOIS_SERVERS_BY_TLD: &[(&str, &str)] = &[
    ("ru", "whois.tcinet.ru"),
    ("su", "whois.tcinet.ru"),
    ("xn--p1ai", "whois.tcinet.ru"),
];

/// Keys a registry uses to refer to the authoritative server.
pub const WHOIS_REFERRAL_KEYS: &[&str] = &["whois", "refer", "registrar whois server"];

const HOSTNAME_PATTERN: &str =
    r"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$";

/// The mapped WHOIS server for a top-level domain, if the table has one.
pub fn whois_server_for_tld(tld: &str) -> Option<&'static str> {
    WHOIS_SERVERS_BY_TLD
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(tld))
        .map(|(_, server)| *server)
}

/// Whether a referral value is a syntactically valid hostname (checked before it is ever used).
pub fn is_valid_whois_server(server: &str) -> bool {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(HOSTNAME_PATTERN).ok())
        .as_ref()
        .is_some_and(|re| re.is_match(&server.to_lowercase()).unwrap_or(false))
}

/// First value of the first matching key, ignoring comment lines.
pub fn whois_field(text: &str, keys: &[&str]) -> Option<String> {
    for line in text.lines() {
        let stripped = line.trim_start();
        if !stripped.contains(':') || stripped.starts_with(['%', '#']) {
            continue;
        }
        let (key, value) = stripped.split_once(':').unwrap_or((stripped, ""));
        let value = value.trim();
        if keys.contains(&key.trim().to_lowercase().as_str()) && !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Behaviour of whois_field: comment lines are skipped and the key match is case-insensitive.
    #[test]
    fn whois_field_skips_comments_and_matches_keys_case_insensitively() {
        let text = "% comment: whois.bad\n# refer: nope\nWhois: whois.good.test\nRefer: other.test";
        assert_eq!(
            whois_field(text, WHOIS_REFERRAL_KEYS).as_deref(),
            Some("whois.good.test")
        );
        assert_eq!(whois_field("nothing here", WHOIS_REFERRAL_KEYS), None);
    }

    // Behaviour of the registry table: ru, su and the punycode .rf map to the same server.
    #[test]
    fn registry_table_maps_cctld_to_tcinet() {
        assert_eq!(whois_server_for_tld("RU"), Some("whois.tcinet.ru"));
        assert_eq!(whois_server_for_tld("xn--p1ai"), Some("whois.tcinet.ru"));
        assert_eq!(whois_server_for_tld("com"), None);
    }

    // Behaviour of the hostname check guarding every referral argument.
    #[test]
    fn referral_server_must_be_a_hostname() {
        assert!(is_valid_whois_server("whois.tcinet.ru"));
        assert!(!is_valid_whois_server("-bad.example"));
        assert!(!is_valid_whois_server("with space.example"));
        assert!(!is_valid_whois_server("localhost"));
    }
}
