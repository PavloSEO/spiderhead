//! Accessors over `serde_json::Value` with the lenient semantics the report code needs.
//!
//! Missing keys, wrong types and falsy values fall back to defaults instead of erroring,
//! so call sites read as plain field lookups.

use serde_json::Value;

/// The mapping, or `None` when the value is not an object.
pub fn map(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    value.as_object()
}

/// Field lookup on a value that may not be an object.
pub fn get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object()?.get(key)
}

/// Field lookup with a default; a present `null` stays `Some(Null)`.
pub fn get_or<'a>(value: &'a Value, key: &str, default: &'a Value) -> &'a Value {
    get(value, key).unwrap_or(default)
}

/// Text for a report cell: `None` and `""` stay empty, booleans render as `True`/`False`,
/// numbers keep their JSON spelling.
pub fn str_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => {
            if *flag {
                "True".to_owned()
            } else {
                "False".to_owned()
            }
        }
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Field read as a display string ("" when absent or `null`).
pub fn get_str(value: &Value, key: &str) -> String {
    get(value, key).map(str_value).unwrap_or_default()
}

/// A non-empty string value.
pub fn filled_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    match get(value, key) {
        Some(Value::String(text)) if !text.is_empty() => Some(text.as_str()),
        _ => None,
    }
}

/// The array, or `None` when the value is not an array.
pub fn list(value: &Value) -> Option<&Vec<Value>> {
    value.as_array()
}

/// Field read as a slice of items (`&[]` when absent or not an array).
pub fn get_list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    get(value, key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Field read as an object; an empty object when absent.
pub fn get_map<'a>(value: &'a Value, key: &str) -> &'a serde_json::Map<String, Value> {
    static EMPTY: std::sync::LazyLock<serde_json::Map<String, Value>> =
        std::sync::LazyLock::new(serde_json::Map::new);
    get(value, key).and_then(Value::as_object).unwrap_or(&EMPTY)
}

/// Truthiness: `null`, `false`, `0`, `""`, `[]` and `{}` are falsy.
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

/// A JSON integer; booleans and floats are not integers.
pub fn is_int(value: &Value) -> bool {
    matches!(value, Value::Number(n) if n.is_i64() || n.is_u64())
}

/// A JSON number, excluding booleans.
pub fn is_number(value: &Value) -> bool {
    value.is_number()
}

/// Integer view of a JSON number (`i64`, else truncated `f64`, else `None`).
pub fn to_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

/// Field read as `i64`; `0` when absent or non-numeric.
pub fn get_i64(value: &Value, key: &str) -> i64 {
    get(value, key).and_then(to_i64).unwrap_or(0)
}

/// Whether the value is a JSON array.
pub fn is_sequence(value: &Value) -> bool {
    value.is_array()
}

/// Error-text rendering of a value: quoted strings, `None`, `True`/`False`, numbers,
/// and nested lists and objects.
pub fn repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(flag) => if *flag { "True" } else { "False" }.to_owned(),
        Value::Number(_) => str_value(value),
        Value::String(text) => repr_str(text),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", repr_str(k), repr(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// Quote a string for error text, with quote selection and escaping.
