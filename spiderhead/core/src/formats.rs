//! On-disk format tags: the package rename accepts both spellings.
//!
//! Artifacts written before the package rename carry `seohead.*` tags; readers normalize that prefix.
//! Writers emit `spiderhead.*` only.

const LEGACY_PREFIX: &str = "seohead.";
const CURRENT_PREFIX: &str = "spiderhead.";

/// Map a legacy `seohead.*` tag onto the current `spiderhead.*` one.
pub fn normalize(format_tag: &str) -> String {
    match format_tag.strip_prefix(LEGACY_PREFIX) {
        Some(rest) => format!("{CURRENT_PREFIX}{rest}"),
        None => format_tag.to_owned(),
    }
}

/// True when the read tag matches the expected format, legacy spelling included.
pub fn matches(actual: Option<&str>, expected: &str) -> bool {
    actual.is_some_and(|tag| normalize(tag) == expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Behaviour of normalize.
    #[test]
    fn legacy_tag_is_normalized_to_current_prefix() {
        assert_eq!(normalize("seohead.scan.v1"), "spiderhead.scan.v1");
        assert_eq!(normalize("spiderhead.scan.v1"), "spiderhead.scan.v1");
        assert_eq!(normalize("other.tag"), "other.tag");
    }

    #[test]
    fn matches_accepts_either_spelling_and_rejects_non_strings() {
        assert!(matches(Some("seohead.audit.v2"), "spiderhead.audit.v2"));
        assert!(matches(Some("spiderhead.audit.v2"), "spiderhead.audit.v2"));
        assert!(!matches(Some("spiderhead.audit.v1"), "spiderhead.audit.v2"));
        assert!(!matches(None, "spiderhead.audit.v2"));
    }
}
