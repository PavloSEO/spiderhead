"""Crawl User-Agent presets, custom strings and the robots.txt product token.

Every crawl request and every robots.txt group match reads the identity resolved
here, so a site owner who filters logs or robots rules sees the same name the
crawler sent. The default identifies this toolkit and links to its source; it
deliberately imitates no other crawler's string.
"""

from __future__ import annotations

import re
from typing import Any

from seohead import __version__

DEFAULT_PRESET = "spiderhead"
DEFAULT_TOKEN = "SpiderHeadBot"
DEFAULT_USER_AGENT = f"{DEFAULT_TOKEN}/{__version__} (+https://github.com/PavloSEO/spiderhead)"
MAX_LENGTH = 512

_GOOGLE = "+http://www.google.com/bot.html"
_BING = "+http://www.bing.com/bingbot.htm"
_BROWSER_DESKTOP_CHROME = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/131.0.0.0 Safari/537.36"
)

# name -> (user agent, robots.txt product token). Browser presets use "*" because a
# browser is not a robots group; they match only the wildcard rules.
PRESETS: dict[str, tuple[str, str]] = {
    "spiderhead": (DEFAULT_USER_AGENT, DEFAULT_TOKEN),
    "googlebot": (f"Mozilla/5.0 (compatible; Googlebot/2.1; {_GOOGLE})", "Googlebot"),
    "googlebot-desktop": (
        f"Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; Googlebot/2.1; "
        f"{_GOOGLE}) Chrome/W.X.Y.Z Safari/537.36",
        "Googlebot",
    ),
    "googlebot-smartphone": (
        "Mozilla/5.0 (Linux; Android 6.0.1; Nexus 5X Build/MMB29P) AppleWebKit/537.36 "
        f"(KHTML, like Gecko) Chrome/W.X.Y.Z Mobile Safari/537.36 (compatible; Googlebot/2.1; {_GOOGLE})",
        "Googlebot",
    ),
    "bingbot": (
        "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; bingbot/2.0; "
        f"{_BING}) Chrome/W.X.Y.Z Safari/537.36",
        "bingbot",
    ),
    "yandexbot": ("Mozilla/5.0 (compatible; YandexBot/3.0; +http://yandex.com/bots)", "YandexBot"),
    "yandexmobilebot": (
        "Mozilla/5.0 (iPhone; CPU iPhone OS 8_1 like Mac OS X) AppleWebKit/600.1.4 (KHTML, like "
        "Gecko) Version/8.0 Mobile/12B411 Safari/600.1.4 (compatible; YandexMobileBot/3.0; "
        "+http://yandex.com/bots)",
        "YandexMobileBot",
    ),
    "baiduspider": (
        "Mozilla/5.0 (compatible; Baiduspider/2.0; +http://www.baidu.com/search/spider.html)",
        "Baiduspider",
    ),
    "duckduckbot": ("DuckDuckBot/1.1; (+http://duckduckgo.com/duckduckbot.html)", "DuckDuckBot"),
    "applebot": (
        "Mozilla/5.0 (compatible; Applebot/0.1; +http://www.apple.com/go/applebot)",
        "Applebot",
    ),
    "gptbot": (
        "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.2; "
        "+https://openai.com/gptbot)",
        "GPTBot",
    ),
    "claudebot": (
        "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; ClaudeBot/1.0; "
        "+claudebot@anthropic.com)",
        "ClaudeBot",
    ),
    "perplexitybot": (
        "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; PerplexityBot/1.0; "
        "+https://perplexity.ai/perplexitybot)",
        "PerplexityBot",
    ),
    "chrome-desktop": (_BROWSER_DESKTOP_CHROME, "*"),
    "firefox-desktop": (
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:133.0) Gecko/20100101 Firefox/133.0",
        "*",
    ),
    "safari-desktop": (
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) "
        "Version/17.6 Safari/605.1.15",
        "*",
    ),
    "edge-desktop": (f"{_BROWSER_DESKTOP_CHROME} Edg/131.0.0.0", "*"),
    "chrome-mobile": (
        "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) "
        "Chrome/131.0.0.0 Mobile Safari/537.36",
        "*",
    ),
    "safari-mobile": (
        "Mozilla/5.0 (iPhone; CPU iPhone OS 17_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like "
        "Gecko) Version/17.6 Mobile/15E148 Safari/604.1",
        "*",
    ),
}

_PRINTABLE = re.compile(r"^[\x20-\x7e]+$")
_TOKEN = re.compile(r"^[A-Za-z0-9_.-]+")


def preset_names() -> list[str]:
    return sorted(PRESETS)


def resolve(value: str | None) -> tuple[str, str]:
    """Return ``(user_agent, robots_token)`` for a preset name, custom string or empty value.

    Names are matched case-insensitively. Anything that is not a preset must be one
    printable ASCII header line; its robots token is its leading product name, or
    ``*`` when that name cannot be read.
    """
    if value is None or value == "":
        return DEFAULT_USER_AGENT, DEFAULT_TOKEN
    if not isinstance(value, str):
        raise ValueError("user agent must be a preset name or a string")
    key = value.strip().lower()
    if key in PRESETS:
        return PRESETS[key]
    if len(value) > MAX_LENGTH or not _PRINTABLE.match(value) or value != value.strip():
        raise ValueError(
            "user agent must be a preset name or one printable ASCII header line "
            f"of at most {MAX_LENGTH} characters"
        )
    match = _TOKEN.match(value)
    return value, match.group(0) if match else "*"


def apply_defaults(http: dict[str, Any], robots: dict[str, Any]) -> None:
    """Fill the crawl settings' empty identity fields in place with the resolved default."""
    if not http.get("user_agent"):
        http["user_agent"] = DEFAULT_USER_AGENT
    if not robots.get("user_agent_token"):
        robots["user_agent_token"] = resolve(http["user_agent"])[1]
