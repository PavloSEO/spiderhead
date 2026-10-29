//! WHOIS and RDAP field parsing for domain profiling.

use super::*;

/// RDAP `{"events","entities","status","nameservers"}` to registration fields.
pub(super) fn from_rdap_domain(data: &Json) -> Json {
    let mut events: Map<String, Json> = Map::new();
    for e in data["events"].as_array().into_iter().flatten() {
        if let Some(obj) = e.as_object() {
            let action = obj
                .get("eventAction")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            events.insert(action, obj.get("eventDate").cloned().unwrap_or(Json::Null));
        }
    }
    let mut registrar = Json::Null;
    'outer: for entity in data["entities"].as_array().into_iter().flatten() {
        let roles: Vec<String> = entity["roles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.as_str().map(str::to_lowercase))
            .collect();
        if !roles.iter().any(|r| r == "registrar") {
            continue;
        }
        if let Some(fields) = entity["vcardArray"].as_array().and_then(|v| v.get(1)) {
            for field in fields.as_array().into_iter().flatten() {
                if field.as_array().map(|f| f.len()).unwrap_or(0) >= 4
                    && field[0].as_str() == Some("fn")
                {
                    registrar = field[3].clone();
                    break 'outer;
                }
            }
        }
        break;
    }
    let mut nameservers: Vec<String> = data["nameservers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|ns| ns.is_object() && !ns["ldhName"].is_null())
        .filter_map(|ns| {
            ns["ldhName"]
                .as_str()
                .map(|n| n.to_lowercase().trim_end_matches('.').to_string())
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    nameservers.sort();
    json!({
        "registrar": registrar,
        "created": events.get("registration").cloned().unwrap_or(Json::Null),
        "expires": events.get("expiration").cloned().unwrap_or(Json::Null),
        "updated": events
            .get("last changed")
            .or_else(|| events.get("last update of rdap database"))
            .cloned()
            .unwrap_or(Json::Null),
        "status": data["status"]
            .as_array()
            .map(|s| s.iter().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string().trim_matches('"').to_string())).collect::<Vec<_>>())
            .unwrap_or_default(),
        "nameservers": nameservers,
    })
}

/// True when the record names the domain that was asked about.
pub fn whois_record_is_about(text: &str, domain: &str) -> bool {
    let Some(stated) = whois_field(text, WHOIS_IDENTITY_KEYS) else {
        return false;
    };
    !stated.is_empty()
        && stated.trim().trim_end_matches('.').to_lowercase()
            == domain.trim().trim_end_matches('.').to_lowercase()
}

/// Parse essential fields for ccTLDs without RDAP.
pub(super) fn from_whois_text(text: &str) -> Json {
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    let mut multi: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in text.lines() {
        if !line.contains(':') || line.trim_start().starts_with('%') {
            continue;
        }
        let (key, value) = line.split_once(':').unwrap_or((line, ""));
        let (key, value) = (key.trim().to_lowercase(), value.trim().to_string());
        if value.is_empty() {
            continue;
        }
        fields.entry(key.clone()).or_insert(value.clone());
        multi.entry(key).or_default().push(value);
    }
    let pick = |names: &[&str]| -> Json {
        names
            .iter()
            .find_map(|n| fields.get(*n))
            .map(|v| json!(v))
            .unwrap_or(Json::Null)
    };
    let mut nameservers: Vec<String> = ["nserver", "name server"]
        .iter()
        .flat_map(|k| multi.get(*k).cloned().unwrap_or_default())
        .filter_map(|v| {
            v.split_whitespace()
                .next()
                .map(|s| s.to_lowercase().trim_end_matches('.').to_string())
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    nameservers.sort();
    json!({
        "registrar": pick(WHOIS_REGISTRAR),
        "created": pick(WHOIS_CREATED),
        "expires": pick(WHOIS_EXPIRES),
        "updated": Json::Null,
        "status": Vec::<Json>::new(),
        "nameservers": nameservers,
    })
}

pub(super) fn is_ipv6(value: &str) -> bool {
    matches!(value.parse::<IpAddr>(), Ok(IpAddr::V6(_)))
}
