"""Thin adapter between the desktop and the core's project schedule and crawl settings.

The desktop keeps no schedule state of its own: every value is read from and written to the project through the core
CLI. The core commands below are the contract of issue #1322 (core branch feat/schedule-core). Until they exist, the
core answers "unknown command" and the page shows the neutral unavailable state.

Every core field name lives in this module (SCHEDULE_FIELDS, CRAWL_FIELDS, the command names), so one change here
realigns the desktop with the core.
"""

from __future__ import annotations

import json
import re
import subprocess
from datetime import datetime
from zoneinfo import ZoneInfo, available_timezones

from PyQt5 import sip
from PyQt5.QtCore import QObject, QRunnable, QThreadPool, pyqtSignal

from .source_service import read_cli

# Core commands (CLI). Each answers one JSON object with "ok".
READ_COMMAND = "project-schedule"
UPDATE_COMMAND = "project-schedule-update"
RUN_DUE_COMMAND = "schedule-run-due"

# Field names inside the project's settings document (the core validates the same names).
SCHEDULE_FIELDS = ("enabled", "weekdays", "days_of_month", "window_start", "window_end", "time_zone", "competitors")
CRAWL_FIELDS = ("mode", "user_agent_preset", "user_agent", "requests_per_second", "url_budget")
MODES = {"spider": "Полный обход по ссылкам", "sitemap": "По sitemap", "url_list": "Список важных URL"}
WEEKDAYS = ("Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс")  # index 0 is Monday, as in datetime.weekday()

SPIDERHEAD_VERSION = "3.0.0"  # keep equal to seohead.__version__; the core may return its own default later
SPIDERHEAD_USER_AGENT = f"SpiderHeadBot/{SPIDERHEAD_VERSION} (+https://github.com/PavloSEO/spiderhead)"
CHROME_DESKTOP = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36"
CHROME_MOBILE = "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36"
# Crawler identities as the vendors publish them (no third-party crawler strings).
USER_AGENT_PRESETS = {
    "spiderhead": ("SpiderHeadBot (по умолчанию)", SPIDERHEAD_USER_AGENT),
    "googlebot_smartphone": ("Googlebot Smartphone", "Mozilla/5.0 (Linux; Android 6.0.1; Nexus 5X Build/MMB29P) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"),
    "googlebot_desktop": ("Googlebot Desktop", "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; Googlebot/2.1; +http://www.google.com/bot.html) Chrome/130.0.0.0 Safari/537.36"),
    "bingbot": ("Bingbot", "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm) Chrome/116.0.1938.76 Safari/537.36"),
    "yandexbot": ("YandexBot", "Mozilla/5.0 (compatible; YandexBot/3.0; +http://yandex.com/bots)"),
    "yandexmobilebot": ("YandexMobileBot", "Mozilla/5.0 (Linux; Android 8.0; Pixel 2 Build/OPD3.170816.012) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36 (compatible; YandexMobileBot/3.0; +http://yandex.com/bots)"),
    "baiduspider": ("Baiduspider", "Mozilla/5.0 (compatible; Baiduspider/2.0; +http://www.baidu.com/search/spider.html)"),
    "duckduckbot": ("DuckDuckBot", "DuckDuckBot/1.1; (+http://duckduckgo.com/duckduckbot.html)"),
    "applebot": ("Applebot", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko; compatible; Applebot/0.1; +http://www.apple.com/go/applebot) Version/17.0 Safari/605.1.15"),
    "gptbot": ("GPTBot", "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.2; +https://openai.com/gptbot)"),
    "claudebot": ("ClaudeBot", "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; ClaudeBot/1.0; +claudebot@anthropic.com)"),
    "perplexitybot": ("PerplexityBot", "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; PerplexityBot/1.0; +https://perplexity.ai/perplexitybot)"),
    "chrome_desktop": ("Chrome, desktop", CHROME_DESKTOP),
    "chrome_mobile": ("Chrome, Android", CHROME_MOBILE),
    "custom": ("Своя строка", ""),
}
DEFAULT_SETTINGS = {
    "schedule": {"enabled": False, "weekdays": [0], "days_of_month": [], "window_start": "01:00", "window_end": "06:00",
                 "time_zone": "UTC", "competitors": False},
    "crawl": {"mode": "spider", "user_agent_preset": "spiderhead", "user_agent": "", "requests_per_second": 1.0, "url_budget": 5000},
}
RPS_RANGE = (0.1, 10.0)
BUDGET_RANGE = (1, 1_000_000)
TIME = re.compile(r"([01]\d|2[0-3]):[0-5]\d")
CONTROL = re.compile(r"[\x00-\x1f\x7f]")
_ZONES = None


def time_zones():
    global _ZONES
    if _ZONES is None:
        _ZONES = sorted(available_timezones() | {"UTC"})
    return _ZONES


def _check(condition, message):
    if not condition:
        raise ValueError(message)


def validate(settings):
    """Return the settings with every field checked and normalised; raise ValueError on the first bad field."""
    schedule_in, crawl_in = settings.get("schedule") or {}, settings.get("crawl") or {}
    schedule = {**DEFAULT_SETTINGS["schedule"], **schedule_in}
    crawl = {**DEFAULT_SETTINGS["crawl"], **crawl_in}
    _check(isinstance(schedule["enabled"], bool) and isinstance(schedule["competitors"], bool), "bool expected")
    weekdays = schedule["weekdays"]
    _check(isinstance(weekdays, list) and all(type(d) is int and 0 <= d <= 6 for d in weekdays), "weekdays 0-6")
    days = schedule["days_of_month"]
    _check(isinstance(days, list) and all(type(d) is int and 1 <= d <= 31 for d in days), "days of month 1-31")
    for key in ("window_start", "window_end"):
        _check(isinstance(schedule[key], str) and TIME.fullmatch(schedule[key]) is not None, f"{key} HH:MM")
    try:
        ZoneInfo(schedule["time_zone"])
    except (ValueError, KeyError, OSError) as exc:
        raise ValueError("unknown time zone") from exc
    _check(crawl["mode"] in MODES, "unknown crawl mode")
    _check(crawl["user_agent_preset"] in USER_AGENT_PRESETS, "unknown user agent preset")
    agent = crawl["user_agent"] if crawl["user_agent_preset"] == "custom" else ""
    if crawl["user_agent_preset"] == "custom":
        _check(isinstance(agent, str) and 0 < len(agent.strip()) <= 512 and not CONTROL.search(agent), "user agent text")
    rps = crawl["requests_per_second"]
    _check(type(rps) in (int, float) and RPS_RANGE[0] <= rps <= RPS_RANGE[1], "requests per second 0.1-10")
    budget = crawl["url_budget"]
    _check(type(budget) is int and BUDGET_RANGE[0] <= budget <= BUDGET_RANGE[1], "URL budget 1-1000000")
    return {
        "schedule": {**schedule, "weekdays": sorted(set(weekdays)), "days_of_month": sorted(set(days))},
        "crawl": {**crawl, "user_agent": agent.strip(), "requests_per_second": float(rps)},
    }


def user_agent_text(crawl):
    """The User-Agent string a scan sends for these crawl settings."""
    if crawl["user_agent_preset"] == "custom":
        return crawl["user_agent"]
    return USER_AGENT_PRESETS[crawl["user_agent_preset"]][1]


def read_args(directory):
    return [READ_COMMAND, "--directory", directory]


def update_args(directory, revision, settings):
    return [UPDATE_COMMAND, "--directory", directory, "--expected-revision", str(revision), "--input", json.dumps(settings)]


def run_due_args(directory):
    return [RUN_DUE_COMMAND, "--directory", directory]


def _stamp(value):
    if not isinstance(value, str):
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).strftime("%d.%m.%Y %H:%M")
    except ValueError:
        return None


