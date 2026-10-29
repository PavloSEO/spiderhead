//! Crawl User-Agent presets, custom strings and the robots.txt product token.
//!
//! Every crawl request and every robots.txt group match reads the identity resolved here. The
//! default identifies this toolkit and links to its source; it imitates no other crawler's
//! string. The identity literals are generated and pinned byte-for-byte (see `presets.rs`).

mod presets;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use serde_json::{Map, Value};
use thiserror::Error;

pub use presets::{
    CHECKS_DOWNLOADER_USER_AGENT, CHECKS_PARSER_USER_AGENT, DEFAULT_TOKEN, DEFAULT_USER_AGENT,
    PRODUCT_VERSION, RECON_USER_AGENT,
};

/// Name of the default preset.
pub const DEFAULT_PRESET: &str = "spiderhead";
/// Longest accepted custom User-Agent, in characters.
pub const MAX_LENGTH: usize = 512;
/// Robots tokens the default identity answered to in earlier releases. Groups written for
/// an old name keep applying, and scans stored under it still resume.
pub const LEGACY_TOKENS: &[&str] = &["seohead-tools"];

/// Raised for a custom User-Agent that is not one printable ASCII header line.
#[derive(Debug, Error, PartialEq, Eq)]
#[error(
    "user agent must be a preset name or one printable ASCII header line of at most 512 characters"
)]
pub struct UserAgentError;

/// A resolved identity: the header value and the robots.txt product token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The User-Agent header sent with every request.
    pub user_agent: String,
    /// The product token matched against robots.txt groups.
    pub robots_token: String,
}

fn presets() -> &'static [(&'static str, &'static str, &'static str)] {
    presets::PRESETS
}

/// Preset names, sorted.
pub fn preset_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = presets().iter().map(|(name, _, _)| *name).collect();
    names.sort_unstable();
    names
}

/// Resolve a preset name, a custom header line, or an empty value to an identity.
///
/// Names are matched case-insensitively after trimming. Anything else must be one printable
/// ASCII line without surrounding spaces; its robots token is its leading product name, or
/// `*` when that name cannot be read.
pub fn resolve(value: Option<&str>) -> Result<Identity, UserAgentError> {
    let value = match value {
        None | Some("") => {
            return Ok(Identity {
                user_agent: DEFAULT_USER_AGENT.to_owned(),
                robots_token: DEFAULT_TOKEN.to_owned(),
            })
        }
        Some(value) => value,
    };
    let key = value.trim().to_lowercase();
    if let Some((_, ua, token)) = presets().iter().find(|(name, _, _)| *name == key) {
        return Ok(Identity {
            user_agent: (*ua).to_owned(),
            robots_token: (*token).to_owned(),
        });
    }
    let printable = value.chars().all(|c| (' '..='~').contains(&c));
    if value.chars().count() > MAX_LENGTH || !printable || value != value.trim() {
        return Err(UserAgentError);
    }
    let token: String = value
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        .collect();
    let robots_token = if token.is_empty() {
        "*".to_owned()
    } else {
        token
    };
    Ok(Identity {
        user_agent: value.to_owned(),
        robots_token,
    })
}

/// True when a JSON value is missing or empty.
fn is_empty(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        Some(_) => false,
    }
}

/// Fill the crawl settings' empty identity fields in place with the resolved default.
pub fn apply_defaults(
    http: &mut Map<String, Value>,
    robots: &mut Map<String, Value>,
) -> Result<(), UserAgentError> {
    if is_empty(http.get("user_agent")) {
        http.insert(
            "user_agent".to_owned(),
            Value::String(DEFAULT_USER_AGENT.to_owned()),
        );
    }
    if is_empty(robots.get("user_agent_token")) {
        let source = http
            .get("user_agent")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_default();
        let token = resolve(Some(&source))?.robots_token;
        robots.insert("user_agent_token".to_owned(), Value::String(token));
    }
    Ok(())
}

/// Lower-cased robots names that address `token`: itself plus, for the default, its old name.
pub fn token_aliases(token: &str) -> BTreeSet<String> {
    let lowered = token.to_lowercase();
    let mut names = BTreeSet::from([lowered.clone()]);
    if lowered == DEFAULT_TOKEN.to_lowercase() {
        names.extend(LEGACY_TOKENS.iter().map(|s| (*s).to_owned()));
    }
    names
}

/// True when two robots tokens select the same groups.
pub fn same_robots_identity(a: &str, b: &str) -> bool {
    token_aliases(b).contains(&a.to_lowercase()) || token_aliases(a).contains(&b.to_lowercase())
}
