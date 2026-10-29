//! Bot verification by published IP ranges: the provider range lists, the six-valued
//! verification status, and the bots-only status filter over a retained artifact.
//!
//! Range lists come from local files or an injected [`RangeFetcher`]; nothing here opens a
//! socket on its own.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;
use serde_json::{json, Map, Value};

use crate::bot_evidence::cached_evidence;
use crate::error::{LogsError, Result};
use crate::net::{parse_ip, Network};
use crate::store::{open_log_source, paging};

/// Outcome of authenticating a bot's claim from its address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationStatus {
    /// The address passed the bot's verification method.
    Verified,
    /// The address failed it: a user-agent claim from a foreign address, including a PTR
    /// outside the vendor domain or a forward lookup that does not return the address.
    Spoofed,
    /// No evidence yet (no cached DNS answer, or no range list loaded for the provider).
    Unverified,
    /// The bot has no published verification method.
    Unsupported,
    /// Reverse DNS returned no name for the address.
    PtrFailed,
    /// DNS was unreachable; retry rather than judge.
    NetworkError,
    /// The resolver does not answer even the control lookup: DNS cannot be trusted in
    /// this environment at all, so no DNS-based verdict is possible here.
    ResolverUnavailable,
}

impl VerificationStatus {
    /// Every status, in code order.
    pub const ALL: [Self; 7] = [
        Self::Verified,
        Self::Spoofed,
        Self::Unverified,
        Self::Unsupported,
        Self::PtrFailed,
        Self::NetworkError,
        Self::ResolverUnavailable,
    ];

    /// Stable numeric code (1 verified .. 7 resolver unavailable).
    pub fn code(self) -> u8 {
        match self {
            Self::Verified => 1,
            Self::Spoofed => 2,
            Self::Unverified => 3,
            Self::Unsupported => 4,
            Self::PtrFailed => 5,
            Self::NetworkError => 6,
            Self::ResolverUnavailable => 7,
        }
    }

    /// Machine name used in filters and rows.
    pub fn name(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Spoofed => "spoofed",
            Self::Unverified => "unverified",
            Self::Unsupported => "unsupported",
            Self::PtrFailed => "ptr_failed",
            Self::NetworkError => "network_error",
            Self::ResolverUnavailable => "resolver_unavailable",
        }
    }

    /// Parse a machine name; `None` for anything else.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// Range-list key for a bot display name, `None` when its vendor publishes no list.
pub fn provider_for(bot: &str) -> Option<&'static str> {
    Some(match bot {
        "GPTBot (OpenAI)" => "gptbot",
        "OAI-SearchBot (OpenAI)" => "oai-searchbot",
        "ChatGPT-User" => "chatgpt-user",
        "PerplexityBot" => "perplexitybot",
        "ClaudeBot (Anthropic)" => "claudebot",
        _ => return None,
    })
}

/// Parsed provider range lists, keyed by provider.
#[derive(Debug, Default, Clone)]
pub struct RangeBook {
    providers: HashMap<String, Vec<Network>>,
}

impl RangeBook {
    /// An empty book: every ranged bot stays unverified until a list is loaded.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse a provider JSON list and replace the provider's ranges; returns the count.
    ///
    /// Accepts `{"prefixes":[{"ipv4Prefix":..}|{"ipv6Prefix":..}]}` and a bare array of
    /// CIDR strings or single addresses. An empty or malformed list is an error so a bad download never turns
    /// every visit into a spoof.
    pub fn load_json(&mut self, provider: &str, text: &str) -> Result<usize> {
        let bad = |what: &str| LogsError::value(format!("range list for {provider} {what}"));
        let doc: Value =
            serde_json::from_str(text).map_err(|e| bad(&format!("is not JSON: {e}")))?;
        let items = match &doc {
            Value::Array(items) => items.as_slice(),
            Value::Object(o) => o
                .get("prefixes")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .ok_or_else(|| bad("has no prefixes"))?,
            _ => return Err(bad("is malformed")),
        };
        let mut nets = Vec::with_capacity(items.len());
        for item in items {
            let cidr = match item {
                Value::String(s) => Some(s.as_str()),
                Value::Object(o) => [
                    "ipv4Prefix",
                    "ipv6Prefix",
                    "ipv4Address",
                    "ipv6Address",
                    "ip_prefix",
                    "cidr",
                ]
                .iter()
                .find_map(|k| o.get(*k).and_then(Value::as_str)),
                _ => None,
            };
            nets.push(Network::parse(
                cidr.ok_or_else(|| bad("has an entry without a prefix"))?,
            )?);
        }
        if nets.is_empty() {
            return Err(bad("is empty"));
        }
        let count = nets.len();
        self.providers.insert(provider.to_string(), nets);
        Ok(count)
    }

    /// Load `<provider>.json` from a local file.
    pub fn load_file(&mut self, provider: &str, path: &Path) -> Result<usize> {
        self.load_json(provider, &std::fs::read_to_string(path)?)
    }

