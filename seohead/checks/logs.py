"""Analyze web-server logs to show how crawlers actually use a site.

Logs expose search-engine behavior rather than assumptions about it. A crawl shows
what can be discovered; access logs show what was requested, how often, which
responses were served, and where crawl budget was spent.

The parser supports Apache/Nginx Common and Combined formats plus IIS W3C Extended.
It detects the format from sample lines. IIS column positions come from each
``#Fields:`` directive rather than a hard-coded order because administrators can
rearrange or change fields during a file.

Files are processed line by line so a multi-gigabyte log does not require matching
memory. Gzip-compressed logs are detected by content and read the same way. Path accumulators are capped; exact IP aggregates use a temporary disk-backed SQLite table.

Bot verification
----------------
A user-agent is trivial to spoof, so a ``Googlebot`` string alone proves nothing.
Authenticity requires forward-confirmed reverse DNS: the client's PTR hostname
must end in an official suffix, and resolving that hostname forward must return
the original IP. An attacker can forge a user-agent but not another provider's
authoritative DNS records.

Official suffixes follow each provider's published rules, per crawler family.
For Google: common crawlers (Googlebot and variants, GoogleOther, Google-Extended)
verify against ``*.googlebot.com``; special-case crawlers (AdsBot, Mediapartners,
APIs-Google, Google-InspectionTool) against ``*.google.com``; user-triggered
fetchers against ``*.gae.googleusercontent.com`` only. Bare
``*.googleusercontent.com`` is never official — ``*.bc.googleusercontent.com`` is
the reverse DNS of ordinary Google Cloud VMs, not of any Google crawler.

DNS verification performs network requests, so it is disabled by default and must
be enabled explicitly with ``verify_bots=True``.

One failure mode needs special handling. If outbound DNS is blocked by the audited
environment, every lookup could appear unverified and falsely imply that all bots
are impersonators. A canary PTR lookup first confirms that reverse DNS is available.
If the canary fails, verification is reported as unavailable rather than producing
an authenticity verdict.
"""

from __future__ import annotations

import gzip
import ipaddress
import itertools
import re
import socket
import sqlite3
import threading
import time
from collections import Counter, OrderedDict, defaultdict
from collections.abc import Iterable
from contextlib import closing
from datetime import datetime, timedelta, timezone
from queue import Empty, Queue
from typing import Any

# Official reverse-DNS suffixes, one table per Google crawler family (#1532). A
# non-empty tuple means the bot is verifiable. Bare ``.googleusercontent.com`` is
# deliberately absent: ``*.bc.googleusercontent.com`` is the reverse DNS of ordinary
# Google Cloud VMs, which no official Google crawler uses.
GOOGLE_CRAWLERS = (
    ".googlebot.com",  # common crawlers: Googlebot and variants, GoogleOther, Google-Extended
)
GOOGLE_SPECIAL = (
    ".google.com",  # special-case crawlers: AdsBot, Mediapartners, APIs-Google, Google-InspectionTool
)
GOOGLE_FETCHERS = (".gae.googleusercontent.com",)  # user-triggered fetchers only
BING = (".search.msn.com",)
YANDEX = (".yandex.com", ".yandex.net", ".yandex.ru")
APPLE = (".applebot.apple.com",)
BAIDU = (".baidu.com", ".baidu.jp")

