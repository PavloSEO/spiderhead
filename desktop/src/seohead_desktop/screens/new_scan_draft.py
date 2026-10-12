"""Pure draft model of the «Новый скан» dialog: layered defaults, field validation, URL-list counting, the launch plan.

No widgets and no I/O. Values come from the core's crawl-describe-settings descriptor; fields the core has no setting
for are not modelled here at all (the dialog shows them as «ждёт #N» and they never reach the command).
"""

from __future__ import annotations

import csv
import io
import math
import re
import shlex
from copy import deepcopy
from dataclasses import dataclass
from urllib.parse import urlsplit, urlunsplit

from PyQt5.QtCore import QObject, pyqtSignal

from ..crawl_configuration import describe_controls, validate_overrides
from ..i18n import tr, trf
from ..scan_runner import crawl_arguments

# Unavailable fields remain gated by the connected core capabilities.
ISSUE_OTHER = 920
ISSUE_SETTINGS = 925
ISSUE_ESTIMATE = 931
ISSUE_URL_QUERY = 927
ISSUE_PROFILES = 941
ISSUE_RENDER = 950
ISSUE_EXTRACT = 954

SOURCES = ("site", "sitemap", "list", "sf")
LIST_LINE_CAP = 200_000
LIST_FILE_CAP = 20 * 1024 * 1024
RPS_SAFE = 2.0
RPS_MAX = 10.0
# Always written into the command, so limits and speed are explicit and never silently inherited.
EXPLICIT = (
    "limits.max_urls",
    "limits.max_depth",
    "limits.max_requests",
    "limits.max_crawl_seconds",
    "speed.min_delay_seconds",
    "speed.concurrency",
    "rendering.mode",
    "storage.body_mode",
    "robots.policy",
)
GOOGLEBOT_UA = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"
YANDEX_UA = "Mozilla/5.0 (compatible; YandexBot/3.0; +http://yandex.com/bots)"
MOBILE_UA = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) Mobile/15E148"
FILE_GROUPS = {
    "images": ("Изображения", ("jpg", "jpeg", "png", "gif", "webp", "svg", "avif", "ico")),
    "css": ("CSS", ("css",)),
    "js": ("JavaScript", ("js", "mjs")),
    "pdf": ("PDF", ("pdf",)),
    "fonts": ("Шрифты", ("woff", "woff2", "ttf", "otf", "eot")),
    "video": ("Видео", ("mp4", "webm", "mov", "avi", "mkv")),
    "archives": ("Архивы", ("zip", "gz", "tar", "rar", "7z")),
}


@dataclass(frozen=True)
class Num:
    """A numeric text field over one core setting; ``scale`` converts the shown unit to the core unit."""

    path: str
    integer: bool
    low: float
    high: float | None
    message: str
    scale: float = 1


NUMS = {
    "threads": Num("speed.concurrency", True, 1, 1024, "Целое число от 1 до 1024"),
    "limit": Num("limits.max_urls", True, 1, 1_000_000, "Целое число от 1 до 1 000 000"),
    "depth": Num(
        "limits.max_depth", True, -1, None, "Целое число от −1; −1 — без ограничения глубины"
    ),
    "requests": Num(
        "limits.max_requests", True, 0, 2_000_000, "Целое число от 0 до 2 000 000; 0 — без лимита"
    ),
    "minutes": Num(
        "limits.max_crawl_seconds", True, 0, None, "Целое число минут от 0; 0 — без лимита", 60
    ),
    "min_delay": Num("speed.min_delay_seconds", False, 0, None, "Число секунд от 0"),
    "max_delay": Num("speed.max_delay_seconds", False, 0.5, None, "Число секунд от 0,5"),
    "timeouts": Num("speed.stop_after_consecutive_timeouts", True, 1, None, "Целое число от 1"),
    "script_timeout": Num(
        "rendering.browser.script_timeout_seconds", False, 0, None, "Число секунд от 0"
    ),
    "body_mb": Num("storage.max_body_bytes", True, 1, None, "Целое число МБ от 1", 1024**2),
    "free_gb": Num(
        "storage.min_free_bytes",
        True,
        10,
        None,
        "Целое число ГБ не меньше 10: правило приложения",
        1024**3,
    ),
}


