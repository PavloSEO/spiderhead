//! Profile domain infrastructure: registration, DNS, hosting, ASN, country, TLS.
//!
//! Geolocation stays at country level: a city or coordinates without a licensed database
//! would present a guess as data. Every source degrades independently into structured result data.

use std::collections::BTreeMap;
use std::net::IpAddr;

use regex::Regex;
use serde_json::{json, Map, Value as Json};

use spiderhead_core::net::{resolve_socket_addresses, whois_field};

use crate::net_client::{doh, normalize_domain, rdap, whois_lookup_with};

/// Domain age below which the domain counts as young.
pub const YOUNG_DOMAIN_DAYS: i64 = 180;
/// Expiry window that warrants renewal attention.
pub const EXPIRY_WARN_DAYS: i64 = 30;
/// Certificate expiry warning window.
pub const TLS_EXPIRY_WARN_DAYS: i64 = 14;

const WHOIS_CREATED: &[&str] = &[
    "creation date",
    "created",
    "created on",
    "registered on",
    "domain_dateregistered",
];
const WHOIS_EXPIRES: &[&str] = &[
    "registry expiry date",
    "expiration date",
    "paid-till",
    "expires",
    "expire",
];
const WHOIS_REGISTRAR: &[&str] = &["registrar", "sponsoring registrar", "registrar name"];
const WHOIS_IDENTITY_KEYS: &[&str] = &["domain", "domain name", "domain_name"];

use crate::dates::{civil_from_days, days_from, parse_cert_date, parse_date, today_days};

mod whois;
pub use whois::*;
mod network;
pub use network::*;
mod tls;
pub use tls::*;

