//! Crawler identification from user-agent strings and the reverse-DNS suffixes that
//! authenticate each crawler family.

use std::sync::OnceLock;

use regex::Regex;

/// Common Google crawlers: Googlebot and variants, GoogleOther, Google-Extended.
pub const GOOGLE_CRAWLERS: &[&str] = &[".googlebot.com"];
/// Special-case Google crawlers: AdsBot, Mediapartners, APIs-Google, InspectionTool.
pub const GOOGLE_SPECIAL: &[&str] = &[".google.com"];
/// User-triggered Google fetchers. Bare `.googleusercontent.com` is never official: its
/// `.bc.` subdomain is the reverse DNS of ordinary cloud VMs.
pub const GOOGLE_FETCHERS: &[&str] = &[".gae.googleusercontent.com"];
const BING: &[&str] = &[".search.msn.com"];
const YANDEX: &[&str] = &[".yandex.com", ".yandex.net", ".yandex.ru"];
const APPLE: &[&str] = &[".applebot.apple.com"];
const BAIDU: &[&str] = &[".baidu.com", ".baidu.jp"];
const NONE: &[&str] = &[];

/// One user-agent signature: pattern, display name, family, official PTR suffixes.
pub type BotPattern = (
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
);

/// Signatures in match order — the first hit wins, so specific names precede generic ones
/// (Googlebot-Image before Googlebot) and the catch-all comes last.
pub const BOT_PATTERNS: &[BotPattern] = &[
    (
        r"Googlebot[^)]*Mobile|Mobile[^)]*Googlebot",
        "Googlebot Smartphone",
        "googlebot",
        GOOGLE_CRAWLERS,
    ),
    (
        r"Googlebot-Image",
        "Googlebot Image",
        "googlebot",
        GOOGLE_CRAWLERS,
    ),
    (
        r"Googlebot-Video",
        "Googlebot Video",
        "googlebot",
        GOOGLE_CRAWLERS,
    ),
    (
        r"Googlebot-News",
        "Googlebot News",
        "googlebot",
        GOOGLE_CRAWLERS,
    ),
    (
        r"AdsBot-Google-Mobile",
        "AdsBot Google Mobile",
        "googlebot",
        GOOGLE_SPECIAL,
    ),
    (
        r"AdsBot-Google",
        "AdsBot Google",
        "googlebot",
        GOOGLE_SPECIAL,
    ),
    (
        r"Mediapartners-Google",
        "Mediapartners (AdSense)",
        "googlebot",
        GOOGLE_SPECIAL,
    ),
    (
        r"Google-InspectionTool",
        "Google InspectionTool",
        "googlebot",
        GOOGLE_SPECIAL,
    ),
    (
        r"Google-Extended",
        "Google-Extended (Gemini)",
        "ai",
        GOOGLE_CRAWLERS,
    ),
    (r"GoogleOther", "GoogleOther", "googlebot", GOOGLE_CRAWLERS),
    (r"APIs-Google", "APIs-Google", "googlebot", GOOGLE_SPECIAL),
    (
        r"FeedFetcher-Google",
        "FeedFetcher-Google",
        "googlebot",
        GOOGLE_FETCHERS,
    ),
    (r"Googlebot", "Googlebot", "googlebot", GOOGLE_CRAWLERS),
    (r"BingPreview", "BingPreview", "bingbot", BING),
    (r"adidxbot", "AdIdxBot (Bing Ads)", "bingbot", BING),
    (r"bingbot", "Bingbot", "bingbot", BING),
    (r"msnbot", "MSNBot", "bingbot", BING),
    (r"YandexMobileBot", "YandexMobileBot", "yandexbot", YANDEX),
    (r"YandexImages", "YandexImages", "yandexbot", YANDEX),
    (r"YandexBot", "YandexBot", "yandexbot", YANDEX),
    (
        r"Yandex(?:Accessibility|Metrika|Webmaster|[A-Za-z]+)?",
        "Yandex (other)",
        "yandexbot",
        YANDEX,
    ),
    (
        r"DuckDuckBot|DuckDuckGo-Favicons-Bot",
        "DuckDuckBot",
        "search",
        NONE,
    ),
    (r"Baiduspider", "Baiduspider", "search", BAIDU),
    (r"Sogou web spider", "Sogou", "search", NONE),
    (r"Exabot", "Exabot", "search", NONE),
    (r"Applebot", "Applebot", "search", APPLE),
    (r"SeznamBot", "SeznamBot", "search", NONE),
    (r"PetalBot", "PetalBot", "search", NONE),
    (r"GPTBot", "GPTBot (OpenAI)", "ai", NONE),
    (r"OAI-SearchBot", "OAI-SearchBot (OpenAI)", "ai", NONE),
    (r"ChatGPT-User", "ChatGPT-User", "ai", NONE),
    (
        r"ClaudeBot|Claude-Web|anthropic-ai",
        "ClaudeBot (Anthropic)",
        "ai",
        NONE,
    ),
    (r"PerplexityBot", "PerplexityBot", "ai", NONE),
    (r"CCBot", "CCBot (Common Crawl)", "ai", NONE),
    (r"Bytespider", "Bytespider (ByteDance)", "ai", NONE),
    (r"Amazonbot", "Amazonbot", "ai", NONE),
    (
        r"facebookexternalhit|facebookcatalog|meta-externalagent",
        "Facebook",
        "social",
        NONE,
    ),
    (r"Twitterbot", "Twitterbot", "social", NONE),
    (r"LinkedInBot", "LinkedInBot", "social", NONE),
    (r"Slackbot", "Slackbot", "social", NONE),
    (r"WhatsApp", "WhatsApp", "social", NONE),
    (r"TelegramBot", "TelegramBot", "social", NONE),
    (r"Discordbot", "Discordbot", "social", NONE),
    (r"Pinterest(?:bot)?", "Pinterestbot", "social", NONE),
    (r"AhrefsBot", "AhrefsBot", "seo-tool", NONE),
    (r"SemrushBot", "SemrushBot", "seo-tool", NONE),
    (r"MJ12bot", "MJ12bot (Majestic)", "seo-tool", NONE),
    (r"DotBot", "DotBot (Moz)", "seo-tool", NONE),
    (r"rogerbot", "rogerbot (Moz)", "seo-tool", NONE),
    (r"BLEXBot", "BLEXBot", "seo-tool", NONE),
    (r"DataForSeoBot", "DataForSeoBot", "seo-tool", NONE),
    (
        r"Screaming Frog SEO Spider",
        "Screaming Frog",
        "seo-tool",
        NONE,
    ),
    (
        r"\b(bot|crawler|spider|crawl)\b",
        "Other bot",
        "other",
        NONE,
    ),
];