# Order matters because the first match wins. Specific signatures such as
# Googlebot-Image precede generic Googlebot, and the catch-all remains last.
BOT_PATTERNS: tuple[tuple[str, str, str, tuple[str, ...]], ...] = (
    (
        r"Googlebot[^)]*Mobile|Mobile[^)]*Googlebot",
        "Googlebot Smartphone",
        "googlebot",
        GOOGLE_CRAWLERS,
    ),
    (r"Googlebot-Image", "Googlebot Image", "googlebot", GOOGLE_CRAWLERS),
    (r"Googlebot-Video", "Googlebot Video", "googlebot", GOOGLE_CRAWLERS),
    (r"Googlebot-News", "Googlebot News", "googlebot", GOOGLE_CRAWLERS),
    (r"AdsBot-Google-Mobile", "AdsBot Google Mobile", "googlebot", GOOGLE_SPECIAL),
    (r"AdsBot-Google", "AdsBot Google", "googlebot", GOOGLE_SPECIAL),
    (r"Mediapartners-Google", "Mediapartners (AdSense)", "googlebot", GOOGLE_SPECIAL),
    (r"Google-InspectionTool", "Google InspectionTool", "googlebot", GOOGLE_SPECIAL),
    (r"Google-Extended", "Google-Extended (Gemini)", "ai", GOOGLE_CRAWLERS),
    (r"GoogleOther", "GoogleOther", "googlebot", GOOGLE_CRAWLERS),
    (r"APIs-Google", "APIs-Google", "googlebot", GOOGLE_SPECIAL),
    (r"FeedFetcher-Google", "FeedFetcher-Google", "googlebot", GOOGLE_FETCHERS),
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
    (r"DuckDuckBot|DuckDuckGo-Favicons-Bot", "DuckDuckBot", "search", ()),
    (r"Baiduspider", "Baiduspider", "search", BAIDU),
    (r"Sogou web spider", "Sogou", "search", ()),
    (r"Exabot", "Exabot", "search", ()),
    (r"Applebot", "Applebot", "search", APPLE),
    (r"SeznamBot", "SeznamBot", "search", ()),
    (r"PetalBot", "PetalBot", "search", ()),
    (r"GPTBot", "GPTBot (OpenAI)", "ai", ()),
    (r"OAI-SearchBot", "OAI-SearchBot (OpenAI)", "ai", ()),
    (r"ChatGPT-User", "ChatGPT-User", "ai", ()),
    (r"ClaudeBot|Claude-Web|anthropic-ai", "ClaudeBot (Anthropic)", "ai", ()),
    (r"PerplexityBot", "PerplexityBot", "ai", ()),
    (r"CCBot", "CCBot (Common Crawl)", "ai", ()),
    (r"Bytespider", "Bytespider (ByteDance)", "ai", ()),
    (r"Amazonbot", "Amazonbot", "ai", ()),
    (r"facebookexternalhit|facebookcatalog|meta-externalagent", "Facebook", "social", ()),
    (r"Twitterbot", "Twitterbot", "social", ()),
    (r"LinkedInBot", "LinkedInBot", "social", ()),
    (r"Slackbot", "Slackbot", "social", ()),
    (r"WhatsApp", "WhatsApp", "social", ()),
    (r"TelegramBot", "TelegramBot", "social", ()),
    (r"Discordbot", "Discordbot", "social", ()),
    (r"Pinterest(?:bot)?", "Pinterestbot", "social", ()),
    (r"AhrefsBot", "AhrefsBot", "seo-tool", ()),
    (r"SemrushBot", "SemrushBot", "seo-tool", ()),
    (r"MJ12bot", "MJ12bot (Majestic)", "seo-tool", ()),
    (r"DotBot", "DotBot (Moz)", "seo-tool", ()),
    (r"rogerbot", "rogerbot (Moz)", "seo-tool", ()),
    (r"BLEXBot", "BLEXBot", "seo-tool", ()),
    (r"DataForSeoBot", "DataForSeoBot", "seo-tool", ()),
    (r"Screaming Frog SEO Spider", "Screaming Frog", "seo-tool", ()),
    (r"\b(bot|crawler|spider|crawl)\b", "Other bot", "other", ()),
)

_COMPILED = tuple(
    (re.compile(p, re.IGNORECASE), name, family, sfx) for p, name, family, sfx in BOT_PATTERNS
)

