//! Russian UI titles and fix hints for registry checks.
//!
//! The table lives in `data/check_titles_ru.json` and is embedded at build
//! time. Lookups are case-insensitive. An id missing from the table yields
//! `None`, and callers fall back to the registry's English text.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

const TABLE: &str = include_str!("../data/check_titles_ru.json");

/// One translated check: the short title and the fix hint.
#[derive(Debug, Deserialize)]
pub struct Entry {
    /// Short Russian title shown as the check's name.
    pub title: String,
    /// Russian fix hint.
    pub fix: String,
}

fn table() -> &'static HashMap<String, Entry> {
    static MAP: OnceLock<HashMap<String, Entry>> = OnceLock::new();
    MAP.get_or_init(|| serde_json::from_str(TABLE).unwrap_or_default())
}

fn lookup(id: &str) -> Option<&'static Entry> {
    let map = table();
    map.get(&id.to_uppercase()).or_else(|| map.get(id))
}

/// Russian title of a check, when the table has one.
pub fn title(id: &str) -> Option<&'static str> {
    lookup(id).map(|e| e.title.as_str())
}

/// Russian fix hint of a check, when the table has one.
pub fn fix(id: &str) -> Option<&'static str> {
    lookup(id).map(|e| e.fix.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_cyrillic(text: &str) -> bool {
        text.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c))
    }

    #[test]
    fn every_registry_check_has_a_russian_title_and_fix() {
        let reg = spiderhead_formats::check_registry::builtin().expect("registry");
        assert_eq!(table().len(), reg.len(), "table and registry sizes differ");
        for c in reg.checks() {
            let entry = lookup(&c.id).unwrap_or_else(|| panic!("no title for {}", c.id));
            assert!(!entry.title.trim().is_empty(), "empty title for {}", c.id);
            assert!(!entry.fix.trim().is_empty(), "empty fix for {}", c.id);
            assert!(
                has_cyrillic(&entry.title),
                "title of {} is Latin-only: {}",
                c.id,
                entry.title
            );
            assert!(
                entry.title.chars().count() <= 60,
                "title too long: {}",
                c.id
            );
            assert!(!entry.title.ends_with('.'), "trailing dot in {}", c.id);
        }
    }

    #[test]
    fn lookup_is_case_insensitive_and_unknown_ids_are_none() {
        assert_eq!(title("broken_page_4xx"), title("BROKEN_PAGE_4XX"));
        assert!(title("BROKEN_PAGE_4XX").is_some());
        assert_eq!(title("NO_SUCH_CHECK"), None);
        assert_eq!(fix("NO_SUCH_CHECK"), None);
    }
}
