//! Phrase normalization, intent filters and stop lists.
//!
//! `normalize` is the global deduplication key. Intent patterns match on word
//! boundaries; a project picks a preset in `project.yaml` and may override
//! `intent`/`stop`/`info`.

use fancy_regex::Regex;

/// Lowercase, map `ё` to `е`, and drop everything but
/// `0-9 a-z а-я space hyphen`, hyphen→space, collapse whitespace.
pub fn normalize(s: &str) -> String {
    let mut t = s.trim().to_lowercase().replace('ё', "е");
    // Disallowed characters become spaces; runs of whitespace are collapsed
    // afterwards.
    t = t
        .chars()
        .map(|c| {
            if c.is_ascii_digit()
                || c.is_ascii_lowercase()
                || ('а'..='я').contains(&c)
                || c == ' '
                || c == '-'
            {
                c
            } else {
                ' '
            }
        })
        .collect();
    t = t.replace('-', " ");
    collapse_ws(&t)
}

/// Collapse runs of Unicode whitespace to one space and trim the ends.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = true; // leading strip
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    if in_ws {
        out.pop();
    }
    out
}

/// Generic junk words shared by every topic.
pub const JUNK: &str = r"порно|секс|торрент|скачать бесплатн|взлом|кряк|бесплатно скачать";

/// Intent patterns of the SEO topic preset.
pub const SEO_INTENT: &str = r"\bseo\b|\bсео\b|продвижени\w*|оптимизаци\w*|аудит\w*|краулер\w*|парсер\w*|индексаци\w*|\bsitemap\w*|\brobots\b|редирект\w*|\bканоническ\w*|\bcanonical\b|битых? ссыл\w*|скорост\w* (сайт|загруз)|позици\w* сайт|семантическ\w* ядр\w*|ключев\w* слов\w*|screaming frog|\bmcp\b";

/// Informational patterns of the SEO topic preset.
pub const SEO_INFO: &str =
    r"что такое|как сделать|как провести|как проверить|зачем|пример|чек[- ]?лист|гайд";

/// Preset tuple `(intent, stop, info)`; `intent = None` means no topic filter.
fn preset_for(name: &str) -> Option<(Option<&'static str>, String, &'static str)> {
    match name {
        "seo" => Some((
            Some(SEO_INTENT),
            format!("{JUNK}|ваканси|курсы|обучени"),
            SEO_INFO,
        )),
        "generic" => Some((None, format!("{JUNK}|ваканси|работа"), r"(?!x)x")),
        _ => None,
    }
}

/// Compiled project filters: the preset is the base, explicit
/// patterns override it.
pub struct Filters {
    /// Keep every phrase regardless of topic intent.
    pub any: bool,
    intent: Option<Regex>,
    stop: Regex,
    info: Regex,
}

impl Filters {
    /// Constructor arguments: intent, stop, info, any_intent, preset.
    pub fn new(
        intent: Option<&str>,
        stop: Option<&str>,
        info: Option<&str>,
        any_intent: bool,
        preset: &str,
    ) -> Result<Self, crate::Error> {
        let (p_intent, p_stop, p_info) = preset_for(preset).ok_or_else(|| {
            crate::Error::Config(format!(
                "unknown preset: {preset}; choose one of [\"generic\", \"seo\"]"
            ))
        })?;
        let eff_intent = intent.or(p_intent);
        let any = any_intent || (preset == "generic" && intent.is_none());
        let intent_re = if any {
            None
        } else {
            match eff_intent {
                Some(pat) => Some(compile(pat)?),
                None => None,
            }
        };
        let stop_pat = match stop {
            Some(s) => format!("(?:{p_stop})|(?:{s})"),
            None => p_stop,
        };
        Ok(Self {
            any,
            intent: intent_re,
            stop: compile(&stop_pat)?,
            info: compile(info.unwrap_or(p_info))?,
        })
    }

    /// Build the filters from a project config.
    pub fn from_config(cfg: &crate::config::Cfg) -> Result<Self, crate::Error> {
        let get = |k: &str| {
            cfg.get(k)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        };
        Self::new(
            get("intent"),
            get("stop"),
            get("info"),
            // `any_intent` is read as a truthiness test, not a bool cast:
            // 1 and "yes" are truthy, so the config bool rules apply.
            crate::config::cfg_bool(cfg, "any_intent", false),
            cfg.get("preset")
                .and_then(|v| v.as_str())
                .unwrap_or("generic"),
        )
    }

    /// Whether the phrase hits the stop list.
    pub fn is_stop(&self, p: &str) -> Result<bool, crate::Error> {
        matches(&self.stop, p)
    }

    /// Whether the phrase has informational intent.
    pub fn is_info(&self, p: &str) -> Result<bool, crate::Error> {
        matches(&self.info, p)
    }

    /// Whether the phrase matches the intent patterns; always true when `any`.
    pub fn has_intent(&self, p: &str) -> Result<bool, crate::Error> {
        if self.any {
            return Ok(true);
        }
        match &self.intent {
            None => Ok(false),
            Some(re) => matches(re, p),
        }
    }