# Combined is a superset of Common and must be checked first. One or two ident/user tokens
# sit between the client IP and the timestamp (`IP - - [ts]` and the one-dash custom layout);
# an optional time-taken token may follow the response size or the whole line.
_COMBINED_RE = re.compile(
    r'^(\S+)(?:\s+\S+){1,2}\s+\[([^\]]+)\]\s+"([A-Z!]+)\s+(\S+)(?:\s+[^"]*)?"'
    r'\s+(\d{3})\s+(\S+)(?:\s+([\d.,]+))?\s+"([^"]*)"\s+"([^"]*)"(?:\s+([\d.,]+))?\s*$'
)
_COMMON_RE = re.compile(
    r'^(\S+)(?:\s+\S+){1,2}\s+\[([^\]]+)\]\s+"([A-Z!]+)\s+(\S+)(?:\s+[^"]*)?"'
    r"\s+(\d{3})\s+(\S+)(?:\s+([\d.,]+))?\s*$"
)
# How many ident/user tokens a line carries between the IP and the timestamp (#1535).
_IDENTS_RE = re.compile(r"^\S+(\s+\S+)\s+\[")
_TS_RE = re.compile(r"^(\d{2})/([A-Za-z]{3})/(\d{4}):(\d{2}):(\d{2}):(\d{2})\s*([+-]\d{4})?$")
_MONTHS = {
    m: i
    for i, m in enumerate(
        ("Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"), 1
    )
}

MAX_TRACKED_PATHS = 5000
DNS_CACHE_TTL = 3600
_DNS_CACHE: OrderedDict = OrderedDict()
_DNS_LOCK = threading.Lock()
_DNS_WORKERS = threading.BoundedSemaphore(4)
RDNS_TIMEOUT = 3.0
# DNS canary used to confirm PTR lookups work at all; Google's public resolver has
# a stable reverse record.
DNS_CANARY = "8.8.8.8"


def detect_bot(user_agent: str | None) -> dict[str, Any] | None:
    """Identify a known bot; ``None`` means human or unrecognized user-agent."""
    if not user_agent or user_agent == "-":
        return None
    for rx, name, family, suffixes in _COMPILED:
        if rx.search(user_agent):
            return {
                "name": name,
                "family": family,
                "rdns_suffixes": list(suffixes),
                "verifiable": bool(suffixes),
            }
    return None


def parse_apache_timestamp(raw: str) -> datetime | None:
    """Convert an Apache timestamp such as ``10/Oct/2000:13:55:36 -0700`` to UTC.

    When the log omits a time zone, the value is assumed to be UTC. This is an
    explicit assumption, not a measured fact; a server in another zone introduces
    a systematic offset.
    """
    m = _TS_RE.match(raw.strip())
    if not m:
        return None
    day, mon, year, hh, mm, ss, tz = m.groups()
    month = _MONTHS.get(mon)
    if month is None:
        return None
    try:
        naive = datetime(int(year), month, int(day), int(hh), int(mm), int(ss), tzinfo=timezone.utc)
    except ValueError:
        return None
    if tz:
        sign = -1 if tz[0] == "-" else 1
        offset = timedelta(minutes=int(tz[1:3]) * 60 + int(tz[3:5]))
        naive -= sign * offset
    return naive


def _parse_apache(line: str) -> dict[str, Any] | None:
    m = _COMBINED_RE.match(line)
    referer = agent = between_time = trailing_time = None
    if m:
        ip, ts, method, path, status, size, between_time, referer, agent, trailing_time = m.groups()
    else:
        m = _COMMON_RE.match(line)
        if not m:
            return None
        ip, ts, method, path, status, size, trailing_time = m.groups()
    try:
        bytes_sent = int(size)
    except ValueError:
        bytes_sent = 0  # ``-`` means no response body size was recorded.
    time_taken = next(
        (float(value.replace(",", ".")) for value in (between_time, trailing_time) if value), None
    )
    return {
        "ip": ip,
        "time": parse_apache_timestamp(ts),
        "method": method,
        "path": path,
        "status": int(status),
        "bytes": bytes_sent,
        "time_taken": time_taken,
        "referer": None if referer in (None, "", "-") else referer,
        "user_agent": None if agent in (None, "", "-") else agent,
    }


