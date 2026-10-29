//! Network lookups for domain profiling: DNS names, RDAP owner, WHOIS and injectable probes.

use super::*;

/// Team Cymru query name for this address's origin ASN.
pub(super) fn cymru_query_name(ip: &str) -> Option<String> {
    match ip.parse::<IpAddr>().ok()? {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            Some(format!(
                "{}.{}.{}.{}.origin.asn.cymru.com",
                octets[3], octets[2], octets[1], octets[0]
            ))
        }
        IpAddr::V6(v6) => {
            let nibbles: Vec<char> = v6
                .segments()
                .iter()
                .map(|s| format!("{s:04x}"))
                .collect::<String>()
                .chars()
                .collect();
            Some(format!(
                "{}.origin6.asn.cymru.com",
                nibbles
                    .iter()
                    .rev()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(".")
            ))
        }
    }
}

/// PTR query name for an IPv4 or IPv6 address.
pub(super) fn reverse_dns_name(ip: &str) -> Option<String> {
    match ip.parse::<IpAddr>().ok()? {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            Some(format!("{}.{}.{}.{}.in-addr.arpa", o[3], o[2], o[1], o[0]))
        }
        IpAddr::V6(v6) => {
            let nibbles: Vec<char> = v6
                .segments()
                .iter()
                .map(|s| format!("{s:04x}"))
                .collect::<String>()
                .chars()
                .collect();
            Some(format!(
                "{}.ip6.arpa",
                nibbles
                    .iter()
                    .rev()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(".")
            ))
        }
    }
}

pub(super) fn asn_via_cymru(
    doh: &dyn Fn(&str, &str) -> Vec<String>,
    ip: &str,
) -> Map<String, Json> {
    let mut out = Map::new();
    let Some(query) = cymru_query_name(ip) else {
        return out;
    };
    let answer = doh(&query, "TXT");
    if answer.is_empty() {
        return out;
    }
    let parts: Vec<String> = answer[0].split('|').map(|p| p.trim().to_string()).collect();
    if parts.len() < 3 {
        return out;
    }
    let asn = parts[0].split_whitespace().next().map(str::to_string);
    out.insert(
        "asn".to_string(),
        asn.as_ref()
            .map(|a| json!(format!("AS{a}")))
            .unwrap_or(Json::Null),
    );
    out.insert(
        "prefix".to_string(),
        if parts[1].is_empty() {
            Json::Null
        } else {
            json!(parts[1])
        },
    );
    out.insert(
        "country".to_string(),
        if parts[2].is_empty() {
            Json::Null
        } else {
            json!(parts[2])
        },
    );
    if let Some(asn) = asn {
        let name = doh(&format!("AS{asn}.asn.cymru.com"), "TXT");
        if let Some(first) = name.first() {
            if let Some(tail) = first.split('|').next_back().map(|p| p.trim().to_string()) {
                out.insert(
                    "as_name".to_string(),
                    if tail.is_empty() {
                        Json::Null
                    } else {
                        json!(tail)
                    },
                );
            }
        }
    }
    out
}

/// RDAP network owner, which may reveal the provider behind a CDN.
pub(super) fn ip_owner(rdap: &dyn Fn(&str) -> Json, ip: &str) -> Map<String, Json> {
    let mut out = Map::new();
    let res = rdap(&format!("ip/{ip}"));
    if res["supported"].as_bool() != Some(true) {
        return out;
    }
    let data = &res["data"];
    for key in ["name", "handle", "country", "type"] {
        out.insert(
            match key {
                "name" => "network",
                other => other,
            }
            .to_string(),
            data.get(key).cloned().unwrap_or(Json::Null),
        );
    }
    out
}

pub(super) fn dns_provider(nameservers: &[String]) -> Option<&'static str> {
    let known: &[(&str, &str)] = &[
        ("cloudflare", "Cloudflare"),
        ("awsdns", "AWS Route 53"),
        ("azure-dns", "Azure DNS"),
        ("googledomains", "Google"),
        ("google", "Google Cloud DNS"),
        ("nsone", "NS1"),
        ("dnsimple", "DNSimple"),
        ("yandex", "Yandex"),
        ("reg.ru", "REG.RU"),
        ("beget", "Beget"),
        ("hetzner", "Hetzner"),
        ("digitalocean", "DigitalOcean"),
        ("vercel-dns", "Vercel"),
        ("registrar-servers", "Namecheap"),
    ];
    let joined = nameservers.join(" ").to_lowercase();
    known
        .iter()
        .find(|(marker, _)| joined.contains(marker))
        .map(|(_, label)| *label)
}

pub(super) fn mail_provider(mx: &[String]) -> Option<&'static str> {
    let known: &[(&str, &str)] = &[
        ("google", "Google Workspace"),
        ("outlook", "Microsoft 365"),
        ("yandex", "Yandex 360"),
        ("mail.ru", "VK/Mail.ru"),
        ("zoho", "Zoho"),
        ("protonmail", "Proton"),
        ("mailgun", "Mailgun"),
        ("yamdex", "Yandex"),
    ];
    let joined = mx.join(" ").to_lowercase();
    known
        .iter()
        .find(|(marker, _)| joined.contains(marker))
        .map(|(_, label)| *label)
}

/// WHOIS runner result: `(text, server)`.
pub(super) type WhoisResult = (Option<String>, Option<String>);

/// The environment seams `profile_domain` consults; tests inject fakes.
pub struct DomainProbes<'a> {
    /// RDAP query: `(path) -> {supported, data|error}`.
    pub rdap: &'a dyn Fn(&str) -> Json,
    /// DoH query: `(name, record_type) -> records`.
    pub doh: &'a dyn Fn(&str, &str) -> Vec<String>,
    /// WHOIS runner: `(domain) -> (text, server)`.
    pub whois: &'a dyn Fn(&str) -> WhoisResult,
    /// TLS inspector: `(domain) -> result`.
    pub tls: &'a dyn Fn(&str) -> Json,
}
