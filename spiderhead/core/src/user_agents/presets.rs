// GENERATED from oracle-baseline the reference suite (b9847831). Do not hand-edit.

/// Product version embedded in the default identity (reference `spiderhead.__version__`).
pub const PRODUCT_VERSION: &str = "3.2.0";
/// Default crawler token (reference `DEFAULT_TOKEN`).
pub const DEFAULT_TOKEN: &str = "SpiderHeadBot";
/// Default crawler User-Agent (reference `DEFAULT_USER_AGENT`).
pub const DEFAULT_USER_AGENT: &str =
    "SpiderHeadBot/3.2.0 (+https://github.com/PavloSEO/spiderhead)";
/// Check downloader identity (reference `CHECKS_DOWNLOADER_USER_AGENT`).
pub const CHECKS_DOWNLOADER_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";
/// Check parser identity (reference `CHECKS_PARSER_USER_AGENT`).
pub const CHECKS_PARSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
/// Recon identity kept verbatim (reference `RECON_USER_AGENT`).
pub const RECON_USER_AGENT: &str =
    "Mozilla/5.0 (compatible; SEOHEAD-Tools/3.0; +https://seohead.tech/seotools)";
/// Presets as `(name, user agent, robots token)` in reference dict order.
pub const PRESETS: &[(&str, &str, &str)] = &[
    ("spiderhead", "SpiderHeadBot/3.2.0 (+https://github.com/PavloSEO/spiderhead)", "SpiderHeadBot"),
    ("googlebot", "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)", "Googlebot"),
    ("googlebot-desktop", "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; Googlebot/2.1; +http://www.google.com/bot.html) Chrome/W.X.Y.Z Safari/537.36", "Googlebot"),
    ("googlebot-smartphone", "Mozilla/5.0 (Linux; Android 6.0.1; Nexus 5X Build/MMB29P) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/W.X.Y.Z Mobile Safari/537.36 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)", "Googlebot"),
    ("bingbot", "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm) Chrome/W.X.Y.Z Safari/537.36", "bingbot"),
    ("yandexbot", "Mozilla/5.0 (compatible; YandexBot/3.0; +http://yandex.com/bots)", "YandexBot"),
    ("yandexmobilebot", "Mozilla/5.0 (iPhone; CPU iPhone OS 8_1 like Mac OS X) AppleWebKit/600.1.4 (KHTML, like Gecko) Version/8.0 Mobile/12B411 Safari/600.1.4 (compatible; YandexMobileBot/3.0; +http://yandex.com/bots)", "YandexMobileBot"),
    ("baiduspider", "Mozilla/5.0 (compatible; Baiduspider/2.0; +http://www.baidu.com/search/spider.html)", "Baiduspider"),
    ("duckduckbot", "DuckDuckBot/1.1; (+http://duckduckgo.com/duckduckbot.html)", "DuckDuckBot"),
    ("applebot", "Mozilla/5.0 (compatible; Applebot/0.1; +http://www.apple.com/go/applebot)", "Applebot"),
    ("gptbot", "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.2; +https://openai.com/gptbot)", "GPTBot"),
    ("claudebot", "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; ClaudeBot/1.0; +claudebot@anthropic.com)", "ClaudeBot"),
    ("perplexitybot", "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; PerplexityBot/1.0; +https://perplexity.ai/perplexitybot)", "PerplexityBot"),
    ("chrome-desktop", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36", "*"),
    ("firefox-desktop", "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:133.0) Gecko/20100101 Firefox/133.0", "*"),
    ("safari-desktop", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.6 Safari/605.1.15", "*"),
    ("edge-desktop", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0", "*"),
    ("chrome-mobile", "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36", "*"),
    ("safari-mobile", "Mozilla/5.0 (iPhone; CPU iPhone OS 17_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.6 Mobile/15E148 Safari/604.1", "*"),
];