class _IISParser:
    """Read field positions from ``#Fields:``, which may change within a log."""

    WANTED = (
        "date",
        "time",
        "cs-uri-stem",
        "cs-uri-query",
        "cs-method",
        "c-ip",
        "cs(User-Agent)",
        "cs(Referer)",
        "sc-status",
        "sc-bytes",
    )

    def __init__(self) -> None:
        self.fields: list[str] = []

    def parse(self, line: str) -> dict[str, Any] | None:
        if line.startswith("#"):
            if line.lower().startswith("#fields:"):
                self.fields = line.split(":", 1)[1].split()
            return None
        if not self.fields:
            return None
        parts = line.split()
        got: dict[str, str] = {}
        for name in self.WANTED:
            if name in self.fields:
                idx = self.fields.index(name)
                if idx < len(parts):
                    got[name] = parts[idx]
        path = got.get("cs-uri-stem")
        if not path:
            return None
        query = got.get("cs-uri-query")
        if query and query != "-":
            path = f"{path}?{query}"
        when = None
        if got.get("date") and got.get("time"):
            try:
                when = datetime.strptime(
                    f"{got['date']} {got['time']}", "%Y-%m-%d %H:%M:%S"
                ).replace(tzinfo=timezone.utc)
            except ValueError:
                when = None
        try:
            status = int(got.get("sc-status", "0"))
        except ValueError:
            status = 0
        try:
            bytes_sent = int(got.get("sc-bytes", "0"))
        except ValueError:
            bytes_sent = 0
        # IIS encodes spaces in user-agent values as plus signs.
        agent = (got.get("cs(User-Agent)") or "").replace("+", " ").strip()
        referer = got.get("cs(Referer)")
        return {
            "ip": got.get("c-ip", ""),
            "time": when,
            "method": got.get("cs-method", ""),
            "path": path,
            "status": status,
            "bytes": bytes_sent,
            "referer": None if referer in (None, "", "-") else referer,
            "user_agent": agent or None,
        }


def detect_format(sample: Iterable[str]) -> str | None:
    """Detect ``combined``, ``common``, or ``iis`` format from sample lines."""
    combined = common = 0
    for line in sample:
        line = line.strip()
        if not line:
            continue
        if line.startswith("#") and "fields:" in line.lower():
            return "iis"
        if _COMBINED_RE.match(line):
            combined += 1
        elif _COMMON_RE.match(line):
            common += 1
    if combined:
        return "combined"
    if common:
        return "common"
    return None


def verify_bot_rdns(
    ip: str, suffixes: Iterable[str], timeout: float = RDNS_TIMEOUT
) -> dict[str, Any]:
    """Verify a bot with forward-confirmed reverse DNS.

    The PTR hostname must end in an official suffix and resolve forward to the
    original IP. Returns ``{verified, reason, hostname}``, distinguishing a failed
    authenticity check from an unavailable DNS check; collapsing both to ``False``
    would misrepresent the evidence.
    """
    suffixes = tuple(suffixes)
    if not suffixes:
        return {
            "verified": None,
            "reason": "No official PTR suffix is available for this bot",
            "hostname": None,
        }
    try:
        address = ipaddress.ip_address(ip)
    except ValueError:
        return {"verified": None, "hostname": None, "reason": "Invalid IP address"}
    result_queue: Queue = Queue(maxsize=1)
    worker_gate = _DNS_WORKERS
    if not worker_gate.acquire(blocking=False):
        return {
            "verified": None,
            "hostname": None,
            "forward_addresses": [],
            "forward_match": None,
            "reason": "DNS resolver worker capacity is exhausted",
        }

    def lookup():
        result = {
            "verified": None,
            "hostname": None,
            "forward_addresses": [],
            "forward_match": None,
        }
        try:
            hostname, _, _ = socket.gethostbyaddr(str(address))
            hostname = hostname.lower().rstrip(".")
            result["hostname"] = hostname
            if not any(hostname.endswith(s.lower()) for s in suffixes):
                if hostname.endswith(".bc.googleusercontent.com"):
                    # A Google Cloud VM's PTR (#1532): the suffix names a host any
                    # GCP tenant can run, so this is an impersonator, not an outage.
                    result.update(
                        verified=False,
                        reason="PTR hostname is a Google Cloud VM "
                        "(bc.googleusercontent.com), which no official Google crawler uses",
                    )
                else:
                    result.update(
                        verified=False, reason="PTR hostname is outside the official domains"
                    )
            else:
                if address.version == 4:
                    _, _, addrs = socket.gethostbyname_ex(hostname)
                else:
                    addrs = [
                        row[4][0] for row in socket.getaddrinfo(hostname, None, socket.AF_INET6)
                    ]
                addrs = sorted({str(ipaddress.ip_address(value)) for value in addrs})
                match = str(address) in addrs
                result.update(
                    verified=match,
                    forward_match=match,
                    forward_addresses=addrs,
                    reason="PTR and forward resolution confirm the same IP"
                    if match
                    else f"PTR hostname {hostname} does not resolve back to {ip}",
                )
        except socket.herror as exc:
            # Only HOST_NOT_FOUND / NO_DATA prove an absent record. TRY_AGAIN is unavailable.
            result.update(
                verified=False if exc.errno in (1, 4) else None,
                reason="No PTR record was found" if exc.errno in (1, 4) else "DNS is unavailable",
            )
        except (OSError, ValueError) as exc:
            result["reason"] = (
                f"Forward DNS is unavailable: {exc}"
                if result["hostname"]
                else f"DNS is unavailable: {exc}"
            )
        finally:
            worker_gate.release()
        result_queue.put(result)

    # System resolver calls can ignore socket timeouts. A daemon worker gives the caller a
    # hard deadline without changing process-wide socket defaults or waiting at shutdown.
    try:
        threading.Thread(target=lookup, daemon=True).start()
    except RuntimeError:
        worker_gate.release()
        return {
            "verified": None,
            "hostname": None,
            "forward_addresses": [],
            "forward_match": None,
            "reason": "DNS resolver worker is unavailable",
        }
    try:
        return result_queue.get(timeout=timeout)
    except Empty:
        return {
            "verified": None,
            "hostname": None,
            "forward_addresses": [],
            "forward_match": None,
            "reason": "DNS verification deadline exceeded",
        }