    /// Load every `<provider>.json` in a directory; returns the providers loaded, sorted.
    pub fn load_dir(&mut self, dir: &Path) -> Result<Vec<String>> {
        let mut loaded = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let (Some(stem), Some("json")) = (
                path.file_stem().and_then(|s| s.to_str()),
                path.extension().and_then(|s| s.to_str()),
            ) else {
                continue;
            };
            self.load_file(stem, &path)?;
            loaded.push(stem.to_string());
        }
        loaded.sort();
        Ok(loaded)
    }

    /// Whether a list is loaded for `provider`.
    pub fn has(&self, provider: &str) -> bool {
        self.providers.contains_key(provider)
    }

    /// `Some(true/false)` when a list is loaded and `ip` is/is not inside it; `None` when
    /// no list is loaded or `ip` is not an address.
    pub fn contains(&self, provider: &str, ip: &str) -> Option<bool> {
        let nets = self.providers.get(provider)?;
        let addr = parse_ip(ip)?;
        Some(nets.iter().any(|n| n.contains(addr)))
    }
}

/// Source of provider range lists; the only place a network fetch may live.
pub trait RangeFetcher {
    /// Download the raw JSON of `provider`'s list, or describe the failure.
    fn fetch(&self, provider: &str) -> std::result::Result<String, String>;
}

/// Refresh `providers` through `fetcher`. A failing provider keeps its previous list.
pub fn refresh(
    book: &mut RangeBook,
    fetcher: &dyn RangeFetcher,
    providers: &[&str],
) -> Vec<(String, std::result::Result<usize, String>)> {
    providers
        .iter()
        .map(|p| {
            let outcome = fetcher
                .fetch(p)
                .and_then(|text| book.load_json(p, &text).map_err(|e| e.to_string()));
            (p.to_string(), outcome)
        })
        .collect()
}

/// Status of `bot` seen at `ip` by published ranges alone.
pub fn range_status(book: &RangeBook, bot: &str, ip: &str) -> VerificationStatus {
    let Some(provider) = provider_for(bot) else {
        return VerificationStatus::Unsupported;
    };
    match book.contains(provider, ip) {
        Some(true) => VerificationStatus::Verified,
        Some(false) => VerificationStatus::Spoofed,
        None => VerificationStatus::Unverified,
    }
}

/// One bot address with its computed status.
#[derive(Debug, Clone)]
pub struct BotStatus {
    pub bot: String,
    pub family: String,
    pub ip: String,
    pub visits: i64,
    pub last_visit: Option<String>,
    pub status: VerificationStatus,
}

/// Status of every bot address of an open artifact, most-visited first.
///
/// Bots with a published list use it; crawlers with reverse-DNS suffixes use the cached
/// DNS verdict (never a fresh lookup); everything else is `unsupported`.
pub fn bot_statuses(con: &Connection, book: &RangeBook, now: f64) -> Result<Vec<BotStatus>> {
    let mut stmt = con.prepare(
        "SELECT b.bot,b.family,b.ip,b.visits,b.last_visit,c.payload,c.checked_at
         FROM bot_ips b LEFT JOIN dns_cache c ON b.ip=c.ip AND b.bot=c.bot
         ORDER BY b.visits DESC,b.bot,b.ip",
    )?;
    let mut out = Vec::new();
    let mut records = stmt.query([])?;
    while let Some(r) = records.next()? {
        let (bot, ip): (String, String) = (r.get(0)?, r.get(2)?);
        let status = if provider_for(&bot).is_some() {
            range_status(book, &bot, &ip)
        } else if crate::bots::suffixes_for(&bot).is_empty() {
            VerificationStatus::Unsupported
        } else {
            let payload: Option<String> = r.get(5)?;
            let stamp: Option<f64> = r.get(6)?;
            match cached_evidence(payload.as_deref(), stamp, now)["verdict"].as_str() {
                Some("verified") => VerificationStatus::Verified,
                Some("fake") => VerificationStatus::Spoofed,
                _ => VerificationStatus::Unverified,
            }
        };
        out.push(BotStatus {
            bot,
            family: r.get(1)?,
            ip,
            visits: r.get(3)?,
            last_visit: r.get(4)?,
            status,
        });
    }
    Ok(out)
}

/// Request for [`bot_status_page`].
#[derive(Debug, Clone)]
pub struct StatusQuery {
    /// Keep only rows with this status (`verified`, `spoofed`, ...); empty means all.
    pub status: String,
    pub limit: i64,
    pub offset: i64,
}

impl Default for StatusQuery {
    fn default() -> Self {
        Self {
            status: String::new(),
            limit: 100,
            offset: 0,
        }
    }
}