/// A recognised crawler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bot {
    /// Display name, e.g. `Googlebot Image`.
    pub name: &'static str,
    /// Family used for grouping: googlebot, bingbot, yandexbot, search, ai, social, seo-tool, other.
    pub family: &'static str,
    /// Official reverse-DNS suffixes; empty when the vendor publishes none.
    pub rdns_suffixes: &'static [&'static str],
}

impl Bot {
    /// Whether the crawler can be authenticated by reverse DNS at all.
    pub fn verifiable(&self) -> bool {
        !self.rdns_suffixes.is_empty()
    }
}

fn compiled() -> &'static Vec<Regex> {
    static CELL: OnceLock<Vec<Regex>> = OnceLock::new();
    CELL.get_or_init(|| {
        BOT_PATTERNS
            .iter()
            .map(|(pattern, ..)| {
                Regex::new(&format!("(?i){pattern}")).expect("bot signatures are valid patterns")
            })
            .collect()
    })
}

/// Identify a known crawler; `None` means a human or an unrecognised user agent.
pub fn detect_bot(user_agent: Option<&str>) -> Option<Bot> {
    let agent = user_agent?;
    if agent.is_empty() || agent == "-" {
        return None;
    }
    compiled()
        .iter()
        .zip(BOT_PATTERNS)
        .find(|(rx, _)| rx.is_match(agent))
        .map(|(_, (_, name, family, suffixes))| Bot {
            name,
            family,
            rdns_suffixes: suffixes,
        })
}

/// Official reverse-DNS suffixes of the first signature carrying `name`.
pub fn suffixes_for(name: &str) -> &'static [&'static str] {
    BOT_PATTERNS
        .iter()
        .find(|(_, n, ..)| *n == name)
        .map_or(NONE, |(.., suffixes)| suffixes)
}