def _dns_available(timeout: float = RDNS_TIMEOUT) -> bool:
    """Return whether reverse DNS works from this environment.

    Without the canary, blocked DNS could make every bot appear fake, which is the
    most damaging false conclusion this analyzer could report.
    """
    result = verify_bot_rdns(DNS_CANARY, (".google",), timeout=timeout)
    return result.get("hostname") is not None


def _section(path: str) -> str:
    """Return the first path segment as a site section; ``/`` is the root."""
    cut = path.split("?", 1)[0].strip("/")
    if not cut:
        return "/"
    return "/" + cut.split("/", 1)[0]


def analyze_log(
    path: str,
    *,
    verify_bots: bool = False,
    max_lines: int = 5_000_000,
    sample_size: int = 50,
    offset: int = 0,
    limit: int = 100,
    max_dns: int = 20,
    bot: str | None = None,
    ip: str | None = None,
) -> dict[str, Any]:
    """Parse a log and calculate aggregates; only bot verification uses the network."""
    for name, value, minimum, maximum in (
        ("max_lines", max_lines, 1, 100_000_000),
        ("sample_size", sample_size, 1, 1000),
        ("offset", offset, 0, 100_000_000),
        ("limit", limit, 1, 500),
        ("max_dns", max_dns, 0, 100),
    ):
        if type(value) is not int or not minimum <= value <= maximum:
            raise ValueError(f"{name} must be an integer in {minimum}..{maximum}")
    if type(verify_bots) is not bool:
        raise ValueError("verify_bots must be a boolean")
    if ip:
        ipaddress.ip_network(ip, strict=False)
    try:
        handle = _open_log(path)
    except OSError as exc:
        return {"ok": False, "path": path, "error": str(exc)}

    try:
        with closing(sqlite3.connect("")) as ips:
            ips.execute(
                "CREATE TABLE ips (bot TEXT, family TEXT, ip TEXT, visits INTEGER, last_visit TEXT, PRIMARY KEY(bot, ip))"
            )
            return _analyze_handle(
                handle,
                path,
                verify_bots=verify_bots,
                max_lines=max_lines,
                sample_size=sample_size,
                ips=ips,
                offset=offset,
                limit=limit,
                max_dns=max_dns,
                bot_filter=bot,
                ip_filter=ip,
            )
    except (OSError, EOFError, sqlite3.Error) as exc:
        # A truncated or corrupt .gz raises mid-stream. Report it as data: partial counts
        # from a broken archive must not be presented as a complete analysis.
        return {"ok": False, "path": path, "error": f"Cannot read log: {exc}"}
    finally:
        handle.close()