/// Build one domain profile whose data sources degrade independently; probes are injected as a bundle.
pub fn profile_domain_with(domain: &str, with_tls: bool, probes: &DomainProbes<'_>) -> Json {
    let name = normalize_domain(domain);
    if name.is_empty() {
        return json!({"ok": false, "error": format!("not a valid domain: {}", crate::net_client::py_repr(domain))});
    }

    let res = (probes.rdap)(&format!("domain/{name}"));
    let mut registration;
    if res["supported"].as_bool() == Some(true) {
        registration = from_rdap_domain(&res["data"]);
        registration["source"] = json!("rdap");
    } else {
        let empty = json!({
            "registrar": null, "created": null, "expires": null, "updated": null,
            "status": [], "nameservers": [],
        });
        let (text, whois_server) = (probes.whois)(&name);
        if let Some(text) = &text {
            if whois_record_is_about(text, &name) {
                registration = from_whois_text(text);
                registration["source"] = json!("whois");
                if let Some(server) = &whois_server {
                    registration["whois_server"] = json!(server);
                }
            } else {
                registration = empty;
                registration["source"] = json!("none");
                registration["whois_note"] = json!(
                    "WHOIS response did not identify the requested domain; no registration \
                     data was taken from it"
                );
            }
        } else {
            registration = empty;
            registration["source"] = json!("none");
        }
        registration["rdap_note"] = res["error"].clone();
    }

    let created = parse_date(registration["created"].as_str().unwrap_or(""));
    let expires = parse_date(registration["expires"].as_str().unwrap_or(""));
    let age_days = days_from(created);
    registration["age_days"] = age_days.map(Json::from).unwrap_or(Json::Null);
    registration["age_years"] = age_days
        .map(|d| json!((d as f64 / 365.25 * 10.0).round() / 10.0))
        .unwrap_or(Json::Null);
    registration["expires_in_days"] = expires
        .map(|d| Json::from(d - today_days()))
        .unwrap_or(Json::Null);

    let a_records = (probes.doh)(&name, "A");
    let mut ns: Vec<String> = registration["nameservers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    ns.sort();
    if ns.is_empty() {
        ns = {
            let mut v = (probes.doh)(&name, "NS");
            v.sort();
            v
        };
    }
    let mut mx = (probes.doh)(&name, "MX");
    mx.sort();
    let aaaa = (probes.doh)(&name, "AAAA");
    let txt: Vec<String> = (probes.doh)(&name, "TXT").into_iter().take(20).collect();
    let cname = (probes.doh)(&format!("www.{name}"), "CNAME");
    let spf = txt
        .iter()
        .find(|t| t.to_lowercase().starts_with("v=spf1"))
        .cloned();
    let dmarc = (probes.doh)(&format!("_dmarc.{name}"), "TXT")
        .first()
        .cloned();
    let mut dns = json!({
        "a": a_records,
        "aaaa": aaaa,
        "ns": ns,
        "mx": mx,
        "txt": txt,
        "cname": cname,
    });
    dns["dns_provider"] = dns["ns"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .and_then(|v| dns_provider(&v).map(|p| json!(p)))
        .unwrap_or(Json::Null);
    dns["mail_provider"] = dns["mx"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .and_then(|v| mail_provider(&v).map(|p| json!(p)))
        .unwrap_or(Json::Null);
    dns["spf"] = spf.map(Json::from).unwrap_or(Json::Null);
    dns["dmarc"] = dmarc.map(Json::from).unwrap_or(Json::Null);

    let ipv4_re = Regex::new(r"^\d+\.\d+\.\d+\.\d+$").unwrap();
    let mut ip = dns["a"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .find(|r| ipv4_re.is_match(r))
        .map(str::to_string);
    let mut ip_source = ip.as_ref().map(|_| "a");
    if ip.is_none() {
        ip = dns["aaaa"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .find(|r| is_ipv6(r))
            .map(str::to_string);
        ip_source = ip.as_ref().map(|_| "aaaa");
    }
    let mut hosting = json!({});
    if let Some(ip) = ip {
        let mut map = json!({"ip": ip, "ip_source": ip_source});
        let asn = asn_via_cymru(probes.doh, &ip);
        for (k, v) in asn {
            map[k] = v;
        }
        let owner = ip_owner(probes.rdap, &ip);
        for (k, v) in owner {
            if !v.is_null() {
                map[k] = v;
            }
        }
        let ptr = reverse_dns_name(&ip)
            .map(|n| (probes.doh)(&n, "PTR"))
            .and_then(|v| v.first().cloned());
        map["reverse_dns"] = ptr.map(Json::from).unwrap_or(Json::Null);
        hosting = map;
    } else if !dns["a"].as_array().unwrap_or(&vec![]).is_empty()
        || !dns["aaaa"].as_array().unwrap_or(&vec![]).is_empty()
    {
        hosting = json!({
            "unavailable_reason":
                "observed A/AAAA records did not parse as a usable IP address",
        });
    }

    let mut profile = json!({
        "ok": true,
        "domain": name,
        "registration": registration,
        "dns": dns,
        "hosting": hosting,
    });
    if with_tls {
        profile["tls"] = (probes.tls)(&name);
    }
    profile["flags"] = json!(flags(&profile));
    profile
}

/// [`profile_domain_with`] over the real client and system tools.
pub fn profile_domain(domain: &str, with_tls: bool, timeout: f64) -> Json {
    let Ok((client, _)) = crate::net_client::http_client(timeout) else {
        return json!({"ok": false, "error": "httpx is required"});
    };
    let probes = DomainProbes {
        rdap: &|path| rdap(&client, path),
        doh: &|name, rtype| doh(&client, name, rtype),
        whois: &|d| whois_lookup_with(d, 15.0, crate::net_client::whois_text),
        tls: &|d| tls(d, 8.0),
    };
    profile_domain_with(domain, with_tls, &probes)
}

/// Summarize actionable risks for a client report.
fn flags(profile: &Json) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let reg = &profile["registration"];
    let tls = &profile["tls"];
    if let Some(age) = reg["age_days"].as_i64() {
        if age < YOUNG_DOMAIN_DAYS {
            out.push(format!(
                "young domain: {age} days old, with little history for search engines"
            ));
        }
    }
    if let Some(left) = reg["expires_in_days"].as_i64() {
        if left < 0 {
            out.push("domain registration has expired".to_string());
        } else if left < EXPIRY_WARN_DAYS {
            out.push(format!("domain registration expires in {left} days"));
        }
    }
    let statuses: Vec<String> = reg["status"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect();
    if statuses.iter().any(|s| s.to_lowercase().contains("hold")) {
        out.push(format!("domain has a hold status: {}", statuses.join(", ")));
    }
    if reg["source"].as_str() == Some("none") {
        if let Some(note) = reg["whois_note"].as_str() {
            out.push(format!("registration data is unavailable: {note}"));
        } else {
            out.push(
                "registration data is unavailable: the registry has no RDAP service and \
                 WHOIS is not installed"
                    .to_string(),
            );
        }
    }
    if profile["dns"]["a"]
        .as_array()
        .map(|a| a.is_empty())
        .unwrap_or(true)
        && profile["dns"]["aaaa"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true)
    {
        out.push("domain does not resolve to an IP address".to_string());
    }
    if profile["dns"]["spf"].is_null() {
        out.push("SPF record is missing".to_string());
    }
    if profile["dns"]["dmarc"].is_null() {
        out.push("DMARC record is missing".to_string());
    }
    if tls["valid"].as_bool() == Some(false) {
        out.push(format!("TLS: {}", tls["error"].as_str().unwrap_or("")));
    }
    if let Some(days_left) = tls["days_left"].as_i64() {
        if days_left < TLS_EXPIRY_WARN_DAYS {
            out.push(format!("certificate expires in {days_left} days"));
        }
    }
    out
}
