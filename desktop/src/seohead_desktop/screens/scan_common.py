"""Shared reading of runs and saved scans for the Scans, Scan-run and Journal screens (display only, no core calls).

A row joins what the core observed (``host.observed_runs``), what this window started (``owned_runs_for_project``) and the
saved scans of the loaded page (``host.scan_model.rows``). Missing facts stay ``None`` and render as «Нет данных»;
nothing is inferred (no percentage, no ETA, no error count: the core does not report them, issue #921).
"""

from __future__ import annotations

import os
from datetime import datetime, timezone

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QFrame, QGridLayout, QHBoxLayout, QLabel

from .. import theming
from ..i18n import joined, tr, trf
from ..ui.controls import polish
from ..ui.icons import MaterialIconLabel
from ..ui.presentation import short_run_id

LIVE_ISSUE = 921
JOURNAL_ISSUE = 923
SORT_ISSUES = (927, 933)
OWNED_ACTIVE = {"queued", "starting", "running", "stop_requested", "awaiting_core_status"}

KIND_TITLES = {"native": "Скан", "sitemap": "Sitemap", "screaming_frog": "Импорт Screaming Frog"}
KIND_SOURCES = {"native": "Сайт целиком", "sitemap": "По sitemap", "screaming_frog": "Screaming Frog"}
MODES = {"spider": "Сайт целиком", "list": "Список URL", "sitemap": "По sitemap"}
PHASES = {"admission": "проверка лимитов", "collection": "сбор страниц", "render": "рендеринг", "external": "внешний процесс",
          "analysis": "анализ", "finalizing": "сохранение результата"}
CODES = {"started": "запуск принят", "entered": "вход в этап", "progress": "обновление счётчиков", "finished": "завершено", "failed": "ошибка"}
EVENT_ICONS = {"started": "play_arrow", "entered": "subdirectory_arrow_right", "progress": "travel_explore", "finished": "check_circle", "failed": "error"}


def now():
    return datetime.now(timezone.utc)


def parse_time(value):
    if not isinstance(value, str):
        return None
    try:
        stamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return stamp if stamp.tzinfo is not None else None


def isint(value):
    return type(value) is int


def clock(stamp, today=None):
    """«сегодня ЧЧ:ММ» / «ДД.ММ.ГГГГ ЧЧ:ММ» in local time; None when unknown."""
    if stamp is None:
        return None
    local = stamp.astimezone()
    today = (today or now()).astimezone().date()
    if local.date() == today:
        return trf("сегодня {time}", time=local.strftime("%H:%M"))
    return local.strftime("%d.%m.%Y %H:%M")