_GZIP_MAGIC = b"\x1f\x8b"


def _open_log(path: str):
    """Open a log as text, transparently decompressing gzip by content, not by file name.

    Rotated logs (``access.log.1.gz``) and renamed archives both carry the gzip magic
    bytes, so detection reads the header rather than trusting the extension.
    """
    with open(path, "rb") as probe:
        magic = probe.read(2)
    if magic == _GZIP_MAGIC:
        return gzip.open(path, "rt", encoding="utf-8", errors="replace")
    return open(path, encoding="utf-8", errors="replace")


def _analyze_handle(
    handle,
    path: str,
    *,
    verify_bots: bool,
    max_lines: int,
    sample_size: int,
    ips,
    offset: int,
    limit: int,
    max_dns: int,
    bot_filter: str | None,
    ip_filter: str | None,
) -> dict[str, Any]:
    with handle:
        sample = []
        for _ in range(sample_size):
            line = handle.readline()
            if not line:
                break
            sample.append(line)
        fmt = detect_format(sample)
        if fmt is None:
            return {
                "ok": False,
                "path": path,
                "error": "Unrecognized log format: expected Apache Common/Combined or IIS W3C",
            }

        iis = _IISParser() if fmt == "iis" else None
        parse = iis.parse if iis else _parse_apache

        total = parsed = skipped = unknown_agent = 0
        truncated = False
        by_family: dict[str, Counter] = defaultdict(Counter)
        bot_hits: Counter = Counter()
        bot_bytes: Counter = Counter()
        status_by_family: dict[str, Counter] = defaultdict(Counter)
        section_by_family: dict[str, Counter] = defaultdict(Counter)
        paths_by_family: dict[str, Counter] = defaultdict(Counter)
        paths_truncated: set[str] = set()
        daily: Counter = Counter()
        first_time = last_time = None

        # chain(), not [*sample, *handle]: the list literal forces the whole remaining
        # file into memory before the first row is even parsed, which is exactly the
        # eager read max_lines exists to prevent (#252). Stop pulling from the iterator
        # entirely once the cap is hit, instead of continuing to "skip" every line the
        # file still has -- that no longer bounds reads, only the skip counter.
        for line in itertools.chain(sample, handle):
            line = line.rstrip("\n")
            if not line.strip():
                continue
            if total >= max_lines:
                truncated = True
                break
            total += 1
            row = parse(line)
            if row is None:
                skipped += 1
                continue
            parsed += 1

            if not row.get("user_agent"):
                unknown_agent += 1
            bot = detect_bot(row.get("user_agent"))
            fallback = "human" if row.get("user_agent") else "unknown"
            family = bot["family"] if bot else fallback
            name = bot["name"] if bot else fallback

            by_family[family][name] += 1
            status_by_family[family][row["status"]] += 1
            section_by_family[family][_section(row["path"])] += 1
            # A full counter must keep incrementing paths it already tracks -- only a
            # brand-new path should be refused once the cap is reached. Capping every
            # increment here (#251) let a path counted before the cap filled silently
            # undercount every repeat after: the family total stayed complete while its
            # own top-paths breakdown quietly fell out of sync with it.
            family_paths = paths_by_family[family]
            path_value = row["path"]
            if path_value in family_paths or len(family_paths) < MAX_TRACKED_PATHS:
                family_paths[path_value] += 1
            else:
                paths_truncated.add(family)
            if bot:
                bot_hits[name] += 1
                bot_bytes[name] += row.get("bytes") or 0
                when = row.get("time")
                ips.execute(
                    "INSERT INTO ips VALUES (?, ?, ?, 1, ?) ON CONFLICT(bot, ip) DO UPDATE SET "
                    "visits=visits+1, last_visit=CASE WHEN excluded.last_visit > last_visit OR last_visit IS NULL "
                    "THEN excluded.last_visit ELSE last_visit END",
                    (name, family, row["ip"], when.isoformat() if when else None),
                )
            when = row.get("time")
            if when:
                daily[when.date().isoformat()] += 1
                first_time = when if first_time is None or when < first_time else first_time
                last_time = when if last_time is None or when > last_time else last_time

    counts = dict(ips.execute("SELECT bot, count(*) FROM ips GROUP BY bot"))
    total_ips = ips.execute("SELECT count(DISTINCT ip) FROM ips").fetchone()[0]
    selected = []
    matched = 0
    network = ipaddress.ip_network(ip_filter, strict=False) if ip_filter else None
    for name, family, address, visits, last_visit in ips.execute(
        "SELECT * FROM ips ORDER BY bot, ip"
    ):
        if bot_filter and bot_filter not in (name, family):
            continue
        if network and (not _is_ip(address) or ipaddress.ip_address(address) not in network):
            continue
        if offset <= matched < offset + limit:
            selected.append(
                {
                    "bot": name,
                    "family": family,
                    "ip": address,
                    "visits": visits,
                    "last_visit": last_visit,
                }
            )
        matched += 1
    verification = _verify_rows(selected, verify_bots, max_dns)

    result: dict[str, Any] = {
        "ok": True,
        "path": path,
        "format": fmt,
        "lines": {"total": total, "parsed": parsed, "skipped": skipped, "truncated": truncated},
        "bot_detection": {
            "available": unknown_agent < parsed,
            "unknown_user_agent": unknown_agent,
            "reason": "User-agent fields are missing" if unknown_agent else None,
        },
        "period": {
            "from": first_time.isoformat() if first_time else None,
            "to": last_time.isoformat() if last_time else None,
        },
        "by_family": {f: dict(c.most_common(20)) for f, c in by_family.items()},
        "bots": [
            {
                "name": n,
                "hits": h,
                "bytes": bot_bytes[n],
                "unique_ips": counts[n],
                "unique_ips_truncated": False,
            }
            for n, h in bot_hits.most_common(40)
        ],
        "status_by_family": {f: dict(sorted(c.items())) for f, c in status_by_family.items()},
        "sections_by_family": {f: dict(c.most_common(20)) for f, c in section_by_family.items()},
        "top_paths_by_family": {f: dict(c.most_common(15)) for f, c in paths_by_family.items()},
        "paths_truncated": sorted(paths_truncated),
        "ips_truncated": [],
        "unique_bot_ips": total_ips,
        "bot_ips": {
            "rows": verification["checks"],
            "total": matched,
            "offset": offset,
            "limit": limit,
            "has_more": matched > offset + len(selected),
            "next_offset": offset + len(selected) if matched > offset + len(selected) else None,
        },
        "daily": dict(sorted(daily.items())),
        "verification": verification,
    }
    result["findings"] = _findings(result)
    return result


