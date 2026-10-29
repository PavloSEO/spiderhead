//! Split out of `cloudflare_traffic`.

/// `detect_bot`'s pattern table — literal (case-insensitive) substrings and the few
/// non-literal rules evaluated by [`matches_pattern`].
pub(super) struct BotPattern {
    pub(super) name: &'static str,
    pub(super) family: &'static str,
    pub(super) kind: Pattern,
}

pub(super) enum Pattern {
    /// A plain case-insensitive substring.
    Sub(&'static str),
    /// Any of several case-insensitive substrings.
    Any(&'static [&'static str]),
    /// `Googlebot[^)]*Mobile|Mobile[^)]*Googlebot`.
    GoogleMobile,
    /// `Yandex(?:Accessibility|Metrika|Webmaster|[A-Za-z]+)?` — any "yandex".
    YandexAny,
    /// `Pinterest(?:bot)?` — any "pinterest".
    PinterestAny,
    /// `\b(bot|crawler|spider|crawl)\b` — word-boundary keywords.
    BotWord,
}

/// The `BOT_PATTERNS` order matters: specific products precede the generic catch-all.
pub(super) const BOT_PATTERNS: &[BotPattern] = &[
    BotPattern {
        name: "Googlebot Smartphone",
        family: "googlebot",
        kind: Pattern::GoogleMobile,
    },
    BotPattern {
        name: "Googlebot Image",
        family: "googlebot",
        kind: Pattern::Sub("googlebot-image"),
    },
    BotPattern {
        name: "Googlebot Video",
        family: "googlebot",
        kind: Pattern::Sub("googlebot-video"),
    },
    BotPattern {
        name: "Googlebot News",
        family: "googlebot",
        kind: Pattern::Sub("googlebot-news"),
    },
    BotPattern {
        name: "AdsBot Google Mobile",
        family: "googlebot",
        kind: Pattern::Sub("adsbot-google-mobile"),
    },
    BotPattern {
        name: "AdsBot Google",
        family: "googlebot",
        kind: Pattern::Sub("adsbot-google"),
    },
    BotPattern {
        name: "Mediapartners (AdSense)",
        family: "googlebot",
        kind: Pattern::Sub("mediapartners-google"),
    },
    BotPattern {
        name: "Google InspectionTool",
        family: "googlebot",
        kind: Pattern::Sub("google-inspectiontool"),
    },
    BotPattern {
        name: "Google-Extended (Gemini)",
        family: "ai",
        kind: Pattern::Sub("google-extended"),
    },
    BotPattern {
        name: "GoogleOther",
        family: "googlebot",
        kind: Pattern::Sub("googleother"),
    },
    BotPattern {
        name: "APIs-Google",
        family: "googlebot",
        kind: Pattern::Sub("apis-google"),
    },
    BotPattern {
        name: "FeedFetcher-Google",
        family: "googlebot",
        kind: Pattern::Sub("feedfetcher-google"),
    },
    BotPattern {
        name: "Googlebot",
        family: "googlebot",
        kind: Pattern::Sub("googlebot"),
    },
    BotPattern {
        name: "BingPreview",
        family: "bingbot",
        kind: Pattern::Sub("bingpreview"),
    },
    BotPattern {
        name: "AdIdxBot (Bing Ads)",
        family: "bingbot",
        kind: Pattern::Sub("adidxbot"),
    },
    BotPattern {
        name: "Bingbot",
        family: "bingbot",
        kind: Pattern::Sub("bingbot"),
    },
    BotPattern {
        name: "MSNBot",
        family: "bingbot",
        kind: Pattern::Sub("msnbot"),
    },
    BotPattern {
        name: "YandexMobileBot",
        family: "yandexbot",
        kind: Pattern::Sub("yandexmobilebot"),
    },
    BotPattern {
        name: "YandexImages",
        family: "yandexbot",
        kind: Pattern::Sub("yandeximages"),
    },
    BotPattern {
        name: "YandexBot",
        family: "yandexbot",
        kind: Pattern::Sub("yandexbot"),
    },
    BotPattern {
        name: "Yandex (other)",
        family: "yandexbot",
        kind: Pattern::YandexAny,
    },
    BotPattern {
        name: "DuckDuckBot",
        family: "search",
        kind: Pattern::Any(&["duckduckbot", "duckduckgo-favicons-bot"]),
    },
    BotPattern {
        name: "Baiduspider",
        family: "search",
        kind: Pattern::Sub("baiduspider"),
    },
    BotPattern {
        name: "Sogou",
        family: "search",
        kind: Pattern::Sub("sogou web spider"),
    },
    BotPattern {
        name: "Exabot",
        family: "search",
        kind: Pattern::Sub("exabot"),
    },
    BotPattern {
        name: "Applebot",
        family: "search",
        kind: Pattern::Sub("applebot"),
    },
    BotPattern {
        name: "SeznamBot",
        family: "search",
        kind: Pattern::Sub("seznambot"),
    },
    BotPattern {
        name: "PetalBot",
        family: "search",
        kind: Pattern::Sub("petalbot"),
    },
    BotPattern {
        name: "GPTBot (OpenAI)",
        family: "ai",
        kind: Pattern::Sub("gptbot"),
    },
    BotPattern {
        name: "OAI-SearchBot (OpenAI)",
        family: "ai",
        kind: Pattern::Sub("oai-searchbot"),
    },
    BotPattern {
        name: "ChatGPT-User",
        family: "ai",
        kind: Pattern::Sub("chatgpt-user"),
    },
    BotPattern {
        name: "ClaudeBot (Anthropic)",
        family: "ai",
        kind: Pattern::Any(&["claudebot", "claude-web", "anthropic-ai"]),
    },
    BotPattern {
        name: "PerplexityBot",
        family: "ai",
        kind: Pattern::Sub("perplexitybot"),
    },
    BotPattern {
        name: "CCBot (Common Crawl)",
        family: "ai",
        kind: Pattern::Sub("ccbot"),
    },
    BotPattern {
        name: "Bytespider (ByteDance)",
        family: "ai",
        kind: Pattern::Sub("bytespider"),
    },
    BotPattern {
        name: "Amazonbot",
        family: "ai",
        kind: Pattern::Sub("amazonbot"),
    },
    BotPattern {
        name: "Facebook",
        family: "social",
        kind: Pattern::Any(&[
            "facebookexternalhit",
            "facebookcatalog",
            "meta-externalagent",
        ]),
    },
    BotPattern {
        name: "Twitterbot",
        family: "social",
        kind: Pattern::Sub("twitterbot"),
    },
    BotPattern {
        name: "LinkedInBot",
        family: "social",
        kind: Pattern::Sub("linkedinbot"),
    },
    BotPattern {
        name: "Slackbot",
        family: "social",
        kind: Pattern::Sub("slackbot"),
    },
    BotPattern {
        name: "WhatsApp",
        family: "social",
        kind: Pattern::Sub("whatsapp"),
    },
    BotPattern {
        name: "TelegramBot",
        family: "social",
        kind: Pattern::Sub("telegrambot"),
    },
    BotPattern {
        name: "Discordbot",
        family: "social",
        kind: Pattern::Sub("discordbot"),
    },
    BotPattern {
        name: "Pinterestbot",
        family: "social",
        kind: Pattern::PinterestAny,
    },
    BotPattern {
        name: "AhrefsBot",
        family: "seo-tool",
        kind: Pattern::Sub("ahrefsbot"),
    },
    BotPattern {
        name: "SemrushBot",
        family: "seo-tool",
        kind: Pattern::Sub("semrushbot"),
    },
    BotPattern {
        name: "MJ12bot (Majestic)",
        family: "seo-tool",
        kind: Pattern::Sub("mj12bot"),
    },
    BotPattern {
        name: "DotBot (Moz)",
        family: "seo-tool",
        kind: Pattern::Sub("dotbot"),
    },
    BotPattern {
        name: "rogerbot (Moz)",
        family: "seo-tool",
        kind: Pattern::Sub("rogerbot"),
    },
    BotPattern {
        name: "BLEXBot",
        family: "seo-tool",
        kind: Pattern::Sub("blexbot"),
    },
    BotPattern {
        name: "DataForSeoBot",
        family: "seo-tool",
        kind: Pattern::Sub("dataforseobot"),
    },
    BotPattern {
        name: "Screaming Frog",
        family: "seo-tool",
        kind: Pattern::Sub("screaming frog seo spider"),
    },
    BotPattern {
        name: "Other bot",
        family: "other",
        kind: Pattern::BotWord,
    },
];
