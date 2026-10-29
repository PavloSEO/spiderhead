//! Validation helpers for GA4 page-content inputs: decimal bounds, time-zone names and URL parts.
use super::*;

/// True when a verified decimal string's value is below 1, which means every digit is zero.
/// Strings over 4300 digits are an invalid-value error before the comparison.
pub(super) fn decimal_lt_one(text: &str) -> Result<bool, Ga4Error> {
    let digits = text.chars().count();
    if digits > 4300 {
        return Err(Ga4Error::Python {
            class: "ValueError".to_owned(),
            message: format!(
                "Exceeds the limit (4300 digits) for integer string conversion:                  value has {digits} digits; use sys.set_int_max_str_digits()                  to increase the limit"
            ),
        });
    }
    Ok(text.chars().all(|c| pystr::nd_digit(c) == Some(0)))
}

/// Path normalization, enough for the time-zone key check.
pub(super) fn posix_normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let initial = path.bytes().take_while(|b| *b == b'/').count();
    // POSIX preserves exactly two leading slashes (implementation-defined);
    // three or more collapse to one.
    let (root, rest) = match initial {
        0 => ("", path),
        1 => ("/", &path[1..]),
        2 => ("//", &path[2..]),
        n => ("/", &path[n..]),
    };
    let mut comps: Vec<&str> = Vec::new();
    for comp in rest.split('/') {
        match comp {
            "" | "." => {}
            ".." if comps.last() == Some(&"..") || (comps.is_empty() && root.is_empty()) => {
                comps.push("..")
            }
            ".." => {
                comps.pop();
            }
            c => comps.push(c),
        }
    }
    let joined = comps.join("/");
    if joined.is_empty() && root.is_empty() {
        ".".to_owned()
    } else {
        format!("{root}{joined}")
    }
}

/// Time-zone key validation that runs before any lookup: `None` when the key shape is valid,
/// `Some(message)` with the exact error text otherwise.
#[doc(hidden)]
pub fn zoneinfo_key_check(name: &str) -> Option<String> {
    if name != posix_normpath(name) {
        return Some(format!(
            "ZoneInfo keys must be normalized relative paths, got: {name}"
        ));
    }
    if name.starts_with('/') {
        return Some(format!(
            "ZoneInfo keys may not be absolute paths, got: {name}"
        ));
    }
    if name.split('/').any(|part| part == "..") {
        return Some(format!(
            "ZoneInfo keys must refer to subdirectories of TZPATH, got: {name}"
        ));
    }
    None
}

/// Time-zone lookup: `Ok` resolves, `Err(None)` means the zone is not found, and
/// `Err(Some(msg))` is the error for an invalid key.
pub(super) fn zoneinfo_probe(name: &str) -> Result<(), Option<String>> {
    if let Some(message) = zoneinfo_key_check(name) {
        return Err(Some(message));
    }
    // Search order: the environment override first, then the platform defaults.
    let dirs: Vec<PathBuf> = match std::env::var_os("PYTHONTZPATH") {
        Some(value) if !value.is_empty() => std::env::split_paths(&value).collect(),
        _ => [
            "/usr/share/zoneinfo",
            "/usr/lib/zoneinfo",
            "/usr/share/lib/zoneinfo",
            "/etc/zoneinfo",
        ]
        .iter()
        .map(PathBuf::from)
        .collect(),
    };
    for dir in dirs {
        let candidate = dir.join(name);
        if let Ok(mut file) = std::fs::File::open(&candidate) {
            let mut magic = [0u8; 4];
            if file.read_exact(&mut magic).is_ok() && &magic == b"TZif" {
                return Ok(());
            }
        }
    }
    Err(None)
}

/// Userinfo as `(username, password)`; an empty string differs from None.
pub(super) fn userinfo(netloc: &str) -> (Option<String>, Option<String>) {
    match netloc.rsplit_once('@') {
        Some((info, _)) => match info.split_once(':') {
            Some((u, p)) => (Some(u.to_owned()), Some(p.to_owned())),
            None => (Some(info.to_owned()), None),
        },
        None => (None, None),
    }
}

/// Host and raw port string, split from the authority.
pub(super) fn hostinfo(netloc: &str) -> (String, Option<String>) {
    let host = netloc.rsplit('@').next().unwrap_or_default();
    match host.find('[') {
        Some(open) => {
            let bracketed = &host[open + 1..];
            let (hostname, rest) = match bracketed.split_once(']') {
                Some((h, p)) => (h.to_owned(), p),
                None => (bracketed.to_owned(), ""),
            };
            let port = rest.split_once(':').map(|(_, p)| p.to_owned());
            (hostname, port)
        }
        None => {
            let (hostname, port) = match host.split_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (host, None),
            };
            (hostname.to_owned(), port.map(str::to_owned))
        }
    }
}

/// Hostname, lower-cased outside a scoped-IPv6 `%zone` suffix.
/// Non-ASCII names are not IDNA-encoded here; a bare origin never carries them in supported inputs.
pub(super) fn hostname(netloc: &str) -> Option<String> {
    let (hostname, _) = hostinfo(netloc);
    if hostname.is_empty() {
        return None;
    }
    Some(match hostname.split_once('%') {
        Some((host, zone)) => format!("{}%{zone}", host.to_lowercase()),
        None => hostname.to_lowercase(),
    })
}

/// Port: `Ok(None)` when absent, `Ok(Some(_))` when parsed, and `Err` with the exact error text for an invalid port.
pub(super) fn port(netloc: &str) -> Result<Option<i64>, String> {
    let (_, raw) = hostinfo(netloc);
    let Some(text) = raw.filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "Port could not be cast to integer value as {}",
            pystr::repr_str(&text)
        ));
    }
    // Integers are unbounded: an oversized digit string gives the range error.
    let port: i64 = text
        .parse()
        .map_err(|_| "Port out of range 0-65535".to_owned())?;
    if !(0..=65535).contains(&port) {
        return Err("Port out of range 0-65535".to_owned());
    }
    Ok(Some(port))
}

/// Case fold for hostname comparison: lower-case plus the `ß` to `ss` fold, trailing dots removed.
/// Other case-fold differences are non-ASCII edge letters that a bound hostname does not carry.
pub(super) fn folded_host(name: &str) -> String {
    name.to_lowercase()
        .replace(['ß', 'ẞ'], "ss")
        .trim_end_matches('.')
        .to_owned()
}
