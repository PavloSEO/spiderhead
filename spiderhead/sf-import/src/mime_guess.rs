//! `mimetypes.guess_type` for path inputs — the subset `check_html_structure`
//! needs (`mimetypes.guess_type(urllib.parse.urlsplit(url).path)`).
//!
//! The path form always lands in `guess_file_type`: `urlparse(path).scheme` is
//! never longer than one character for a split-produced path, so the URL branch
//! (`data:` handling included) is unreachable and is not ported. The strict
//! `types_map` snapshot lives in [`crate::mime_map`] and is regenerated from
//! the oracle environment.

use crate::mime_map::{ENCODINGS_MAP, SUFFIX_MAP, TYPES_MAP};

/// `posixpath.splitext`: split at the last `.` that is not all of the leading
/// dots of the final path component.
fn splitext(p: &str) -> (&str, &str) {
    let sep_index = p.rfind('/');
    let Some(dot_index) = p.rfind('.') else {
        return (p, "");
    };
    if sep_index.is_some_and(|s| dot_index <= s) {
        return (p, "");
    }
    let mut i = sep_index.map(|s| s + 1).unwrap_or(0);
    while i < dot_index {
        if p.as_bytes()[i] != b'.' {
            return (&p[..dot_index], &p[dot_index..]);
        }
        i += 1;
    }
    (p, "")
}

fn lookup<'m>(map: &'m [(&str, &str)], key: &str) -> Option<&'m str> {
    map.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// `MimeTypes._guess_file_type(path, strict=True, posixpath.splitext)` —
/// returns `(type, encoding)`; either half may be `None`.
fn guess_file_type(path: &str) -> (Option<String>, Option<String>) {
    let (base, ext) = splitext(path);
    let mut base = base.to_owned();
    let mut ext = ext.to_owned();
    // `.tgz`/`.taz`/… collapse through the suffix map until the tail settles.
    while let Some(replacement) = lookup(SUFFIX_MAP, &ext.to_lowercase()) {
        let joined = format!("{base}{replacement}");
        let (b, e) = splitext(&joined);
        base = b.to_owned();
        ext = e.to_owned();
    }
    // `encodings_map` is case sensitive.
    let (ext, encoding) = match lookup(ENCODINGS_MAP, &ext) {
        Some(enc) => {
            let (_b, e) = splitext(&base);
            (e.to_owned(), Some(enc.to_owned()))
        }
        None => (ext, None),
    };
    let ext_lower = ext.to_lowercase();
    (lookup(TYPES_MAP, &ext_lower).map(str::to_owned), encoding)
}

/// `mimetypes.guess_type(path)` reduced to its type half. `None` when the
/// extension names no known type — the same "not a recognized extension"
/// outcome reference returns.
pub fn guess_type(path: &str) -> Option<String> {
    guess_file_type(path).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitext_skips_leading_dots() {
        assert_eq!(splitext("foo/.hidden"), ("foo/.hidden", ""));
        assert_eq!(splitext("foo/.."), ("foo/..", ""));
        assert_eq!(splitext("/a/b/c.tar.gz"), ("/a/b/c.tar", ".gz"));
        assert_eq!(splitext("a."), ("a", "."));
    }

    #[test]
    fn guess_common_types() {
        assert_eq!(guess_type("/a/b.png").as_deref(), Some("image/png"));
        assert_eq!(guess_type("/a/b.HTML").as_deref(), Some("text/html"));
        assert_eq!(guess_type("/a/b"), None);
        assert_eq!(guess_type("/a/.well-known/thing"), None);
        // `.tgz` collapses to `.tar.gz` → the `.tar` type with gzip encoding;
        // the type half is what the caller reads.
        assert_eq!(guess_type("/x/a.tgz").as_deref(), Some("application/x-tar"));
    }
}
