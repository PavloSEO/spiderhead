//! Check registry: one row per check id, the single source of truth for the
//! default severity, evidence source, message and fix hint of every check,
//! plus the frames each check needs before it can say anything.
//!
//! The rows are generated data, not hand-written code: a generator script
//! exports them into `data/check_registry.json`, which is embedded at build time.

use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const EMBEDDED: &str = include_str!("../data/check_registry.json");

/// Default severity of a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A defect that makes the page unavailable to users or crawlers.
    Critical,
    /// A defect worth fixing that does not block the page.
    Warning,
    /// A defect worth a look, lower impact than a warning.
    Notice,
    /// An optimisation that costs nothing to skip and hurts no score.
    Opportunity,
}

impl Severity {
    /// Lowercase name as it appears in the catalogue and in reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Warning => "warning",
            Self::Notice => "notice",
            Self::Opportunity => "opportunity",
        }
    }
}

/// Metadata of one check id.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckMeta {
    /// Check identifier, unique within the registry.
    pub id: String,
    /// Default severity; config may override it per check.
    pub severity: Severity,
    /// Evidence tag saying where the verdict comes from.
    pub source: String,
    /// Human-readable message shown with a finding.
    pub message: String,
    /// Fix hint, when the catalogue has one.
    #[serde(default)]
    pub fix: Option<String>,
    /// Path relative to the docs directory that Desktop links to, when present.
    #[serde(default)]
    pub doc_url: Option<String>,
}

/// Frames a check needs in the run before its verdict means anything.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    /// Check that declares the requirement.
    pub id: String,
    /// Loader frames the check needs, in declaration order. Never empty.
    pub frames: Vec<String>,
}

/// Metadata used for an id the registry does not know.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fallback {
    /// Severity given to an unknown id.
    pub severity: Severity,
    /// Evidence tag given to an unknown id.
    pub source: String,
    /// Fix hint given to an unknown id; always absent.
    #[serde(default)]
    pub fix: Option<String>,
}

