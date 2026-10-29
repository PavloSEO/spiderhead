//! The five advertised tool subsets, mirroring the reference the reference suite registry.

use std::fmt;

/// One of the five MCP profiles a server instance can start with.
///
/// `full` keeps every registered tool; the other four are explicit include-lists
/// that remove unadvertised schemas at startup (the reference server physically
/// removes them, so calling one answers `Unknown tool`, not a profile error).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    Audit,
    Full,
    Infra,
    QuickCheck,
    Router,
}

impl Profile {
    /// All profiles, sorted the way the reference registry reports them.
    pub const ALL: [Profile; 5] = [
        Profile::Audit,
        Profile::Full,
        Profile::Infra,
        Profile::QuickCheck,
        Profile::Router,
    ];

    /// The wire name (`--profile <name>` on both the reference CLI and this binary).
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Audit => "audit",
            Profile::Full => "full",
            Profile::Infra => "infra",
            Profile::QuickCheck => "quick-check",
            Profile::Router => "router",
        }
    }

    /// Russian caption shown to clients, verbatim from `PROFILE_LABELS`.
    pub fn label(self) -> &'static str {
        match self {
            Profile::Audit => "Аудит",
            Profile::Full => "Полный",
            Profile::Infra => "Инфраструктура",
            Profile::QuickCheck => "Быстрая проверка",
            Profile::Router => "Маршрутизатор",
        }
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A profile name the registry does not know; the message is verbatim reference.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("profile must be one of ['audit', 'full', 'infra', 'quick-check', 'router']")]
pub struct UnknownProfile;

impl std::str::FromStr for Profile {
    type Err = UnknownProfile;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Profile::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or(UnknownProfile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_five_wire_names() {
        for profile in Profile::ALL {
            assert_eq!(profile.as_str().parse::<Profile>(), Ok(profile));
        }
    }

    #[test]
    fn labels_match_the_reference_registry() {
        let labels: &[(Profile, &str)] = &[
            (Profile::Full, "Полный"),
            (Profile::Audit, "Аудит"),
            (Profile::QuickCheck, "Быстрая проверка"),
            (Profile::Infra, "Инфраструктура"),
            (Profile::Router, "Маршрутизатор"),
        ];
        for (profile, label) in labels {
            assert_eq!(&profile.label(), label);
        }
    }

    #[test]
    fn unknown_name_raises_the_reference_message() {
        assert_eq!(
            "no-such".parse::<Profile>().unwrap_err().to_string(),
            "profile must be one of ['audit', 'full', 'infra', 'quick-check', 'router']"
        );
    }
}
