"""Thin adapter between the desktop and the core's project schedule (seohead.projects.schedule).

The desktop keeps no schedule state of its own. Settings live in the project's schedule.json and are read and written
through the core CLI; the runtime facts (last window, next due time) come from `schedule-show`. This module is the only
place that knows the core's command names and field names, so a change in the core is a change here.
"""

from __future__ import annotations

import json
import re
import subprocess
from datetime import datetime
from zoneinfo import available_timezones

from PyQt5 import sip
from PyQt5.QtCore import QObject, QRunnable, QThreadPool, pyqtSignal, pyqtSlot

# Core commands (CLI). Each prints one JSON object on stdout; a refusal prints nothing there and "error: ..." on stderr.
READ_COMMAND = "schedule-show"
UPDATE_COMMAND = "schedule-set"
RUN_DUE_COMMAND = "schedule-run-due"
RUN_TIMEOUT_SECONDS = (
    6 * 3600
)  # a run crawls over the network; the core's own lease is six hours too

# Core field names and values.
DAYS = ("mon", "tue", "wed", "thu", "fri", "sat", "sun")  # datetime.weekday() order
DAY_LABELS = dict(zip(DAYS, ("Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс")))
AUTO_ZONE = "auto"
MODES = {"spider": "Полный обход по ссылкам", "sitemap": "По sitemap", "list": "Список URL"}
DELAY_RANGE = (0.5, 60.0)  # crawl.min_delay_seconds
BUDGET_RANGE = (1, 1_000_000)  # crawl.max_urls
LIST_LIMIT = 500  # crawl.urls
DEFAULT_PRESET = "spiderhead"
# Core user-agent presets (seohead.crawl.user_agents.PRESETS), with the labels the UI shows.
USER_AGENT_PRESETS = {
    "spiderhead": "SpiderHeadBot (по умолчанию)",
    "googlebot": "Googlebot",
    "googlebot-desktop": "Googlebot Desktop",
    "googlebot-smartphone": "Googlebot Smartphone",
    "bingbot": "Bingbot",
    "yandexbot": "YandexBot",
    "yandexmobilebot": "YandexMobileBot",
    "baiduspider": "Baiduspider",
    "duckduckbot": "DuckDuckBot",
    "applebot": "Applebot",
    "gptbot": "GPTBot",
    "claudebot": "ClaudeBot",
    "perplexitybot": "PerplexityBot",
    "chrome-desktop": "Chrome, desktop",
    "firefox-desktop": "Firefox, desktop",
    "safari-desktop": "Safari, desktop",
    "edge-desktop": "Edge, desktop",
    "chrome-mobile": "Chrome, Android",
    "safari-mobile": "Safari, iPhone",
    "custom": "Своя строка",
}
CUSTOM = (
    "custom"  # UI choice only: the core stores it as user_agent.custom with the default preset kept
)
MAX_UA_LENGTH = 512