def duration(seconds):
    if seconds is None:
        return None
    seconds = max(0, int(seconds))
    if seconds < 60:
        return trf("{s} с", s=seconds)
    if seconds < 3600:
        return trf("{m} мин {s} с", m=seconds // 60, s=seconds % 60)
    return trf("{h} ч {m} мин", h=seconds // 3600, m=seconds % 3600 // 60)


def ago(seconds):
    if seconds is None:
        return None
    seconds = max(0, int(seconds))
    return trf("{n} назад", n=duration(seconds) if seconds < 3600 else trf("{h} ч {m} мин", h=seconds // 3600, m=seconds % 3600 // 60))


def number(value):
    """Integer with thin grouping; None stays None."""
    return f"{value:,}".replace(",", " ") if isint(value) else None


def selected_scan(host):
    path = getattr(host, "selected_scan_path", None)
    return next((row for row in host.scan_model.rows if row.get("path") == path), None) if path else None


def megabytes(value):
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        return None
    return trf("{n} МБ", n=f"{value / 1_048_576:.1f}".replace(".", ",") if value >= 104_858 else "< 0,1")


class RunRow:
    """One line of the run table: a run observed by the core, a run of this window, or a saved scan without a run."""

    def __init__(self, run=None, owned=None, scan=None, observed_at=None):
        self.run, self.owned, self.scan = run, owned, scan
        self.observed_at = parse_time(observed_at) if isinstance(observed_at, str) else observed_at
        run_id = (run or {}).get("id")
        self.key = f"run:{run_id}" if run_id else f"owned:{owned['id']}" if owned else f"scan:{(scan or {}).get('uuid') or (scan or {}).get('path')}"
        self.id = run_id or (owned or {}).get("observer_run_id") or (scan or {}).get("uuid") or ""
        if run:
            self.kind = run.get("kind")
        elif owned:
            self.kind = "sitemap" if owned.get("sitemap_url") else "native"
        else:
            self.kind = (scan or {}).get("source_kind")
        collector = (run or {}).get("collector") or {}
        self.collector = collector
        self.mode = MODES.get(collector.get("mode"), collector.get("mode")) if collector.get("mode") else None
        self.host_name = (scan or {}).get("host") or collector.get("origin")
        counters = (run or {}).get("counters") or {}
        telemetry = (run or {}).get("telemetry") or {}
        self.telemetry = telemetry
        self.fetched, self.queued, self.inflight, self.excluded = (counters.get(key) if isint(counters.get(key)) else None
                                                                    for key in ("fetched", "queued", "inflight", "excluded"))
        self.found = self.fetched + self.queued + self.inflight + self.excluded if None not in (self.fetched, self.queued, self.inflight, self.excluded) else None
        rate = counters.get("rate_per_second")
        self.rate = rate if telemetry.get("state") == "fresh" and isinstance(rate, (int, float)) and not isinstance(rate, bool) else None
        self.rate_unit = telemetry.get("unit")
        done = ((scan or {}).get("evidence") or {}).get("frontier", {}).get("counts", {}).get("done") if scan else None
        self.saved_urls = done if isint(done) else None
        self.started = parse_time((run or {}).get("started_at")) or parse_time((scan or {}).get("created_at"))
        self.finished = parse_time((run or {}).get("finished_at")) or parse_time((scan or {}).get("finished_at"))
        self.events = list((run or {}).get("events") or [])
        self.pid_state = (run or {}).get("pid_state")
        self.state = self._state()
        self.group, self.badge_kind, self.badge_icon = self._classify()

    # state
    def _state(self):
        if self.run:
            return self.run.get("state")
        if self.owned:
            return self.owned.get("state")
        scan = self.scan or {}
        return "partial" if scan.get("crawl_partial") else scan.get("lifecycle")

    def _classify(self):
        state, fresh = self.state, self.telemetry.get("state")
        if state == "running":
            if fresh == "fresh":
                return "live", "info", "progress_activity"
            if fresh in ("stale", "retained"):
                return "stale", "warn", "update"
            return "pending", "info", "hourglass_top"
        if state == "stop_requested":
            return "live", "warn", "stop_circle"
        if state in ("queued", "starting", "awaiting_core_status"):
            return "pending", "info", "hourglass_top"
        if state in ("failed", "rejected"):
            return "failed", "err", "error"
        if state == "status_unavailable":
            return "failed", "warn", "update"
        if state in ("interrupted", "partial"):
            return "partial", "warn", "warning"
        if state in ("cancelled", "cancelled_before_start"):
            return "failed", "mut", "cancel"
        if state in ("finished", "complete") and (self.scan or {}).get("crawl_partial") is True:
            return "partial", "warn", "warning"
        if state in ("finished", "complete"):
            return "done", "ok", "check_circle"
        return "done", "mut", "help"

    @property
    def active(self):
        return self.group in ("live", "stale", "pending")

    @property
    def title(self):
        base = tr(KIND_TITLES.get(self.kind, "Запуск скана"))
        return joined(" · ", [base, self.host_name]) if self.host_name else base

    @property
    def source(self):
        if self.kind == "native" and self.mode:
            return tr(self.mode)
        return tr(KIND_SOURCES.get(self.kind, "Нет данных"))

    @property
    def meta(self):
        return short_run_id(self.id)

    def sample_age(self, at=None):
        """Seconds since the sample behind the counters was taken (observation age plus time since the window observed it)."""
        age = self.telemetry.get("age_seconds")
        if not isinstance(age, (int, float)) or isinstance(age, bool):
            return None
        since = max(0.0, ((at or now()) - self.observed_at).total_seconds()) if self.observed_at else 0.0
        return age + since

    def observation_age(self, at=None):
        return max(0.0, ((at or now()) - self.observed_at).total_seconds()) if self.observed_at else None

    def state_label(self, at=None):
        age = self.sample_age(at) if self.group == "stale" else None
        names = {"running": "Идёт", "stop_requested": "Остановка запрошена", "queued": "В очереди", "starting": "Запускается",
                 "awaiting_core_status": "Ожидание статуса ядра", "failed": "Ошибка", "rejected": "Отклонён",
                 "status_unavailable": "Статус не подтверждён", "interrupted": "Прерван", "partial": "Частичный",
                 "cancelled": "Отменён", "cancelled_before_start": "Отменён до старта", "finished": "Завершён", "complete": "Завершён"}
        if self.group == "stale":
            return trf("Устарело · {age}", age=duration(age)) if age is not None else tr("Устарело")
        if self.group == "pending" and self.state == "running":
            return tr("Идёт · измерений ещё нет")
        if self.state == "finished" and self.group == "partial":
            return tr("Частичный")
        return tr(names.get(self.state, "Нет данных"))

    @property
    def urls_text(self):
        if self.active and self.fetched is not None:
            return f"{number(self.fetched)} / {number(self.found)}" if self.found is not None else number(self.fetched)
        value = self.saved_urls if self.saved_urls is not None else self.fetched
        return number(value)

    @property
    def completeness(self):
        scan = self.scan
        if self.active:
            return tr("не определена") if self.group == "stale" else tr("идёт")
        if not scan:
            return None
        crawl, corpus = scan.get("crawl_partial"), scan.get("corpus_partial")
        if crawl is True:
            return tr("частичный")
        if crawl is False and corpus is False:
            return tr("полный")
        if crawl is False and corpus is True:
            return tr("обход полный · корпус неполный")
        return None

    @property
    def started_text(self):
        return clock(self.started)

    def partial_resumable(self):
        scan = self.scan or {}
        return bool(scan.get("path")) and (scan.get("lifecycle") == "interrupted" or scan.get("crawl_partial") is True or self.state in ("interrupted", "partial"))

    def signature(self):
        return (self.key, self.group, self.badge_kind, self.urls_text, self.completeness, self.source, self.title, self.state_label(), self.started_text)


def build_rows(host):
    """Rows for the loaded page of runs and scans; newest first. Reads only what the window already holds."""
    if not host.project_directory:
        return []
    scans = list(host.scan_model.rows)
    by_artifact = {os.path.basename(scan["path"]): scan for scan in scans if isinstance(scan.get("path"), str)}
    owned_runs = host.owned_runs_for_project()
    owned_by_core = {}
    for owned in owned_runs:
        for identity in (owned.get("core_run_id"), owned.get("observer_run_id")):
            if identity:
                owned_by_core[identity] = owned
    rows, used_scans, used_owned = [], set(), set()
    for run in host.observed_runs or []:
        artifact = run.get("artifact")
        scan = by_artifact.get(os.path.basename(artifact)) if isinstance(artifact, str) else None
        owned = owned_by_core.get(run.get("id"))
        if scan:
            used_scans.add(scan.get("path"))
        if owned:
            used_owned.add(owned["id"])
        rows.append(RunRow(run, owned, scan, host.observed_at))
    for owned in owned_runs:
        if owned["id"] in used_owned:
            continue
        artifact = owned.get("artifact")
        scan = by_artifact.get(os.path.basename(artifact)) if isinstance(artifact, str) else None
        if scan:
            used_scans.add(scan.get("path"))
        rows.append(RunRow(None, owned, scan, host.observed_at))
    rows.extend(RunRow(None, None, scan, host.observed_at) for scan in scans if scan.get("path") not in used_scans)
    rows.sort(key=lambda row: row.started.timestamp() if row.started else 0.0, reverse=True)
    return rows


def active_scan_summary(host):
    """Text and counters for the navigation card; ``None`` when nothing is running."""
    active = [row for row in build_rows(host) if row.active]
    if not active:
        return None
    row = active[0]
    stale = row.group == "stale"
    done, total = (row.fetched, row.found) if row.found else (None, None)
    if row.group == "pending" and row.state != "running":
        text = tr("Скан запускается")
    elif done is not None and total:
        text = trf("{kind} · {done} / {total} URL", kind=row.title.split(" · ")[0], done=number(done), total=number(total))
    elif row.fetched is not None:
        text = trf("{kind} · получено {done} URL, объём не измерен", kind=row.title.split(" · ")[0], done=number(row.fetched))
    else:
        text = tr("Скан идёт · число страниц не измерено")
    if stale:
        text = trf("{text} · наблюдение устарело", text=text)
    if len(active) > 1:
        text = trf("{text} · ещё {n}", text=text, n=len(active) - 1)
    return {"text": text, "done": done, "total": total, "stale": stale}


def mode_text(row):
    parts = []
    if row.mode:
        parts.append(tr(row.mode))
    limit = row.collector.get("max_urls")
    if isinstance(limit, int) and not isinstance(limit, bool):
        parts.append(trf("лимит {n} URL", n=number(limit)) if limit > 0 else tr("без лимита URL"))
    return joined(" · ", parts) if parts else None


def events_text(row):
    """Last four lifecycle events (the core gives phase and code only, no message text)."""
    lines = []
    for event in reversed(row.events[-4:]):
        stamp = parse_time(event.get("at"))
        label = trf("{code} · {phase}", code=CODES.get(event.get("code"), "событие"), phase=PHASES.get(event.get("phase"), "этап не указан"))
        lines.append(trf("{time}  {label}", time=stamp.astimezone().strftime("%H:%M:%S") if stamp else "--:--:--", label=label))
    return joined("\n", lines) if lines else tr("События ядра не получены")


class StatusBadge(QLabel):
    """QLabel[badge] with a 16 px icon; ``set_state`` changes kind, text and icon in place."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setFixedHeight(theming.metrics()["control"]["badge"])
        self.setIndent(20)
        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        self.icon = MaterialIconLabel("help", 16)
        layout.addWidget(self.icon)
        layout.addStretch(1)
        self.kind = None

    def set_state(self, kind, text, icon):
        self.setText(text)
        if kind != self.kind:
            self.kind = kind
            self.setProperty("badge", kind)
            polish(self)
        self.icon.set_material_icon(icon, theming.theme()["badges"][kind][1])
        self.setMinimumWidth(self.fontMetrics().horizontalAdvance(text) + 28)


class Pairs(QFrame):
    """Key / value rows with stable value labels, updated in place (``None`` renders italic «Нет данных»)."""

    def __init__(self, keys, mono=(), parent=None):
        super().__init__(parent)
        grid = QGridLayout(self)
        grid.setContentsMargins(0, 0, 0, 0)
        grid.setHorizontalSpacing(16)
        grid.setVerticalSpacing(6)
        grid.setColumnMinimumWidth(0, 140)
        grid.setColumnStretch(1, 1)
        self.values = {}
        for index, key in enumerate(keys):
            caption = QLabel(tr(key))
            caption.setProperty("text_style", "meta")
            value = QLabel()
            value.setWordWrap(True)
            value.setTextInteractionFlags(Qt.TextSelectableByMouse)
            if key in mono:
                value.setProperty("text_style", "mono")
            grid.addWidget(caption, index, 0, Qt.AlignTop)
            grid.addWidget(value, index, 1)
            self.values[key] = value

    def set(self, key, text, na=None):
        label = self.values[key]
        label.setText(text if text else tr("Нет данных"))
        label.setProperty("na", (not text) if na is None else na)
        polish(label)


def open_saved(host, row):
    """Open the saved scan of a row in the URL table (selects it first when it is not the selected one)."""
    if row is None or row.scan is None:
        return False
    if row.scan.get("path") != host.selected_scan_path:
        host.select_project_scan(row.scan)
    host.navigation.select_section("url")
    return True


def can_stop(row):
    return bool(row and row.owned and row.owned.get("state") in ("queued", "starting", "running"))


def request_stop(host, row):
    """Stop only a run this window started (SIGINT through the scan manager); foreign runs wait for #921."""
    if not can_stop(row):
        return False
    host.choose_owned_run(row.owned["id"])
    host.cancel_active_work()
    return True
