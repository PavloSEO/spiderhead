//! User-agent presets: which group a crawler belongs to and how its address is
//! authenticated. The table is data (`data/presets.json`, embedded) and users extend it
//! with their own JSON files.
//!
//! Model, from the reference behaviour: a preset has wildcard `matches`, a `group`
//! (bot, browser, search_engine_bot, ai_bot, googlebot, unknown) and an ordered list of
//! verification `steps`. Verification only applies to bot groups.
//!
//! Rules that go beyond the reference:
//! * Steps are ordered and the first that confirms wins (`ip_list` -> `dns_reverse_forward`
//!   -> `asn`), so a crawler with an official list and a PTR domain gets a second chance
//!   when its list is stale or missing.
//! * `dns_reverse` is accepted as an alias of `dns_reverse_forward`: a reverse record alone
//!   is never enough, the forward lookup must contain the original address.
//! * A user file with the same preset `name` replaces the built-in entry; a new `name`
//!   is appended. A malformed user file is an error, never a silent skip.

use std::path::Path;
use std::sync::OnceLock;

use serde_json::Value;

use crate::error::{LogsError, Result};

const BUILTIN: &str = include_str!("../data/presets.json");

/// Group of a user agent. Verification is defined only for bot groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Bot,
    Browser,
    SearchEngineBot,
    AiBot,
    Googlebot,
    Unknown,
}

impl Group {
    /// Machine name used in rows and filters.
    pub fn name(self) -> &'static str {
        match self {
            Self::Bot => "bot",
            Self::Browser => "browser",
            Self::SearchEngineBot => "search_engine_bot",
            Self::AiBot => "ai_bot",
            Self::Googlebot => "googlebot",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a machine name.
    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::Bot,
            Self::Browser,
            Self::SearchEngineBot,
            Self::AiBot,
            Self::Googlebot,
            Self::Unknown,
        ]
        .into_iter()
        .find(|g| g.name() == name)
    }

    /// Whether rows of this group are crawlers (the only ones ever verified).
    pub fn is_bot(self) -> bool {
        !matches!(self, Self::Browser | Self::Unknown)
    }

    /// Whether the group belongs to a virtual group (`all_bots`, `all_ai_bots`, ...).
    pub fn in_virtual(self, virtual_group: &str) -> bool {
        match virtual_group {
            "all" => true,
            "all_bots" => self.is_bot(),
            "all_browsers" => self == Self::Browser,
            "all_googlebots" => self == Self::Googlebot,
            "all_ai_bots" => self == Self::AiBot,
            "all_search_engine_bots" => self == Self::SearchEngineBot,
            "all_unknown" => self == Self::Unknown,
            _ => false,
        }
    }
}

/// One verification method, tried in the order a preset lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Published vendor IP ranges.
    IpList,
    /// Reverse DNS inside the vendor domains, then forward-confirm.
    Dns,
    /// Prefixes announced by an autonomous system (whois).
    Asn,
}

impl Step {
    /// Name stored in presets and reported as the confirmation method.
    pub fn name(self) -> &'static str {
        match self {
            Self::IpList => "ip_list",
            Self::Dns => "dns_reverse_forward",
            Self::Asn => "asn",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "ip_list" => Some(Self::IpList),
            "dns_reverse" | "dns_reverse_forward" => Some(Self::Dns),
            "asn" => Some(Self::Asn),
            _ => None,
        }
    }
}

/// A user-agent preset.
#[derive(Debug, Clone)]
pub struct Preset {
    /// Display name; equals the bot name stored in artifacts.
    pub name: String,
    pub group: Group,
    /// Vendor key: failures of one vendor never slow another.
    pub vendor: String,
    /// Wildcard patterns (`*` only), case-insensitive, over the whole user agent.
    pub matches: Vec<String>,
    pub steps: Vec<Step>,
    /// Range-list provider keys consulted by the `ip_list` step.
    pub lists: Vec<String>,
    /// PTR domain suffixes; empty means the built-in suffixes of the bot name.
    pub suffixes: Vec<String>,
    /// Autonomous system for the `asn` step, e.g. `AS32934`.
    pub asn: Option<String>,
}

/// Where a provider's range list is downloaded from.
#[derive(Debug, Clone)]
pub struct RangeSource {
    pub provider: String,
    /// `None` for lists a vendor does not publish as a file; they come from the ranges dir.
    pub url: Option<String>,
}

/// The preset table plus the range-list sources.
#[derive(Debug, Clone)]
pub struct PresetSet {
    pub presets: Vec<Preset>,
    pub sources: Vec<RangeSource>,
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_preset(v: &Value) -> Result<Preset> {
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let name = text("name").ok_or_else(|| LogsError::value("preset needs a name"))?;
    let group = text("group")
        .and_then(|g| Group::from_name(&g))
        .ok_or_else(|| LogsError::value(format!("preset {name} has an unknown group")))?;
    let mut steps = Vec::new();
    for s in strings(v.get("verification")) {
        steps.push(Step::parse(&s).ok_or_else(|| {
            LogsError::value(format!("preset {name} has an unknown verification {s}"))
        })?);
    }
    if !steps.is_empty() && !group.is_bot() {
        return Err(LogsError::value(format!(
            "preset {name}: verification is only supported for bots"
        )));
    }
    Ok(Preset {
        vendor: text("vendor").unwrap_or_else(|| name.to_lowercase()),
        matches: strings(v.get("matches")),
        lists: strings(v.get("lists")),
        suffixes: strings(v.get("suffixes")),
        asn: text("asn"),
        name,
        group,
        steps,
    })
}

impl PresetSet {
    /// The built-in table.
    pub fn builtin() -> &'static PresetSet {
        static CELL: OnceLock<PresetSet> = OnceLock::new();
        CELL.get_or_init(|| {
            let mut set = PresetSet {
                presets: Vec::new(),
                sources: Vec::new(),
            };
            set.merge_text(BUILTIN).expect("built-in presets are valid");
            set
        })
    }