DEFAULT_SETTINGS = {
    "enabled": False,
    "time_zone": AUTO_ZONE,
    "weekdays": [],
    "month_days": [],
    "window": {"start": "01:00", "end": "06:00"},
    "include_competitors": False,
    "crawl": {
        "mode": "spider",
        "sitemap_url": None,
        "urls": [],
        "max_urls": 500,
        "max_requests": 2000,
        "min_delay_seconds": 1.0,
        "slice": False,
    },
    "user_agent": {"preset": DEFAULT_PRESET, "custom": None},
}
_TIME = re.compile(r"([01]\d|2[0-3]):[0-5]\d")
_CONTROL = re.compile(r"[\x00-\x1f\x7f]")
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
    """Check the settings the UI is about to send. The core validates again and is the authority."""
    _check(
        isinstance(settings.get("enabled"), bool)
        and isinstance(settings.get("include_competitors"), bool),
        "flags",
    )
    zone = settings.get("time_zone")
    if zone != AUTO_ZONE:
        _check(isinstance(zone, str) and zone in set(time_zones()), "unknown time zone")
    _check(all(day in DAYS for day in settings.get("weekdays", [])), "weekdays")
    days = settings.get("month_days", [])
    _check(all(type(day) is int and 1 <= day <= 31 for day in days), "month days 1-31")
    window = settings.get("window") or {}
    for key in ("start", "end"):
        _check(
            isinstance(window.get(key), str) and _TIME.fullmatch(window[key]) is not None,
            "window HH:MM",
        )
    _check(window["start"] != window["end"], "window start and end differ")
    crawl = settings.get("crawl") or {}
    _check(crawl.get("mode") in MODES, "crawl mode")
    if crawl["mode"] == "sitemap":
        _check(_is_http(crawl.get("sitemap_url")), "sitemap URL")
    urls = crawl.get("urls") or []
    _check(
        len(urls) <= LIST_LIMIT
        and len(set(urls)) == len(urls)
        and all(_is_http(url) for url in urls),
        "URL list",
    )
    _check(crawl["mode"] != "list" or urls, "list mode needs URLs")
    budget = crawl.get("max_urls")
    _check(type(budget) is int and BUDGET_RANGE[0] <= budget <= BUDGET_RANGE[1], "URL budget")
    delay = crawl.get("min_delay_seconds")
    _check(type(delay) in (int, float) and DELAY_RANGE[0] <= delay <= DELAY_RANGE[1], "delay")
    _check(type(crawl.get("slice")) is bool, "slice")
    agent = settings.get("user_agent") or {}
    if agent.get("custom") is not None:
        text = agent["custom"]
        _check(
            isinstance(text, str)
            and 0 < len(text) <= MAX_UA_LENGTH
            and not _CONTROL.search(text)
            and text == text.strip(),
            "user agent",
        )
    else:
        _check(
            agent.get("preset") in USER_AGENT_PRESETS and agent.get("preset") != CUSTOM,
            "user agent preset",
        )
    return settings


def _is_http(value):
    return (
        isinstance(value, str)
        and value.startswith(("http://", "https://"))
        and len(value) > 8
        and " " not in value
    )


def user_agent_text(agent):
    """The label-level preview of what the scan sends; the exact string is the core's (see the CLI)."""
    return (
        agent["custom"]
        if agent.get("custom") is not None
        else USER_AGENT_PRESETS.get(agent.get("preset"), "")
    )


def read_args(directory):
    return [READ_COMMAND, "--directory", directory]


def update_args(directory, revision, settings):
    return [
        UPDATE_COMMAND,
        "--directory",
        directory,
        "--expected-revision",
        str(revision),
        "--input",
        json.dumps({"settings": settings}),
    ]


def run_due_args(directory):
    return [RUN_DUE_COMMAND, "--directory", directory]


def _stamp(value):
    if not isinstance(value, str):
        return None
    try:
        return (
            datetime.fromisoformat(value.replace("Z", "+00:00"))
            .astimezone()
            .strftime("%d.%m.%Y %H:%M")
        )
    except ValueError:
        return None


def parse_state(value):
    """What the UI needs from `schedule-show` (or a save answer): settings, revision and the runtime facts."""
    settings = (
        {key: value["settings"][key] for key in DEFAULT_SETTINGS}
        if isinstance(value.get("settings"), dict)
        else None
    )
    if settings is None:
        raise ValueError("core answer has no settings")
    state = value.get("state") if isinstance(value.get("state"), dict) else {}
    results = state.get("last_result") if isinstance(state.get("last_result"), list) else []
    last = None
    if state.get("last_run_at"):
        last = {
            "at": _stamp(state["last_run_at"]),
            "status": "ok" if results and all(r.get("ok") for r in results) else "failed",
        }
    due = value.get("due") if isinstance(value.get("due"), dict) else {}
    return {
        "revision": value["revision"] if type(value.get("revision")) is int else 0,
        "settings": settings,
        "next_run": _stamp(value.get("next_due_at")),
        "last_run": last,
        "due_now": due.get("due") is True,
        "zone": due.get("time_zone"),
        "resume": value.get("resume_artifact") is not None,
        "competitors": [
            dict(item)
            for item in value.get("competitors", [])
            if isinstance(item, dict)
            and isinstance(item.get("directory"), str)
            and item.get("directory")
        ]
        if isinstance(value.get("competitors"), list)
        else [],
    }