pub fn repr_str(text: &str) -> String {
    let has_single = text.contains('\'');
    let quote = if has_single && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// HTML-escape text, including quote characters: `&`, `<`, `>`, `"` and `'`.
pub fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Whether an address is globally routable; translated IPv6 forms are checked through
/// the embedded IPv4.
pub fn is_public_address(value: &str) -> bool {
    use std::net::IpAddr;
    use std::str::FromStr;
    let base = value.split('%').next().unwrap_or(value);
    let Ok(address) = IpAddr::from_str(base) else {
        return false;
    };
    match address {
        IpAddr::V4(v4) => ipv4_is_global(v4),
        IpAddr::V6(v6) => {
            if let Some(embedded) = translated_embedded_ipv4(&v6) {
                return ipv4_is_global(embedded);
            }
            ipv6_is_global(v6)
        }
    }
}

fn in4(v4: std::net::Ipv4Addr, network: [u8; 4], bits: u32) -> bool {
    let value = u32::from(v4);
    let net = u32::from_be_bytes(network);
    let mask = if bits == 0 {
        0
    } else {
        u32::MAX << (32 - bits)
    };
    value & mask == net & mask
}

/// IPv4 global routability per the IANA special-purpose registry.
fn ipv4_is_global(v4: std::net::Ipv4Addr) -> bool {
    const PRIVATE: &[([u8; 4], u32)] = &[
        ([0, 0, 0, 0], 8),
        ([10, 0, 0, 0], 8),
        ([100, 64, 0, 0], 10),
        ([127, 0, 0, 0], 8),
        ([169, 254, 0, 0], 16),
        ([172, 16, 0, 0], 12),
        ([192, 0, 0, 0], 29),
        ([192, 0, 0, 170], 31),
        ([192, 0, 2, 0], 24),
        ([192, 31, 196, 0], 24),
        ([192, 52, 193, 0], 24),
        ([192, 88, 99, 0], 24),
        ([192, 168, 0, 0], 16),
        ([198, 18, 0, 0], 15),
        ([198, 51, 100, 0], 24),
        ([203, 0, 113, 0], 24),
        ([224, 0, 0, 0], 4),
        ([240, 0, 0, 0], 4),
    ];
    if v4 == std::net::Ipv4Addr::new(255, 255, 255, 255) {
        return false;
    }
    !PRIVATE.iter().any(|(net, bits)| in4(v4, *net, *bits))
}

/// Translated IPv6 prefix membership, returning the IPv4 address it wraps.
fn translated_embedded_ipv4(v6: &std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    let packed = v6.octets();
    let in_prefix = |byte_len: usize, prefix: &[u8]| packed[..byte_len] == prefix[..];
    // 64:ff9b::/96, 64:ff9b:1::/48, ::ffff:0:0/96, ::/96 -> last 4 bytes.
    if in_prefix(12, &[0x00, 0x64, 0xff, 0x9b, 0, 0, 0, 0, 0, 0, 0, 0])
        || in_prefix(6, &[0x00, 0x64, 0xff, 0x9b, 0x00, 0x01])
        || in_prefix(12, &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff])
        || in_prefix(12, &[0; 12])
    {
        return Some(std::net::Ipv4Addr::new(
            packed[12], packed[13], packed[14], packed[15],
        ));
    }
    // 2002::/16 6to4 -> bytes 2..6.
    if packed[0] == 0x20 && packed[1] == 0x02 {
        return Some(std::net::Ipv4Addr::new(
            packed[2], packed[3], packed[4], packed[5],
        ));
    }
    None
}

/// IPv6 global routability: non-global ranges are rejected.
fn ipv6_is_global(v6: std::net::Ipv6Addr) -> bool {
    if v6.is_unspecified() || v6.is_loopback() || v6.is_multicast() {
        return false;
    }
    let seg = v6.segments();
    // fe80::/10 link-local, fc00::/7 unique-local, fec0::/10 site-local.
    if seg[0] & 0xffc0 == 0xfe80 || seg[0] & 0xfe00 == 0xfc00 || seg[0] & 0xffc0 == 0xfec0 {
        return false;
    }
    // ::/128 handled above; 100::/64 discard; 2001:db8::/32 documentation.
    if seg[0] == 0x0100 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0 {
        return false;
    }
    if seg[0] == 0x2001 && seg[1] == 0x0db8 {
        return false;
    }
    // 2001::/23 special-purpose block (except known-global subranges 2001:1::1-3,
    // 2001:3::/32, 2001:4:112::/48, 2001:20::/28, 2001:30::/32 approximated).
    if seg[0] & 0xfe00 == 0x0200 {
        let global = (seg[0] == 0x2001 && seg[1] == 0x0003)
            || (seg[0] == 0x2001 && seg[1] == 0x0004 && seg[2] == 0x0112)
            || (seg[0] == 0x2001 && seg[1] & 0xfff0 == 0x0020)
            || (seg[0] == 0x2001 && seg[1] == 0x0030)
            || (seg[0] == 0x2001 && seg[1] == 0x0001 && (seg[2] & 0xfffe) == 0);
        if !global {
            return false;
        }
    }
    true
}