    /// Merge one JSON document (`{"presets":[..],"sources":[..]}`) into the set.
    pub fn merge_text(&mut self, text: &str) -> Result<()> {
        let doc: Value = serde_json::from_str(text)
            .map_err(|e| LogsError::value(format!("preset file is not JSON: {e}")))?;
        for item in doc
            .get("presets")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let preset = parse_preset(item)?;
            match self.presets.iter_mut().find(|p| p.name == preset.name) {
                Some(slot) => *slot = preset,
                None => self.presets.push(preset),
            }
        }
        for item in doc
            .get("sources")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let provider = item
                .get("provider")
                .and_then(Value::as_str)
                .ok_or_else(|| LogsError::value("source needs a provider"))?
                .to_string();
            let url = item.get("url").and_then(Value::as_str).map(str::to_string);
            match self.sources.iter_mut().find(|s| s.provider == provider) {
                Some(slot) => slot.url = url,
                None => self.sources.push(RangeSource { provider, url }),
            }
        }
        Ok(())
    }

    /// Built-in table extended by every `*.json` file of `dir` (sorted by name).
    pub fn with_user_dir(dir: &Path) -> Result<PresetSet> {
        let mut set = Self::builtin().clone();
        let mut files: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        files.sort();
        for file in files {
            set.merge_text(&std::fs::read_to_string(&file)?)?;
        }
        Ok(set)
    }

    /// Preset of a stored bot name.
    pub fn find(&self, name: &str) -> Option<&Preset> {
        self.presets.iter().find(|p| p.name == name)
    }

    /// Group of a stored bot name; names without a preset are plain bots.
    pub fn group_of(&self, name: &str) -> Group {
        self.find(name).map_or(Group::Bot, |p| p.group)
    }

    /// Preset whose wildcards match a raw user agent, in table order.
    pub fn detect(&self, user_agent: &str) -> Option<&Preset> {
        self.presets
            .iter()
            .find(|p| p.matches.iter().any(|m| wildcard(m, user_agent)))
    }

    /// Source URL of a provider list, if it is published as a file.
    pub fn url_of(&self, provider: &str) -> Option<&str> {
        self.sources
            .iter()
            .find(|s| s.provider == provider)
            .and_then(|s| s.url.as_deref())
    }

    /// Every provider key any preset consults, sorted and unique.
    pub fn providers(&self) -> Vec<String> {
        let mut all: Vec<String> = self
            .presets
            .iter()
            .flat_map(|p| p.lists.iter().cloned())
            .collect();
        all.sort();
        all.dedup();
        all
    }
}

/// Case-insensitive match where `*` stands for any run of characters.
pub fn wildcard(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.to_lowercase(), text.to_lowercase());
    let parts: Vec<&str> = p.split('*').collect();
    if parts.len() == 1 {
        return p == t;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !t.starts_with(first) || t.len() < first.len() + last.len() || !t.ends_with(last) {
        return false;
    }
    let mut rest = &t[first.len()..t.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_table_parses_and_groups() {
        let set = PresetSet::builtin();
        assert_eq!(set.group_of("Googlebot"), Group::Googlebot);
        assert_eq!(set.group_of("GPTBot (OpenAI)"), Group::AiBot);
        assert_eq!(set.group_of("human"), Group::Browser);
        assert!(set.find("Facebook").unwrap().asn.is_some());
        assert!(Group::AiBot.in_virtual("all_bots"));
        assert!(!Group::Browser.in_virtual("all_bots"));
    }

    #[test]
    fn wildcards_anchor_without_stars() {
        assert!(wildcard("*claudebot*", "Mozilla ClaudeBot/1.0"));
        assert!(wildcard("Claude-User*", "claude-user/1"));
        assert!(!wildcard("Claude-User*", "x Claude-User"));
        assert!(wildcard("*bot", "FooBot"));
        assert!(!wildcard("*bot", "FooBot/2"));
    }

    #[test]
    fn user_file_replaces_and_extends() {
        let mut set = PresetSet::builtin().clone();
        set.merge_text(
            r#"{"presets":[{"name":"GPTBot (OpenAI)","group":"ai_bot","verification":[]},
            {"name":"MyBot","group":"bot","matches":["*mybot*"],"verification":["dns_reverse"],"suffixes":[".my.example"]}]}"#,
        )
        .unwrap();
        assert!(set.find("GPTBot (OpenAI)").unwrap().steps.is_empty());
        assert_eq!(set.detect("a MyBot/1").unwrap().steps, vec![Step::Dns]);
        let bad = r#"{"presets":[{"name":"X","group":"browser","verification":["asn"]}]}"#;
        assert!(set.merge_text(bad).is_err());
    }
}
