//! Splits and joins URLs with the `urlsplit`/`urlunsplit` rules the ledger identities depend on.

/// The port is not a number in `0..=65535`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortError;

/// Components of a split URL.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Split {
    /// Lower-cased scheme, empty when absent.
    pub scheme: String,
    /// `user:pass@host:port` part, as written.
    pub netloc: String,
    /// Path.
    pub path: String,
    /// Query without the `?`.
    pub query: String,
    /// Fragment without the `#`.
    pub fragment: String,
}

impl Split {
    /// User part of the netloc.
    pub fn username(&self) -> Option<String> {
        let (userinfo, _) = self.netloc.rsplit_once('@')?;
        Some(userinfo.split(':').next().unwrap_or("").to_string())
    }

    /// Password part of the netloc.
    pub fn password(&self) -> Option<String> {
        let (userinfo, _) = self.netloc.rsplit_once('@')?;
        userinfo.split_once(':').map(|(_, p)| p.to_string())
    }

    fn host_port(&self) -> &str {
        self.netloc
            .rsplit_once('@')
            .map_or(self.netloc.as_str(), |(_, h)| h)
    }

    /// Lower-cased host.
    pub fn hostname(&self) -> Option<String> {
        let hostinfo = self.host_port();
        let host = if let Some(rest) = hostinfo.strip_prefix('[') {
            rest.split(']').next().unwrap_or("")
        } else {
            hostinfo.split(':').next().unwrap_or("")
        };
        if host.is_empty() {
            None
        } else {
            Some(host.to_lowercase())
        }
    }

    /// Port; `Err` when it is not a valid number in range.
    pub fn port(&self) -> Result<Option<u32>, PortError> {
        let hostinfo = self.host_port();
        let after = if let Some(rest) = hostinfo.strip_prefix('[') {
            rest.split_once(']').map(|(_, tail)| tail).unwrap_or("")
        } else {
            hostinfo.split_once(':').map(|(_, p)| p).unwrap_or("")
        };
        let digits = after.strip_prefix(':').unwrap_or(after);
        if digits.is_empty() {
            return Ok(None);
        }
        if !digits.chars().all(|c| c.is_ascii_digit()) {
            return Err(PortError);
        }
        match digits.parse::<u32>() {
            Ok(p) if p <= 65535 => Ok(Some(p)),
            _ => Err(PortError),
        }
    }
}

/// Split a URL the way `urlsplit` does for hierarchical URLs.
pub fn urlsplit(url: &str) -> Split {
    let url: String = url
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let url = url.trim_start_matches(|c: char| c <= ' ');
    let mut scheme = String::new();
    let mut rest = url;
    if let Some(i) = url.find(':') {
        let candidate = &url[..i];
        let mut chars = candidate.chars();
        if chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            scheme = candidate.to_lowercase();
            rest = &url[i + 1..];
        }
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..end].to_string();
        rest = &after[end..];
    }
    let (rest, fragment) = match rest.split_once('#') {
        Some((a, b)) => (a, b.to_string()),
        None => (rest, String::new()),
    };
    let (path, query) = match rest.split_once('?') {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (rest.to_string(), String::new()),
    };
    Split {
        scheme,
        netloc,
        path,
        query,
        fragment,
    }
}

/// Join components back into a URL.
pub fn urlunsplit(scheme: &str, netloc: &str, path: &str, query: &str, fragment: &str) -> String {
    let mut url = path.to_string();
    if !netloc.is_empty() {
        if !url.is_empty() && !url.starts_with('/') {
            url = format!("/{url}");
        }
        url = format!("//{netloc}{url}");
    }
    if !scheme.is_empty() {
        url = format!("{scheme}:{url}");
    }
    if !query.is_empty() {
        url = format!("{url}?{query}");
    }
    if !fragment.is_empty() {
        url = format!("{url}#{fragment}");
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    // Site identity is netloc-based.
    #[test]
    fn splits_like_python() {
        let s = urlsplit("HTTPS://User:pw@Example.COM:8443/a/b?x=1#f");
        assert_eq!(s.scheme, "https");
        assert_eq!(s.netloc, "User:pw@Example.COM:8443");
        assert_eq!(s.path, "/a/b");
        assert_eq!(s.query, "x=1");
        assert_eq!(s.fragment, "f");
        assert_eq!(s.username().as_deref(), Some("User"));
        assert_eq!(s.password().as_deref(), Some("pw"));
        assert_eq!(s.hostname().as_deref(), Some("example.com"));
        assert_eq!(s.port(), Ok(Some(8443)));
        assert_eq!(urlsplit("//host/x").netloc, "host");
        assert_eq!(urlsplit("/just/path").netloc, "");
        assert_eq!(urlsplit("http://a.test").port(), Ok(None));
        assert_eq!(urlsplit("http://a.test:99999/").port(), Err(PortError));
    }

    #[test]
    fn unsplit_round_trips_simple_urls() {
        assert_eq!(
            urlunsplit("https", "example.com", "/", "", ""),
            "https://example.com/"
        );
        assert_eq!(
            urlunsplit("http", "a.test:81", "/p", "q=1", "f"),
            "http://a.test:81/p?q=1#f"
        );
    }
}