def _call(executable, arguments, timeout=45):
    """Run one core command. Returns its JSON object; a refusal (no JSON on stdout) raises with the core's own line."""
    completed = subprocess.run(
        [executable, *arguments], capture_output=True, text=True, timeout=timeout, check=False
    )
    if len(completed.stdout) > 4 * 1024 * 1024:
        raise ValueError("Core answer too large")
    try:
        value = json.loads(completed.stdout)
    except ValueError as exc:
        reason = next(
            (
                line[len("error: ") :]
                for line in completed.stderr.splitlines()
                if line.startswith("error: ")
            ),
            "",
        )
        raise ValueError(reason or "Core command refused") from exc
    if not isinstance(value, dict):
        raise ValueError("Core answer is not an object")
    return value


def read_schedule(executable, directory, call=_call):
    return parse_state(call(executable, read_args(directory)))


def save_schedule(executable, directory, revision, settings, call=_call):
    """Validate, write with the revision that was read, then read back what the core stored with its next run time."""
    checked = validate(settings)
    call(executable, update_args(directory, revision, checked))
    return read_schedule(executable, directory, call=call)


def run_due(executable, directory, call=_call):
    """One headless pass for a project: the core checks the window and the overlap, then runs due scans."""
    value = call(executable, run_due_args(directory), timeout=RUN_TIMEOUT_SECONDS)
    return {
        "ran": value.get("ran") is True,
        "ok": value.get("ok") is not False,
        "reason": str(value.get("reason") or ""),
    }


class _Signals(QObject):
    done = pyqtSignal(object)
    failed = pyqtSignal(str)


FAILED = "Ядро не ответило или не поддерживает расписание"


class _Job(QRunnable):
    def __init__(self, function):
        super().__init__()
        self.function = function
        self.signals = _Signals()

    def run(self):
        try:
            self.signals.done.emit(self.function())
        except (
            ValueError
        ) as exc:  # refusals carry the core's own short line or a validation message
            self.signals.failed.emit(str(exc) or FAILED)
        except (OSError, TypeError, AttributeError, KeyError, subprocess.SubprocessError):
            self.signals.failed.emit(FAILED)


class ScheduleJobs(QObject):
    """Runs the blocking core calls on the thread pool; answers for an owner that is gone are dropped."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self._pending = {}
        self._serial = 0

    def submit(self, function, callback, on_error, owner):
        self._serial += 1
        serial = self._serial
        job = _Job(function)
        delivery = _ScheduleDelivery(self, serial, callback, on_error, owner)
        self._pending[serial] = (job, delivery)
        job.signals.done.connect(delivery.succeeded)
        job.signals.failed.connect(delivery.failed)
        QThreadPool.globalInstance().start(job)

    def _deliver(self, serial, handler, value, owner):
        self._pending.pop(serial, None)
        if not sip.isdeleted(owner):
            handler(value)


class _ScheduleDelivery(QObject):
    """Bound Qt slots ensure worker results are queued back to the GUI thread."""

    def __init__(self, service, serial, callback, on_error, owner):
        super().__init__(service)
        self.service = service
        self.serial = serial
        self.callback = callback
        self.on_error = on_error
        self.owner = owner

    @pyqtSlot(object)
    def succeeded(self, value):
        self.service._deliver(self.serial, self.callback, value, self.owner)

    @pyqtSlot(str)
    def failed(self, value):
        self.service._deliver(self.serial, self.on_error, value, self.owner)
