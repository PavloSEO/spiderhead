//! One reader for the toolkit's `SPIDERHEAD_*` environment variables.
//!
//! The `SPIDERHEAD_*` name wins; when it is unset or empty, the legacy `SEOHEAD_*` name is still accepted.
//! Nothing writes legacy names.

use crate::host::Host;

/// Current variable prefix.
pub const PREFIX: &str = "SPIDERHEAD_";
/// Legacy variable prefix, read only.
pub const LEGACY_PREFIX: &str = "SEOHEAD_";

/// Like [`read_env`], but also reports which name supplied the value.
/// Returns `(variable, value)`.
pub fn read_env_named(host: &dyn Host, name: &str) -> Option<(String, String)> {
    let rest = [PREFIX, LEGACY_PREFIX]
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))
        .unwrap_or(name);
    for qualified in [format!("{PREFIX}{rest}"), format!("{LEGACY_PREFIX}{rest}")] {
        if let Some(value) = host.var(&qualified).filter(|value| !value.is_empty()) {
            return Some((qualified, value));
        }
    }
    None
}

/// The value of `SPIDERHEAD_<REST>`, falling back to the legacy `SEOHEAD_<REST>` spelling.
pub fn read_env(host: &dyn Host, name: &str) -> Option<String> {
    read_env_named(host, name).map(|(_, value)| value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::FakeHost;

    const VAR: &str = "SEOHEAD_CONFIG_DIR";

    fn host(pairs: &[(&str, &str)]) -> FakeHost {
        FakeHost {
            env: pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..FakeHost::default()
        }
    }

    #[test]
    fn new_name_is_read() {
        let h = host(&[("SPIDERHEAD_CONFIG_DIR", "/tmp/new")]);
        assert_eq!(read_env(&h, VAR).as_deref(), Some("/tmp/new"));
        assert_eq!(read_env(&h, "CONFIG_DIR").as_deref(), Some("/tmp/new"));
    }

    #[test]
    fn unset_falls_back_to_legacy_name() {
        let h = host(&[("SEOHEAD_CONFIG_DIR", "/tmp/legacy")]);
        assert_eq!(read_env(&h, VAR).as_deref(), Some("/tmp/legacy"));
        assert_eq!(
            read_env(&h, "SPIDERHEAD_CONFIG_DIR").as_deref(),
            Some("/tmp/legacy")
        );
    }

    #[test]
    fn empty_new_name_falls_back_to_legacy() {
        let h = host(&[
            ("SPIDERHEAD_CONFIG_DIR", ""),
            ("SEOHEAD_CONFIG_DIR", "/tmp/legacy"),
        ]);
        assert_eq!(read_env(&h, VAR).as_deref(), Some("/tmp/legacy"));
    }

    #[test]
    fn new_name_wins_when_both_set() {
        let h = host(&[
            ("SPIDERHEAD_CONFIG_DIR", "/tmp/new"),
            ("SEOHEAD_CONFIG_DIR", "/tmp/legacy"),
        ]);
        assert_eq!(read_env(&h, VAR).as_deref(), Some("/tmp/new"));
    }

    #[test]
    fn unset_returns_none() {
        assert_eq!(read_env(&host(&[]), VAR), None);
    }

    #[test]
    fn read_env_named_reports_which_name_won() {
        let legacy = host(&[("SEOHEAD_CONFIG_DIR", "/tmp/legacy")]);
        assert_eq!(
            read_env_named(&legacy, VAR),
            Some(("SEOHEAD_CONFIG_DIR".into(), "/tmp/legacy".into()))
        );
        let both = host(&[
            ("SPIDERHEAD_CONFIG_DIR", "/tmp/new"),
            ("SEOHEAD_CONFIG_DIR", "/tmp/legacy"),
        ]);
        assert_eq!(
            read_env_named(&both, VAR),
            Some(("SPIDERHEAD_CONFIG_DIR".into(), "/tmp/new".into()))
        );
        assert_eq!(read_env_named(&host(&[]), VAR), None);
    }
}
