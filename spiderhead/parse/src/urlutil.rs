//! URL helpers shared by the parser modules.
//! Thin wrappers over [`crate::py_url`] that map its errors to [`ParseError`].

use crate::py_url;
use crate::types::ParseError;

fn invalid(err: py_url::UrlError) -> ParseError {
    ParseError::Invalid(err.to_string())
}

/// Resolve `href` against `base`; `Err` when the join is invalid.
pub fn urljoin(base: &str, href: &str) -> Result<String, ParseError> {
    py_url::urljoin(base, href).map_err(invalid)
}

/// Lower-cased hostname; `None` when absent; `Err` when the URL cannot be split.
pub fn hostname(url: &str) -> Result<Option<String>, ParseError> {
    let netloc = py_url::urlparse(url).map_err(invalid)?.netloc;
    let hostinfo = netloc.rsplit_once('@').map_or(netloc.as_str(), |(_, h)| h);
    let host = match hostinfo.split_once('[') {
        Some((_, bracketed)) => bracketed.split_once(']').map_or(bracketed, |(h, _)| h),
        None => hostinfo.split_once(':').map_or(hostinfo, |(h, _)| h),
    };
    if host.is_empty() {
        return Ok(None);
    }
    // The part before a `%` zone id is lower-cased; the zone itself keeps its case.
    Ok(Some(match host.split_once('%') {
        Some((name, zone)) => format!("{}%{zone}", name.to_lowercase()),
        None => host.to_lowercase(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_follows_urlparse() {
        // Host handling follows the standard hostname rules.
        assert_eq!(
            hostname("https://User@Ex.COM:80/x")
                .ok()
                .flatten()
                .as_deref(),
            Some("ex.com")
        );
        assert_eq!(
            hostname("https://[::1]:80/").ok().flatten().as_deref(),
            Some("::1")
        );
        assert_eq!(hostname("/relative").ok().flatten(), None);
        assert!(hostname("http://[::1").is_err());
        assert_eq!(
            urljoin("https://a.com/b/", "c").ok().as_deref(),
            Some("https://a.com/b/c")
        );
    }
}