    /// Route: `info` or `commercial`.
    pub fn route(&self, p: &str) -> Result<&'static str, crate::Error> {
        Ok(if self.is_info(p)? {
            "info"
        } else {
            "commercial"
        })
    }

    /// Return `(status, route)`; status is kept, review or dropped.
    pub fn classify(
        &self,
        p: &str,
        base: Option<i64>,
        min_base: i64,
    ) -> Result<(&'static str, &'static str), crate::Error> {
        let route = self.route(p)?;
        if self.is_stop(p)? {
            return Ok(("dropped", route));
        }
        if p.chars().count() < 4 || p.split_whitespace().count() > 9 {
            return Ok(("dropped", route));
        }
        if !self.has_intent(p)? {
            return Ok(("review", route));
        }
        if let Some(b) = base {
            if b < min_base {
                return Ok(("review", route));
            }
        }
        Ok(("kept", route))
    }
}

/// fancy-regex runtime errors (for example the backtrack limit on a
/// user-supplied pattern) surface as errors instead of a silent non-match —
/// a misclassified phrase must be loud, not quietly wrong.
fn matches(re: &Regex, p: &str) -> Result<bool, crate::Error> {
    re.find(p)
        .map(|m| m.is_some())
        .map_err(|e| crate::Error::Config(format!("pattern match failed: {e}")))
}

/// Compile a pattern through fancy-regex (lookaround and backrefs are needed
/// by the preset `(?!x)x` and word-boundary classes).
fn compile(pat: &str) -> Result<Regex, crate::Error> {
    Regex::new(pat).map_err(|e| crate::Error::Config(format!("invalid regex {pat:?}: {e}")))
}

/// Length in code points, not bytes.
pub fn py_len(s: &str) -> usize {
    s.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_matches_reference() {
        assert_eq!(
            normalize("  SEO-Продвижение  Сайта! "),
            "seo продвижение сайта"
        );
        assert_eq!(normalize("Ёлка  ЁЖ"), "елка еж");
        assert_eq!(normalize("купить\tчасы\nнедорого"), "купить часы недорого");
        assert_eq!(normalize("a+b=c"), "a b c");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn generic_preset_any_intent() {
        let f = Filters::new(None, None, None, false, "generic").unwrap();
        assert!(f.any);
        assert!(f.is_stop("порно видео").unwrap());
        assert_eq!(
            f.classify("краулер сайта онлайн", Some(50), 20).unwrap(),
            ("kept", "commercial")
        );
        assert_eq!(f.classify("ab", Some(50), 20).unwrap().0, "dropped");
        assert_eq!(
            f.classify("слово ".repeat(10).trim(), Some(50), 20)
                .unwrap()
                .0,
            "dropped"
        );
        assert_eq!(f.classify("порно сайт", Some(50), 20).unwrap().0, "dropped");
    }

    #[test]
    fn seo_preset_review_when_no_intent() {
        let f = Filters::new(None, None, None, false, "seo").unwrap();
        assert!(!f.any);
        assert_eq!(f.classify("краулер сайта", Some(50), 20).unwrap().0, "kept");
        assert_eq!(
            f.classify("котлы навьен", Some(50), 20).unwrap().0,
            "review"
        );
        assert!(f.is_info("что такое seo").unwrap());
        assert_eq!(f.route("как проверить скорость").unwrap(), "info");
    }

    #[test]
    fn any_intent_uses_python_truthiness() {
        // regression: `any_intent: 1` or `any_intent: "yes"` must disable the topic filter.
        fn filters_with(value: &serde_json::Value) -> Filters {
            let mut cfg = crate::config::Cfg::new();
            cfg.insert("any_intent".into(), value.clone());
            cfg.insert("preset".into(), serde_json::json!("seo"));
            Filters::from_config(&cfg).unwrap()
        }
        for value in [
            serde_json::json!(1),
            serde_json::json!("yes"),
            serde_json::json!(1.5),
        ] {
            assert!(filters_with(&value).any, "{value}");
        }
        for value in [
            serde_json::json!(0),
            serde_json::json!(""),
            serde_json::json!(false),
            serde_json::Value::Null,
        ] {
            assert!(!filters_with(&value).any, "{value}");
        }
    }

    #[test]
    fn regex_runtime_errors_surface_instead_of_a_silent_miss() {
        // regression: a backtrack-limit failure on a user pattern must not read
        // as "no match" and silently reclassify the phrase.
        // A lookahead inside the quantified group keeps fancy_regex on its
        // backtracking VM (no NFA delegation). The required `a$` literal is
        // present in the haystack, so the seek pre-filter cannot prune; the
        // trailing `b` then forces an exponential partition search that
        // exhausts the default backtrack limit.
        let blowup = Regex::new(r"^(a|a(?=a))*a$").unwrap();
        let f = Filters {
            any: false,
            intent: Some(blowup.clone()),
            stop: blowup.clone(),
            info: blowup,
        };
        let input = format!("{}b", "a".repeat(100_000));
        assert!(f.is_stop(&input).is_err());
        assert!(f.is_info(&input).is_err());
        assert!(f.has_intent(&input).is_err());
        assert!(f.classify(&input, Some(50), 20).is_err());
    }
}