def parse_state(value):
    """Settings, revision and the scheduling facts the core reports. Missing fields fall back to the defaults."""
    settings = validate({
        "schedule": (value.get("schedule") or {}),
        "crawl": (value.get("crawl") or {}),
    })
    last = value.get("last_run") if isinstance(value.get("last_run"), dict) else None
    return {
        "revision": value.get("revision") if type(value.get("revision")) is int else 0,
        "settings": settings,
        "next_run": _stamp(value.get("next_run")),
        "last_run": {"at": _stamp(last.get("at")), "status": str(last.get("status") or "")} if last else None,
    }


def read_schedule(executable, directory, call=read_cli):
    return parse_state(call(executable, read_args(directory)))


def save_schedule(executable, directory, revision, settings, call=read_cli):
    checked = validate(settings)
    return parse_state(call(executable, update_args(directory, revision, checked)))


def run_due(executable, directory, call=read_cli):
    """One headless pass of the core scheduler for a project. Safe to call often: the core refuses overlap."""
    value = call(executable, run_due_args(directory))
    running = value.get("running")
    return {"running": running if type(running) is int and running >= 0 else 0}


class _Signals(QObject):
    done = pyqtSignal(object)
    failed = pyqtSignal(str)


class _Job(QRunnable):
    def __init__(self, function):
        super().__init__()
        self.function = function
        self.signals = _Signals()

    def run(self):
        try:
            self.signals.done.emit(self.function())
        except (OSError, ValueError, TypeError, AttributeError, KeyError, subprocess.SubprocessError):
            self.signals.failed.emit(FAILED)


FAILED = "Ядро не ответило или не поддерживает расписание"


class ScheduleJobs(QObject):
    """Runs the blocking core calls on the thread pool; answers for an owner that is gone are dropped."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self._pending = {}
        self._serial = 0

    def submit(self, function, callback, on_error, owner):
        """Answers go to the callbacks only while the owner (the page that asked) still exists."""
        self._serial += 1
        serial = self._serial
        job = self._pending[serial] = _Job(function)

        def deliver(handler, value):
            self._pending.pop(serial, None)
            if not sip.isdeleted(owner):
                handler(value)

        job.signals.done.connect(lambda value: deliver(callback, value))
        job.signals.failed.connect(lambda value: deliver(on_error, value))
        QThreadPool.globalInstance().start(job)