def is_local_host(host):
    host = (host or "").lower().rstrip(".")
    return host in {"localhost", "127.0.0.1", "::1"} or host.endswith(".localhost")


def grouped(number):
    return f"{number:,}".replace(",", "\u00a0")


def _plain(value):
    if isinstance(value, float) and value.is_integer():
        value = int(value)
    return str(value).replace(".", ",") if isinstance(value, float) else str(value)


def parse_number(text, integer):
    """The number in a user-typed text (spaces and a decimal comma allowed) or None."""
    cleaned = re.sub(r"[\s  ]", "", str(text)).replace(",", ".").replace("−", "-")
    if not cleaned or not re.fullmatch(r"[+-]?\d+(\.\d+)?", cleaned):
        return None
    if integer and not re.fullmatch(r"[+-]?\d+", cleaned):
        return None
    value = float(cleaned)
    return int(cleaned) if integer else value if math.isfinite(value) else None


@dataclass(frozen=True)
class ListReport:
    ready: int = 0
    added: int = 0
    duplicates: int = 0
    foreign: int = 0
    invalid: int = 0
    truncated: bool = False
    urls: tuple = ()

    @property
    def usable(self):
        return self.ready + self.added


def normalize_list(text, host, *, cap=LIST_LINE_CAP):
    """Minimal app-side count of a pasted URL list; the core has no list preview (waits for #931).

    A line without a scheme gets https://; fragments are dropped; the first of equal URLs wins; URLs of another host
    are skipped. Equality is by scheme, host (case-insensitive) and the rest of the URL as typed.
    """
    seen, urls = set(), []
    ready = added = duplicates = foreign = invalid = 0
    lines = text.splitlines()
    for line in lines[:cap]:
        raw = line.strip()
        if not raw or raw.startswith("#"):
            continue
        has_scheme = re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*://", raw) is not None
        candidate = raw if has_scheme else "https://" + raw.lstrip("/")
        try:
            parts = urlsplit(candidate)
            parts.port  # noqa: B018 - malformed ports raise here
        except ValueError:
            invalid += 1
            continue
        if (
            parts.scheme.lower() not in {"http", "https"}
            or not parts.hostname
            or parts.username
            or parts.password
            or len(candidate) > 4096
            or any(char in candidate for char in " \x00")
        ):
            invalid += 1
            continue
        if host and parts.hostname.lower() != host.lower():
            foreign += 1
            continue
        key = urlunsplit(
            (parts.scheme.lower(), parts.netloc.lower(), parts.path or "/", parts.query, "")
        )
        if key in seen:
            duplicates += 1
            continue
        seen.add(key)
        urls.append(key)
        if has_scheme:
            ready += 1
        else:
            added += 1
    return ListReport(ready, added, duplicates, foreign, invalid, len(lines) > cap, tuple(urls))


def list_from_file_text(text, suffix):
    """Lines of URLs from a .txt or .csv file; a CSV contributes its url/address column (else the first)."""
    if suffix.lower() != ".csv":
        return text
    rows = list(
        csv.reader(io.StringIO(text), delimiter=";" if text.count(";") > text.count(",") else ",")
    )
    if not rows:
        return ""
    names = {"url", "urls", "address", "адрес", "loc"}
    header = [cell.strip().lower() for cell in rows[0]]
    column = next((i for i, cell in enumerate(header) if cell in names), 0)
    body = rows[1:] if any(cell in names for cell in header) else rows
    return "\n".join(row[column].strip() for row in body if len(row) > column)


class PlanError(ValueError):
    """A launch plan the core or the app refuses; ``field`` names the control that should show the message."""

    def __init__(self, field, message):
        super().__init__(message)
        self.field = field


def explain(error):
    text = str(error)
    if "Sitemap URL" in text:
        return tr(
            "Укажите полный HTTP(S)-адрес sitemap без логина, пароля и #фрагмента, до 4096 символов."
        )
    if "port" in text.lower():
        return tr(
            "В адресе sitemap некорректный порт. Исправьте адрес; остальные настройки сохранены."
        )
    if "selected scan project" in text:
        return tr("Папка проекта недоступна. Настройки сохранены; проверьте расположение проекта.")
    return text


