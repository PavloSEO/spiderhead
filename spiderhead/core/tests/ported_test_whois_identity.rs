//! Ports of the pure WHOIS parsing tests in the reference suite. Each test names its
//! source. The identity check (`whois_record_is_about`), the `profile_domain` tests, the
//! system-whois runner tests and the Punycode profile test need the recon layer or a process
//! runner that is not ported yet, so they are listed in the slice report instead.

use spiderhead_core::net::{whois_field, whois_server_for_tld, WHOIS_REFERRAL_KEYS};

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
    assert_eq!(whois_field(text, WHOIS_REFERRAL_KEYS), None);
}

#[test]
fn ru_is_mapped_to_the_registry_server() {
    assert_eq!(whois_server_for_tld("ru"), Some("whois.tcinet.ru"));
    assert_eq!(whois_server_for_tld("xn--p1ai"), Some("whois.tcinet.ru"));
}