/// Bot IPs of an artifact with their verification status, filterable by status.
///
/// Bots with a published list use it; crawlers with reverse-DNS suffixes use the cached
/// DNS verdict (never a fresh lookup); everything else is `unsupported`. Only bot
/// addresses are rows: humans are never verified. `status_counts` always covers all rows,
/// so tiles stay stable while a filter is applied.
pub fn bot_status_page(source: &str, book: &RangeBook, q: &StatusQuery, now: f64) -> Result<Value> {
    paging(q.limit, q.offset)?;
    let wanted = if q.status.is_empty() {
        None
    } else {
        Some(VerificationStatus::from_name(&q.status).ok_or_else(|| {
            LogsError::value(
                "status must be verified, spoofed, unverified, unsupported, ptr_failed, network_error or resolver_unavailable",
            )
        })?)
    };
    let con = open_log_source(source)?;
    let mut rows: Vec<Value> = Vec::new();
    let mut counts: Map<String, Value> = VerificationStatus::ALL
        .iter()
        .map(|s| (s.name().to_string(), json!(0)))
        .collect();
    for hit in bot_statuses(&con, book, now)? {
        if let Some(slot) = counts.get_mut(hit.status.name()) {
            *slot = json!(slot.as_i64().unwrap_or(0) + 1);
        }
        if wanted.is_none_or(|w| w == hit.status) {
            rows.push(json!({
                "bot": hit.bot, "family": hit.family, "ip": hit.ip,
                "visits": hit.visits, "last_visit": hit.last_visit,
                "status": hit.status.name(), "status_code": hit.status.code(),
            }));
        }
    }
    let total = rows.len() as i64;
    let page: Vec<Value> = rows
        .into_iter()
        .skip(q.offset as usize)
        .take(q.limit as usize)
        .collect();
    let more = q.offset + (page.len() as i64) < total;
    Ok(json!({
        "ok": true, "rows": page, "total": total, "limit": q.limit, "offset": q.offset,
        "has_more": more, "next_offset": if more { json!(q.offset + q.limit) } else { Value::Null },
        "status_counts": counts,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPENAI: &str = r#"{"creationTime":"2026-10-01","prefixes":[{"ipv4Prefix":"20.171.206.0/24"},{"ipv6Prefix":"2001:db8::/32"}]}"#;

    #[test]
    fn provider_json_shapes_load_and_match() {
        let mut book = RangeBook::new();
        assert_eq!(book.load_json("gptbot", OPENAI).unwrap(), 2);
        assert_eq!(book.contains("gptbot", "20.171.206.9"), Some(true));
        assert_eq!(book.contains("gptbot", "2001:db8::7"), Some(true));
        assert_eq!(book.contains("gptbot", "8.8.8.8"), Some(false));
        assert_eq!(book.contains("claudebot", "8.8.8.8"), None);
        assert_eq!(
            book.load_json("claudebot", r#"["160.79.104.0/23"]"#)
                .unwrap(),
            1
        );
    }

    #[test]
    fn bad_lists_never_replace_a_good_one() {
        let mut book = RangeBook::new();
        book.load_json("gptbot", OPENAI).unwrap();
        for bad in ["not json", "{}", "[]", r#"[{"x":1}]"#, r#"["300.1.1.1/8"]"#] {
            assert!(book.load_json("gptbot", bad).is_err(), "{bad}");
        }
        assert_eq!(book.contains("gptbot", "20.171.206.9"), Some(true));
    }

    #[test]
    fn status_follows_list_presence() {
        let mut book = RangeBook::new();
        let bot = "GPTBot (OpenAI)";
        assert_eq!(
            range_status(&book, bot, "20.171.206.9"),
            VerificationStatus::Unverified
        );
        book.load_json("gptbot", OPENAI).unwrap();
        assert_eq!(
            range_status(&book, bot, "20.171.206.9"),
            VerificationStatus::Verified
        );
        assert_eq!(
            range_status(&book, bot, "8.8.8.8"),
            VerificationStatus::Spoofed
        );
        assert_eq!(
            range_status(&book, "Amazonbot", "8.8.8.8"),
            VerificationStatus::Unsupported
        );
    }

    #[test]
    fn status_names_round_trip() {
        for s in VerificationStatus::ALL {
            assert_eq!(VerificationStatus::from_name(s.name()), Some(s));
        }
        assert_eq!(VerificationStatus::from_name("nope"), None);
    }

    struct Canned;
    impl RangeFetcher for Canned {
        fn fetch(&self, provider: &str) -> std::result::Result<String, String> {
            if provider == "gptbot" {
                Ok(OPENAI.into())
            } else {
                Err("offline".into())
            }
        }
    }

    #[test]
    fn failed_refresh_keeps_previous_list() {
        let mut book = RangeBook::new();
        book.load_json("claudebot", r#"["160.79.104.0/23"]"#)
            .unwrap();
        let out = refresh(&mut book, &Canned, &["gptbot", "claudebot"]);
        assert!(out[0].1.is_ok());
        assert!(out[1].1.is_err());
        assert_eq!(book.contains("claudebot", "160.79.104.5"), Some(true));
    }
}