def _cached_verification(ip: str, suffixes: Iterable[str]) -> dict[str, Any]:
    key = (ip, tuple(suffixes))
    now = time.monotonic()
    with _DNS_LOCK:
        entry = _DNS_CACHE.get(key)
        if entry and now - entry[0] < DNS_CACHE_TTL:
            return {**entry[1], "cached": True}
    result = {**verify_bot_rdns(ip, key[1]), "checked_at": time.time()}
    with _DNS_LOCK:
        _DNS_CACHE[key] = (now, result)
        _DNS_CACHE.move_to_end(key)
        while len(_DNS_CACHE) > 1024:
            _DNS_CACHE.popitem(last=False)
    return {**result, "cached": False}


def _verify_rows(rows: list[dict[str, Any]], enabled: bool, max_dns: int = 20) -> dict[str, Any]:
    """Verify only the explicit bounded page; never extrapolate one IP to a family."""
    checks = []
    requests = 0

    def cached(row):
        suffixes = next((s for _, n, _, s in BOT_PATTERNS if n == row["bot"]), ())
        with _DNS_LOCK:
            entry = _DNS_CACHE.get((row["ip"], tuple(suffixes)))
            return bool(entry and time.monotonic() - entry[0] < DNS_CACHE_TTL)

    eligible = any(
        next((s for _, n, _, s in BOT_PATTERNS if n == row["bot"]), ())
        and _is_ip(row["ip"])
        and not cached(row)
        for row in rows
    )
    available = _dns_available() if enabled and max_dns and eligible else None
    for row in rows:
        suffixes = next((s for _, n, _, s in BOT_PATTERNS if n == row["bot"]), ())
        result = {
            "verified": None,
            "hostname": None,
            "forward_addresses": [],
            "forward_match": None,
            "cached": False,
            "reason": "DNS verification was not requested",
        }
        if enabled and not suffixes:
            result["reason"] = "No official PTR suffix is available for this bot"
        elif enabled and not _is_ip(row["ip"]):
            result["reason"] = "Invalid IP address"
        elif enabled and cached(row):
            result = _cached_verification(row["ip"], suffixes)
        elif enabled and available is False:
            result["reason"] = "Reverse DNS is unavailable; this does not mean the bot is fake"
        elif enabled and requests < max_dns:
            result = _cached_verification(row["ip"], suffixes)
            requests += int(not result["cached"])
            if not result["cached"]:
                time.sleep(0.05)
        elif enabled:
            result["reason"] = "DNS request budget exhausted; request another bounded page"
        verdict = (
            "verified"
            if result.get("verified") is True
            else "fake"
            if result.get("verified") is False
            else "unchecked"
        )
        checks.append({**row, **result, "verdict": verdict})
    return {
        "checked": enabled and any(row.get("checked_at") is not None for row in checks),
        "requested": enabled,
        "dns_available": available,
        "checks": checks,
        "dns_checks": requests,
        "canary_requests": int(available is not None),
        "reason": "Verification applies only to this page; unchecked rows are not authenticity verdicts",
    }