/// Errors raised while loading a check catalogue.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The catalogue text is not valid JSON of the expected shape.
    #[error("check catalogue is not valid: {0}")]
    Json(#[from] serde_json::Error),
    /// The same check id is declared twice.
    #[error("check id {0} is declared more than once")]
    DuplicateCheck(String),
    /// A requirement names a check the catalogue does not declare.
    #[error("requirement names unknown check {0}")]
    UnknownRequirement(String),
    /// A check declares its requirements twice.
    #[error("requirements of check {0} are declared more than once")]
    DuplicateRequirement(String),
    /// A requirement has no frame, or a frame with an empty name.
    #[error("requirement of check {0} declares no usable frame")]
    EmptyRequirement(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalogue {
    fallback: Fallback,
    checks: Vec<CheckMeta>,
    requires: Vec<Requirement>,
}

/// Validated check catalogue with lookup by id.
#[derive(Debug, Clone)]
pub struct CheckRegistry {
    fallback: Fallback,
    checks: Vec<CheckMeta>,
    index: HashMap<String, usize>,
    requires: Vec<Requirement>,
    requires_index: HashMap<String, usize>,
}

impl CheckRegistry {
    /// Parses and validates a catalogue document.
    ///
    /// Rejects duplicate check ids, requirements on unknown checks, duplicate
    /// requirements and requirements with no usable frame.
    pub fn from_json(text: &str) -> Result<Self, RegistryError> {
        let raw: Catalogue = serde_json::from_str(text)?;
        let mut index = HashMap::with_capacity(raw.checks.len());
        for (position, check) in raw.checks.iter().enumerate() {
            if index.insert(check.id.clone(), position).is_some() {
                return Err(RegistryError::DuplicateCheck(check.id.clone()));
            }
        }
        let mut requires_index = HashMap::with_capacity(raw.requires.len());
        for (position, requirement) in raw.requires.iter().enumerate() {
            if !index.contains_key(&requirement.id) {
                return Err(RegistryError::UnknownRequirement(requirement.id.clone()));
            }
            if requirement.frames.is_empty()
                || requirement.frames.iter().any(|f| f.trim().is_empty())
            {
                return Err(RegistryError::EmptyRequirement(requirement.id.clone()));
            }
            if requires_index
                .insert(requirement.id.clone(), position)
                .is_some()
            {
                return Err(RegistryError::DuplicateRequirement(requirement.id.clone()));
            }
        }
        Ok(Self {
            fallback: raw.fallback,
            checks: raw.checks,
            index,
            requires: raw.requires,
            requires_index,
        })
    }

    /// Number of declared checks.
    pub fn len(&self) -> usize {
        self.checks.len()
    }

    /// True when no check is declared.
    pub fn is_empty(&self) -> bool {
        self.checks.is_empty()
    }

    /// All checks, in catalogue order.
    pub fn checks(&self) -> &[CheckMeta] {
        &self.checks
    }

    /// Metadata of a declared check, or `None` for an unknown id.
    pub fn get(&self, check_id: &str) -> Option<&CheckMeta> {
        self.index
            .get(check_id)
            .and_then(|&position| self.checks.get(position))
    }

    /// Metadata of a check id, falling back to the default for an
    /// unknown id: notice severity, the fallback source, the id as message and
    /// no fix hint.
    pub fn meta_or_fallback(&self, check_id: &str) -> CheckMeta {
        match self.get(check_id) {
            Some(meta) => meta.clone(),
            None => CheckMeta {
                id: check_id.to_owned(),
                severity: self.fallback.severity,
                source: self.fallback.source.clone(),
                message: check_id.to_owned(),
                fix: self.fallback.fix.clone(),
                doc_url: None,
            },
        }
    }

    /// All requirements, in catalogue order.
    pub fn requires(&self) -> &[Requirement] {
        &self.requires
    }

    /// Frames a check declares it needs; empty when it declares none.
    pub fn requirements_of(&self, check_id: &str) -> &[String] {
        self.requires_index
            .get(check_id)
            .and_then(|&position| self.requires.get(position))
            .map_or(&[], |requirement| requirement.frames.as_slice())
    }

    /// Frames a check needs that are not in `available`, in declaration order.
    /// Unknown ids and checks without requirements return an empty list.
    pub fn missing_requirements(&self, check_id: &str, available: &HashSet<&str>) -> Vec<&str> {
        self.requirements_of(check_id)
            .iter()
            .map(String::as_str)
            .filter(|frame| !available.contains(frame))
            .collect()
    }
}

static BUILTIN: OnceLock<Result<CheckRegistry, RegistryError>> = OnceLock::new();

/// The catalogue embedded in the crate, parsed and validated once.
pub fn builtin() -> Result<&'static CheckRegistry, &'static RegistryError> {
    BUILTIN
        .get_or_init(|| CheckRegistry::from_json(EMBEDDED))
        .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin_ok() -> &'static CheckRegistry {
        match builtin() {
            Ok(registry) => registry,
            Err(e) => panic!("embedded catalogue must load: {e}"),
        }
    }

    fn frames(names: &[&'static str]) -> HashSet<&'static str> {
        names.iter().copied().collect()
    }

    #[test]
    fn declared_requirements_name_real_checks_and_frames() {
        let registry = builtin_ok();
        for requirement in registry.requires() {
            assert!(
                registry.get(&requirement.id).is_some(),
                "{} is not a check",
                requirement.id
            );
            assert!(!requirement.frames.is_empty());
            assert!(requirement.frames.iter().all(|f| !f.trim().is_empty()));
        }
    }

    #[test]
    fn missing_requirements_reports_only_absent_frames() {
        let registry = builtin_ok();
        assert!(registry
            .missing_requirements("IMG_MISSING_ALT", &frames(&["images_missing_alt"]))
            .is_empty());
        assert_eq!(
            registry.missing_requirements("IMG_MISSING_ALT", &frames(&[])),
            vec!["images_missing_alt"]
        );
        assert!(registry
            .missing_requirements("NOT_A_CHECK", &frames(&[]))
            .is_empty());
    }

    #[test]
    fn every_declared_check_reports_its_frame_as_missing_when_absent() {
        let registry = builtin_ok();
        for requirement in registry.requires() {
            let missing = registry.missing_requirements(&requirement.id, &frames(&[]));
            let declared: Vec<&str> = requirement.frames.iter().map(String::as_str).collect();
            assert_eq!(missing, declared, "check {}", requirement.id);
        }
    }

    // Pinned counts; regenerate with the generator script and update together.
    #[test]
    fn embedded_catalogue_has_the_oracle_counts() {
        let registry = builtin_ok();
        assert_eq!(registry.len(), 298);
        assert_eq!(registry.requires().len(), 13);
    }

    #[test]
    fn spot_check_entries_match_the_oracle() {
        let registry = builtin_ok();
        let broken = registry.get("BROKEN_PAGE_4XX").expect("declared");
        assert_eq!(broken.severity, Severity::Critical);
        assert_eq!(broken.source, "SF:Response Codes:4xx");
        assert_eq!(broken.message, "Page returns a 4xx response (broken page)");
        assert_eq!(broken.doc_url.as_deref(), Some("scenarios/broken-pages.md"));

        let nonlinked = registry.get("NON_INDEXABLE_LINKED").expect("declared");
        assert_eq!(nonlinked.severity, Severity::Notice);
        assert_eq!(nonlinked.doc_url, None);
    }

    #[test]
    fn severity_names_round_trip_through_the_catalogue() {
        for (name, severity) in [
            ("critical", Severity::Critical),
            ("warning", Severity::Warning),
            ("notice", Severity::Notice),
            ("opportunity", Severity::Opportunity),
        ] {
            assert_eq!(severity.as_str(), name);
            let parsed: Severity =
                serde_json::from_str(&format!("\"{name}\"")).expect("known severity");
            assert_eq!(parsed, severity);
        }
    }

    #[test]
    fn unknown_check_gets_the_oracle_fallback() {
        let registry = builtin_ok();
        let meta = registry.meta_or_fallback("NOT_A_CHECK");
        assert_eq!(meta.severity, Severity::Notice);
        assert_eq!(meta.source, "SF-derived");
        assert_eq!(meta.message, "NOT_A_CHECK");
        assert_eq!(meta.fix, None);
        assert_eq!(meta.doc_url, None);
        assert!(registry.get("NOT_A_CHECK").is_none());
    }

    #[test]
    fn declared_check_is_returned_unchanged_by_the_fallback_accessor() {
        let registry = builtin_ok();
        let declared = registry.get("MISSING_HSTS").expect("declared").clone();
        assert_eq!(registry.meta_or_fallback("MISSING_HSTS"), declared);
    }

    #[test]
    fn requirements_of_an_undeclared_check_is_empty() {
        assert!(builtin_ok().requirements_of("BROKEN_PAGE_4XX").is_empty());
    }

    #[test]
    fn duplicate_check_id_is_rejected() {
        let text = r#"{"fallback":{"severity":"notice","source":"x"},
            "checks":[
              {"id":"A","severity":"warning","source":"s","message":"m"},
              {"id":"A","severity":"notice","source":"s","message":"m"}],
            "requires":[]}"#;
        assert!(
            matches!(CheckRegistry::from_json(text), Err(RegistryError::DuplicateCheck(id)) if id == "A")
        );
    }

    #[test]
    fn requirement_on_unknown_check_is_rejected() {
        let text = r#"{"fallback":{"severity":"notice","source":"x"},"checks":[],
            "requires":[{"id":"GHOST","frames":["f"]}]}"#;
        assert!(
            matches!(CheckRegistry::from_json(text), Err(RegistryError::UnknownRequirement(id)) if id == "GHOST")
        );
    }

    #[test]
    fn requirement_without_a_usable_frame_is_rejected() {
        let empty = r#"{"fallback":{"severity":"notice","source":"x"},
            "checks":[{"id":"A","severity":"warning","source":"s","message":"m"}],
            "requires":[{"id":"A","frames":[]}]}"#;
        assert!(matches!(
            CheckRegistry::from_json(empty),
            Err(RegistryError::EmptyRequirement(_))
        ));
        let blank = empty.replace("\"frames\":[]", "\"frames\":[\" \"]");
        assert!(matches!(
            CheckRegistry::from_json(&blank),
            Err(RegistryError::EmptyRequirement(_))
        ));
    }

    #[test]
    fn duplicate_requirement_is_rejected() {
        let text = r#"{"fallback":{"severity":"notice","source":"x"},
            "checks":[{"id":"A","severity":"warning","source":"s","message":"m"}],
            "requires":[{"id":"A","frames":["f"]},{"id":"A","frames":["g"]}]}"#;
        assert!(matches!(
            CheckRegistry::from_json(text),
            Err(RegistryError::DuplicateRequirement(_))
        ));
    }

    #[test]
    fn unknown_severity_and_unknown_fields_are_rejected() {
        let bad_severity =
            r#"{"fallback":{"severity":"fatal","source":"x"},"checks":[],"requires":[]}"#;
        assert!(matches!(
            CheckRegistry::from_json(bad_severity),
            Err(RegistryError::Json(_))
        ));
        let extra_field = r#"{"fallback":{"severity":"notice","source":"x","colour":"red"},"checks":[],"requires":[]}"#;
        assert!(matches!(
            CheckRegistry::from_json(extra_field),
            Err(RegistryError::Json(_))
        ));
    }
}