class ScanDraft(QObject):
    """Everything the dialog edits. ``values`` holds the effective core settings, ``initial`` the layered start."""

    changed = pyqtSignal()

    def __init__(
        self, descriptor, *, target="", host="", project_directory="", prefs=None, parent=None
    ):
        super().__init__(parent)
        self.descriptor = descriptor
        self.capabilities = (descriptor or {}).get("capabilities") or {}
        self.target, self.host, self.project_directory = (
            target or "",
            (host or "").lower(),
            project_directory,
        )
        self.local = is_local_host(self.host)
        rows = {row["path"]: row for row in describe_controls(descriptor)}
        self.rows = rows
        self.core = {p: deepcopy(r["default"]) for p, r in rows.items() if r["editable"]}
        self.app_layer = self._from_prefs(prefs)
        self.project_layer = {}
        self.policy_state = "unknown"  # unknown | loading | ready | unavailable
        self.source = "site"
        self.sitemap_url = ""
        self.list_text = ""
        self.confirmed = False
        self.texts, self.errors = {}, {}
        self._signature = None
        self.reset()

    # ---- layers ------------------------------------------------------------------------------------------------
    def _from_prefs(self, prefs):
        layer = {}
        if prefs is None:
            return layer
        rate = prefs.get("scan.rate")
        if isinstance(rate, (int, float)) and rate > 0:
            layer["speed.min_delay_seconds"] = round(1 / rate, 6)
        limit = prefs.get("scan.url_limit")
        if isinstance(limit, int) and limit > 0:
            layer["limits.max_urls"] = limit
        depth = prefs.get("scan.depth")
        if isinstance(depth, int) and depth >= 0:
            layer["limits.max_depth"] = (
                depth or -1
            )  # the setting's 0 means «no limit», the core's is −1
        layer["robots.policy"] = "respect" if prefs.get("scan.robots") is not False else "ignore"
        layer["storage.body_mode"] = (
            "captured_entity_bytes" if prefs.get("scan.save_html") is not False else "off"
        )
        return {p: v for p, v in layer.items() if p in self.core}

    def _start(self, stack=("core", "app", "project")):
        """Layered defaults; request and time budgets are explicitly «no limit» unless a layer above the core sets them."""
        merged = {}
        for name in stack:
            merged.update(
                {"core": self.core, "app": self.app_layer, "project": self.project_layer}[name]
            )
        for path in ("limits.max_requests", "limits.max_crawl_seconds"):
            if path in self.core and path not in self.app_layer and path not in self.project_layer:
                merged[path] = 0
        return {p: deepcopy(v) for p, v in merged.items() if p in self.core}

    def reset(self, stack=("core", "app", "project")):
        """Back to the layered defaults (project profile > application defaults > core defaults)."""
        self.values = self._start(stack)
        self.initial = deepcopy(self.values)
        limit = self.values.get("limits.max_urls", 0)
        self.limit_enabled = limit > 0
        self.url_limit = limit if limit > 0 else int(self.app_layer.get("limits.max_urls") or 1500)
        self.values["limits.max_urls"] = self.url_limit if self.limit_enabled else 0
        self.initial["limits.max_urls"] = self.values["limits.max_urls"]
        self.errors = {}
        self.sync_texts()
        self.emit_changed()

    def apply_project_policy(self, overrides):
        """Fold the project's policy.crawl_overrides in as the top default layer (it wins over application defaults)."""
        self.project_layer = {p: v for p, v in (overrides or {}).items() if p in self.core}
        self.policy_state = "ready"
        edited = set(self.edited_paths())
        for path, value in self._start().items():
            if path not in edited and path != "limits.max_urls":
                self.values[path] = deepcopy(value)
                self.initial[path] = deepcopy(value)
        policy_limit = self.project_layer.get("limits.max_urls")
        if (
            policy_limit is not None
            and "limits.max_urls" not in edited
            and "limit" not in self.errors
        ):
            self.limit_enabled = policy_limit > 0
            self.url_limit = policy_limit if policy_limit > 0 else self.url_limit
            self.values["limits.max_urls"] = self.url_limit if self.limit_enabled else 0
            self.initial["limits.max_urls"] = self.values["limits.max_urls"]
        self.sync_texts(keep=set(self.errors))
        self.emit_changed()

    @property
    def policy_paths(self):
        return set(self.project_layer)

    # ---- values and texts --------------------------------------------------------------------------------------
    def has(self, path):
        return path in self.core

    def value(self, path, default=None):
        return self.values.get(path, default)

    def set_value(self, path, value):
        if path in self.core:
            self.values[path] = deepcopy(value)
            self.emit_changed()

    def display(self, key):
        spec = NUMS[key]
        value = self.effective(spec.path)
        return "" if value is None else _plain(value / spec.scale if spec.scale != 1 else value)

    def rps_text(self):
        delay = self.values.get("speed.min_delay_seconds")
        return _plain(float(f"{1 / delay:.3g}")) if delay and delay > 0 else ""

    def sync_texts(self, keep=()):
        for key in [*NUMS, "rps"]:
            if key in keep:
                continue
            self.errors.pop(key, None)
            if key == "rps":
                self.texts[key] = self.rps_text()
            elif key == "limit":
                self.texts[key] = _plain(self.url_limit)
            elif NUMS[key].path in self.core:
                self.texts[key] = self.display(key)

    def effective(self, path):
        """The value that will really be used: the full-scan disk guard is shown as such."""
        if path == "storage.min_free_bytes" and self.values.get(path) == self.initial.get(path):
            if not self.limit_enabled and self.capabilities.get("full_site_native_sqlite") is True:
                return 12 * 1024**3
        return self.values.get(path)

    def set_limit_enabled(self, enabled):
        self.limit_enabled = bool(enabled)
        self.values["limits.max_urls"] = self.url_limit if self.limit_enabled else 0
        if not self.limit_enabled:
            self.errors.pop("limit", None)
        self.sync_texts(keep=set(self.errors))
        self.emit_changed()

    def set_text(self, key, text):
        """Store typed text; a valid number updates the setting, an invalid one keeps the text and shows its error."""
        self.texts[key] = text
        if key == "rps":
            number = parse_number(text, False)
            if number is None or number <= 0:
                self.errors["rps"] = tr("Введите положительное число запросов в секунду")
            else:
                self.errors.pop("rps", None)
                self.values["speed.min_delay_seconds"] = round(1 / number, 6)
                self.errors.pop("min_delay", None)
                self.texts["min_delay"] = self.display("min_delay")
        else:
            spec = NUMS[key]
            number = parse_number(text, spec.integer)
            core = None if number is None else number * spec.scale
            ok = (
                number is not None
                and number >= spec.low
                and (spec.high is None or number <= spec.high)
            )
            if not ok:
                self.errors[key] = tr(spec.message)
            else:
                self.errors.pop(key, None)
                core = round(core) if spec.integer else float(core)
                if key == "limit":
                    self.url_limit = core
                    if self.limit_enabled:
                        self.values["limits.max_urls"] = core
                else:
                    self.values[spec.path] = core
                if key == "min_delay":
                    self.errors.pop("rps", None)
                    self.texts["rps"] = self.rps_text()
        self.emit_changed()

    def edited_paths(self):
        return sorted(p for p, v in self.values.items() if v != self.initial.get(p))

    # ---- list ----------------------------------------------------------------------------------------------------
    def list_report(self):
        cached = getattr(self, "_list_cache", None)
        if cached is None or cached[0] != (self.list_text, self.host):
            cached = ((self.list_text, self.host), normalize_list(self.list_text, self.host))
            self._list_cache = cached
        return cached[1]

    # ---- validation ----------------------------------------------------------------------------------------------
    def rps(self):
        delay = self.values.get("speed.min_delay_seconds")
        return 1 / delay if delay and delay > 0 else None

    def problems(self):
        """Control key -> message for everything that blocks the launch, in display order."""
        found = dict(self.errors)
        rate = parse_number(self.texts.get("rps", ""), False)
        if "rps" not in found and rate is not None:
            if rate > RPS_MAX:
                found["rps"] = trf("Не больше {max} запросов/с", max=int(RPS_MAX))
        if (
            not self.limit_enabled
            and self.capabilities.get("full_site_native_sqlite") is not True
            and "limit" not in found
        ):
            found["limit"] = tr(
                "Это ядро не поддерживает обход без лимита URL. Включите лимит или обновите комплект."
            )
        if self.values.get("robots.policy") == "ignore" and not self.local:
            found["robots"] = tr(
                "Игнорировать robots.txt на чужом боевом сайте нельзя; только для своего стенда"
            )
        delay, ceiling = (
            self.values.get("speed.min_delay_seconds"),
            self.values.get("speed.max_delay_seconds"),
        )
        if (
            delay is not None
            and ceiling is not None
            and delay > ceiling
            and "min_delay" not in found
        ):
            found["min_delay"] = tr("Пауза не может быть больше максимальной паузы при замедлении")
        width, height = (
            self.values.get("rendering.browser.viewport_width", 0),
            self.values.get("rendering.browser.viewport_height", 0),
        )
        if bool(width) != bool(height):
            found["viewport"] = tr("Ширина и высота окна задаются вместе")
        if self.source == "sitemap":
            if self.capabilities.get("sitemap_only_retained") is not True:
                found["source"] = tr("Подключённое ядро не поддерживает сохранённый sitemap-скан")
            elif not self.sitemap_url.strip():
                found["sitemap"] = tr("Укажите адрес sitemap")
            else:
                try:
                    crawl_arguments(
                        self.project_directory, 1, "raw", sitemap_url=self.sitemap_url.strip()
                    )
                except ValueError as exc:
                    if "project is unavailable" not in str(exc):
                        found["sitemap"] = explain(exc)
        elif self.source == "list":
            report = self.list_report()
            if report.truncated:
                found["list"] = trf(
                    "Список длиннее {n} строк: разделите его", n=grouped(LIST_LINE_CAP)
                )
            elif not report.usable:
                found["list"] = tr("В списке нет ни одного подходящего адреса")
            found["source"] = tr(
                "Запуск списка URL из приложения ждёт ядра: скан списка не попадает в наблюдение проекта"
            )
        elif self.source == "sf":
            found["source"] = tr("Недоступно в этой сборке")
        return found

    def overrides(self):
        explicit = {p: self.values[p] for p in EXPLICIT if p in self.values}
        explicit["limits.max_urls"] = self.url_limit if self.limit_enabled else 0
        changed = {p: self.values[p] for p in self.edited_paths() if p != "limits.max_urls"}
        merged = {**explicit, **changed}
        if "storage.min_free_bytes" not in changed and self.effective(
            "storage.min_free_bytes"
        ) != self.values.get("storage.min_free_bytes"):
            merged["storage.min_free_bytes"] = self.effective("storage.min_free_bytes")
        if merged.get("rendering.mode") != "js":
            merged = {
                p: v
                for p, v in merged.items()
                if p == "rendering.mode" or not p.startswith("rendering.")
            }
        return merged

    def build_plan(self):
        """(max_urls, mode, max_requests, max_seconds, approve, overrides, sitemap) as ``MainWindow.launch_scan`` takes it."""
        blocked = self.problems()
        if blocked:
            key = next(iter(blocked))
            raise PlanError(key, blocked[key])
        if self.source not in ("site", "sitemap"):
            raise PlanError("source", tr("Источник не запускается из приложения"))
        settings = self.overrides()
        try:
            preview = validate_overrides(self.descriptor, settings)
        except ValueError as exc:
            raise PlanError(self._field_of(str(exc)), self._friendly(str(exc))) from exc
        sitemap = self.sitemap_url.strip() if self.source == "sitemap" else None
        try:
            crawl_arguments(
                self.project_directory,
                settings["limits.max_urls"],
                settings["rendering.mode"],
                overrides=tuple(preview.items()),
                approve_large_crawl=True,
                sitemap_url=sitemap,
            )
        except ValueError as exc:
            raise PlanError("sitemap" if sitemap is not None else "source", explain(exc)) from exc
        return (
            settings["limits.max_urls"],
            settings["rendering.mode"],
            settings.get("limits.max_requests", 0),
            settings.get("limits.max_crawl_seconds", 0),
            True,
            settings,
            sitemap,
        )

    def command_text(self):
        """The core command this draft would run, with the project folder as <проект>; empty when the draft is invalid."""
        sitemap = self.sitemap_url.strip() if self.source == "sitemap" else None
        try:
            settings = self.overrides()
            checked = validate_overrides(self.descriptor, settings)
            args = crawl_arguments(
                self.project_directory,
                settings["limits.max_urls"],
                settings["rendering.mode"],
                overrides=tuple(checked.items()),
                approve_large_crawl=True,
                sitemap_url=sitemap or None,
            )
        except (ValueError, KeyError, OSError):
            return ""
        shown = [
            tr("<проект>") if index and args[index - 1] == "--project" else shlex.quote(item)
            for index, item in enumerate(args)
        ]
        return "seohead " + " ".join(shown)

    @staticmethod
    def _path_of(message):
        match = re.match(r"([a-z][a-z0-9_.]*):", message)
        return match.group(1) if match else ""

    def _field_of(self, message):
        path = self._path_of(message)
        for key, spec in NUMS.items():
            if spec.path == path:
                return key
        return {"robots.policy": "robots"}.get(path, path or "source")

    def _friendly(self, message):
        path = self._path_of(message)
        for spec in NUMS.values():
            if spec.path == path:
                return tr(spec.message)
        return message

    def signature(self):
        report = self.list_report() if self.source == "list" else None
        return (
            self.source,
            self.sitemap_url.strip(),
            report.usable if report else None,
            self.limit_enabled,
            tuple(sorted((p, repr(v)) for p, v in self.overrides().items())),
        )

    # ---- persistence -------------------------------------------------------------------------------------------
    def snapshot(self):
        return {
            "source": self.source,
            "sitemap": self.sitemap_url,
            "list": self.list_text,
            "rps": self.texts.get("rps", ""),
            "save_html": self.values.get("storage.body_mode") != "off",
            "limit_enabled": self.limit_enabled,
            "limit": self.url_limit,
            "values": deepcopy(self.values),
        }

    def restore(self, saved):
        if not isinstance(saved, dict) or "values" not in saved:
            return
        self.source = saved.get("source") if saved.get("source") in SOURCES else "site"
        self.sitemap_url, self.list_text = str(saved.get("sitemap", "")), str(saved.get("list", ""))
        self.values.update({p: v for p, v in saved["values"].items() if p in self.core})
        self.limit_enabled, self.url_limit = (
            bool(saved.get("limit_enabled")),
            int(saved.get("limit") or self.url_limit),
        )
        self.values["limits.max_urls"] = self.url_limit if self.limit_enabled else 0
        self.sync_texts()
        self.emit_changed()

    def clone(self):
        other = ScanDraft(
            self.descriptor,
            target=self.target,
            host=self.host,
            project_directory=self.project_directory,
        )
        other.app_layer, other.project_layer, other.policy_state = (
            dict(self.app_layer),
            dict(self.project_layer),
            self.policy_state,
        )
        other.adopt(self)
        return other

    def adopt(self, other):
        """Copy the editable state of ``other`` into this draft (the settings window works on a clone)."""
        self.values, self.initial = deepcopy(other.values), deepcopy(other.initial)
        self.limit_enabled, self.url_limit = other.limit_enabled, other.url_limit
        self.source, self.sitemap_url, self.list_text = (
            other.source,
            other.sitemap_url,
            other.list_text,
        )
        self.texts, self.errors = dict(other.texts), dict(other.errors)
        self.emit_changed()

    def emit_changed(self):
        self.changed.emit()


def request_project_policy(host, draft):
    """Ask the host for the project scan policy once per draft; apply it when it arrives."""
    if draft.policy_state != "unknown" or not hasattr(host, "request_scan_policy"):
        return
    draft.policy_state = "loading"

    def arrived(overrides):
        try:
            draft.apply_project_policy(overrides)
        except RuntimeError:
            pass

    def failed(_text):
        try:
            draft.policy_state = "unavailable"
            draft.emit_changed()
        except RuntimeError:
            pass

    if not host.request_scan_policy(arrived, failed):
        draft.policy_state = "unavailable"