def _is_ip(value: str) -> bool:
    try:
        ipaddress.ip_address(value)
        return True
    except ValueError:
        return False


def _findings(r: dict[str, Any]) -> list[str]:
    out: list[str] = []
    lines = r["lines"]
    if lines["parsed"] == 0:
        return ["No lines were parsed; the detected format is wrong or the file is empty"]
    if lines["skipped"] > lines["parsed"] * 0.1:
        out.append(
            f"Failed to parse {lines['skipped']} of {lines['total']} lines; "
            "the log may mix formats or contain custom fields"
        )

    fams = r["by_family"]
    if r.get("bot_detection", {}).get("unknown_user_agent"):
        out.append(
            "Bot classification is unavailable for rows without a user-agent; absent bot labels do not prove absent visits"
        )
    if "googlebot" not in fams and not r.get("bot_detection", {}).get("unknown_user_agent"):
        out.append(
            "Googlebot does not appear in the log; the site may not be crawled, "
            "or the log period may be too short"
        )
    if "ai" in fams:
        total_ai = sum(fams["ai"].values())
        out.append(f"AI crawlers made {total_ai} requests ({', '.join(list(fams['ai'])[:4])})")
    if "seo-tool" in fams:
        total_seo = sum(fams["seo-tool"].values())
        out.append(
            f"Third-party SEO crawlers made {total_seo} requests, consuming "
            "your bandwidth and server resources"
        )

    for family, statuses in r["status_by_family"].items():
        if family in ("human", "unknown"):
            continue
        errors = sum(c for s, c in statuses.items() if s >= 400)
        total = sum(statuses.values()) or 1
        if errors / total > 0.1:
            out.append(
                f"{family}: {errors} of {total} requests returned errors "
                f"({errors / total:.0%}), wasting crawl budget"
            )

    ver = r["verification"]
    if ver.get("checked"):
        fake = [c["bot"] for c in ver.get("checks", []) if c.get("verified") is False]
        if fake:
            out.append(
                f"Reverse-DNS verification failed for {', '.join(fake)}; "
                "these requests may be impersonating known bots"
            )
    elif ver.get("dns_available") is False:
        out.append("Bot authenticity was not checked because reverse DNS is unavailable")
    return out
