"""«Расписание» and «Обход» tabs of «Настройки проекта».

Both tabs read the project's schedule through the core (schedule_core) and write it back with the revision they read.
The desktop keeps no copy: after a save the page shows what the core stored. When the core cannot answer (no CLI, or a
core without these commands) the tab shows the neutral waiting state and its controls stay disabled.
"""

from __future__ import annotations

import copy
from datetime import datetime

from PyQt5.QtCore import QEvent, QLocale, QSize, Qt, QTime, pyqtSignal
from PyQt5.QtWidgets import (
    QAbstractSpinBox,
    QBoxLayout,
    QButtonGroup,
    QComboBox,
    QDoubleSpinBox,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMenu,
    QPlainTextEdit,
    QPushButton,
    QScrollArea,
    QSizePolicy,
    QSlider,
    QSpinBox,
    QTimeEdit,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from ..i18n import tr, trf
from ..schedule_core import (
    AUTO_ZONE,
    BUDGET_RANGE,
    CUSTOM,
    DAY_LABELS,
    DAYS,
    DEFAULT_PRESET,
    DELAY_RANGE,
    FAILED,
    MODES,
    USER_AGENT_PRESETS,
    ScheduleJobs,
    read_schedule,
    save_schedule,
    time_zones,
    user_agent_text,
    validate,
)
from ..ui.canvas_components import (
    CalendarDay,
    CanvasField,
    CanvasPreset,
    CanvasSegmented,
    CanvasSwitch,
    ChipFlow,
    CodeCard,
    Columns,
    ModeCard,
    WindowPreview,
    card,
    polish,
)
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import PageHeader, StatePanel


def meta(text):
    label = QLabel(text)
    label.setProperty("text_style", "meta")
    label.setWordWrap(True)
    label.setMinimumHeight(19)
    return label


def field_label(text):
    label = meta(text)
    label.setProperty("canvas_label", True)
    label.setFixedHeight(20)
    return label


def next_run_text(value):
    if not value:
        return "Not scheduled"
    try:
        stamp = datetime.strptime(value, "%d.%m.%Y %H:%M")
    except (TypeError, ValueError):
        return value
    day = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")[stamp.weekday()]
    return f"{day} {stamp:%d.%m}, {stamp:%H:%M}"


def summary_text(state):
    """One line for the project overview and the tab: next and last scheduled run, or why there is none."""
    if state is None:
        return tr("Расписание недоступно в этой версии ядра")
    if not state["settings"]["enabled"]:
        return tr("Расписание выключено")
    last = state["last_run"]
    last_text = (
        f"{last['at']} · {tr('ok') if last['status'] == 'ok' else tr('с ошибкой')}"
        if last and last.get("at")
        else tr("ещё не запускалось")
    )
    return trf(
        "Следующий запуск: {next} · Последний: {last}",
        next=state["next_run"] or tr("не запланирован"),
        last=last_text,
    )


class _SettingsPage(QWidget):
    """Loads the whole schedule document, changes one section, and saves it with the revision it read."""

    availabilityChanged = pyqtSignal(bool)

    def __init__(self, host):
        super().__init__()
        self.setObjectName("scheduleTab")
        self.host = host
        self.state = None
        self.jobs = ScheduleJobs(self)
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(16)
        self.header = PageHeader(self.title, self.subtitle)
        self.header.title.setFixedHeight(28)
        self.header.meta.setMinimumHeight(19)
        self.header.layout().itemAt(0).layout().setSpacing(0)
        outer.addWidget(self.header)
        self.status = QLabel()
        self.status.setProperty("text_style", "meta")
        self.status.setWordWrap(True)
        outer.addWidget(self.status)
        self.unavailable = StatePanel("waiting", "Расписание недоступно", FAILED)
        self.unavailable.hide()
        outer.addWidget(self.unavailable)
        self.form_widget = QWidget()
        self.form_widget.setObjectName("scheduleBody")
        self.body = QVBoxLayout(self.form_widget)
        self.body.setContentsMargins(0, 0, 0, 0)
        self.body.setSpacing(16)
        self.build(self.body)
        self.body.addStretch(1)
        self.scroll = QScrollArea()
        self.scroll.setWidgetResizable(True)
        self.scroll.setFrameShape(QFrame.NoFrame)
        self.scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        self.scroll.setWidget(self.form_widget)
        self.scroll.viewport().setObjectName("scheduleViewport")
        outer.addWidget(self.scroll, 1)
        self.revert = QPushButton(tr("Отменить"))
        self.revert.clicked.connect(self.reload)
        self.save_button = QPushButton(tr("Сохранить"))
        self.save_button.setProperty("role", "primary")
        self.save_button.setIcon(material_icon("save", "role:on_primary"))
        self.save_button.clicked.connect(self.save)
        self.header.add_action(self.revert)
        self.header.add_action(self.save_button)
        polish(self)
        self.reload()

    # Subclasses: title, subtitle, build(layout), fill(settings), apply(settings) changes a copy of the settings.
    def build(self, layout):
        raise NotImplementedError

    def fill(self, settings):
        raise NotImplementedError

    def apply(self, settings):
        raise NotImplementedError

    def _executable(self):
        return getattr(self.host, "core_executable", None)

    def _directory(self):
        return getattr(self.host, "project_directory", None)

    def _set_available(self, available):
        self.unavailable.setVisible(not available)
        self.form_widget.setEnabled(available)
        self.form_widget.setVisible(available)
        enabled_frame = getattr(self, "enabled_frame", None)
        if enabled_frame is not None:
            enabled_frame.setEnabled(available)
            enabled_frame.setVisible(available)
        self.save_button.setEnabled(available and self.state is not None)
        self.revert.setEnabled(available)
        self.availabilityChanged.emit(available)

    def changeEvent(self, event):
        super().changeEvent(event)
        if event.type() == QEvent.EnabledChange:
            self.availabilityChanged.emit(self.isEnabled())

    def reload(self):
        executable, directory = self._executable(), self._directory()
        if not executable or not directory:
            self.state = None
            self._set_available(False)
            self.status.setText(
                tr("Откройте проект с установленным ядром, чтобы менять расписание.")
            )
            return
        self.status.setText(tr("Читаем настройки проекта из ядра…"))
        self.status.show()
        self.setEnabled(False)
        self.jobs.submit(
            lambda: read_schedule(executable, directory), self._loaded, self._failed, self
        )

    def _loaded(self, state):
        self.state = state
        self.fill(state["settings"])
        self.setEnabled(True)
        self._set_available(True)
        self._show_state(state)

    def _failed(self, text):
        self.state = None
        self.setEnabled(True)
        self._set_available(False)
        self.status.setText(text)
        self.status.show()

    def _show_state(self, state):
        self.status.setText("")
        self.status.hide()

    def save(self):
        if self.state is None or not self.isEnabled():
            return
        try:
            settings = validate(self.apply(copy.deepcopy(self.state["settings"])))
        except ValueError:
            self.status.setText(tr("Проверьте значения: часть полей заполнена неверно."))
            self.status.show()
            return
        executable, directory = self._executable(), self._directory()
        revision = self.state["revision"]
        self.status.setText(tr("Сохраняем в ядро…"))
        self.status.show()
        self.setEnabled(False)
        self.jobs.submit(
            lambda: save_schedule(executable, directory, revision, settings),
            self._saved,
            self._save_failed,
            self,
        )

    def _saved(self, state):
        self.state = state
        self.fill(state["settings"])
        self._show_state(state)
        self.setEnabled(True)
        self.availabilityChanged.emit(True)

    def _save_failed(self, text):
        self.setEnabled(True)
        self.status.setText(trf("Не сохранено: {reason}", reason=text))
        self.status.show()


class SchedulePage(_SettingsPage):
    title = "Расписание сканов"
    subtitle = "The site scans itself — at night, a little at a time, in the site's own time zone. Every run lands in the project history."

    def build(self, layout):
        from ..ui.controls import Note
        from .work import project_names

        _, host_name = project_names(self.host)
        primary = QWidget()
        primary.setObjectName("scheduleColumn")
        left = QVBoxLayout(primary)
        left.setContentsMargins(0, 0, 0, 0)
        left.setSpacing(16)
        self.enabled = CanvasSwitch(tr("Schedule enabled"))
        self.enabled_frame = QFrame()
        self.enabled_frame.setProperty("schedule_enabled", True)
        enabled_box = QHBoxLayout(self.enabled_frame)
        enabled_box.setContentsMargins(16, 8, 10, 8)
        enabled_box.setSpacing(12)
        enabled_box.addWidget(QLabel(tr("Enabled")))
        enabled_box.addWidget(self.enabled)
        self.enabled_label = self.enabled_frame.findChild(QLabel)
        self.enabled_label.setProperty("text_style", "control")
        self.header.add_action(self.enabled_frame)
        self.site_host = meta(host_name or "")
        frame, box = card("Your site", "language", subtitle=self.site_host)
        frequency_box = QVBoxLayout()
        frequency_box.setSpacing(8)
        frequency_box.addWidget(field_label(tr("How often")))
        self.frequency = CanvasSegmented(
            [
                ("daily", tr("Every day")),
                ("week", tr("Days of week")),
                ("month", tr("Days of month")),
                ("manual", tr("Manual only")),
            ],
            "daily",
            tr("Frequency"),
        )
        self.frequency.setObjectName("scheduleFrequency")
        buttons = list(self.frequency._buttons.values())
        for index, button in enumerate(buttons):
            button.setProperty(
                "schedule_segment",
                "first" if index == 0 else "last" if index == len(buttons) - 1 else "middle",
            )
        self.frequency.changed.connect(self._frequency_changed)
        frequency_box.addWidget(self.frequency, 0, Qt.AlignLeft)
        box.addLayout(frequency_box)
        calendar_box = QVBoxLayout()
        calendar_box.setSpacing(8)
        self.calendar_label = field_label(tr("Days"))
        calendar_box.addWidget(self.calendar_label)
        self.week_widget = QWidget()
        self.week_widget.setObjectName("scheduleCalendar")
        days = QHBoxLayout(self.week_widget)
        days.setContentsMargins(0, 0, 0, 0)
        days.setSpacing(8)
        self.weekdays = {}
        for name, label in zip(DAYS, ("Mo", "Tu", "We", "Th", "Fr", "Sa", "Su")):
            check = CalendarDay(tr(label))
            check.setProperty("weekend", name in {"sat", "sun"})
            check.setAccessibleName(tr(DAY_LABELS[name]))
            self.weekdays[name] = check
            days.addWidget(check)
        days.addStretch(1)
        calendar_box.addWidget(self.week_widget)
        self.month_widget = QWidget()
        self.month_widget.setObjectName("scheduleCalendar")
        grid = QGridLayout(self.month_widget)
        grid.setContentsMargins(0, 0, 0, 0)
        grid.setSpacing(6)
        self.month_buttons = {}
        for day in range(1, 32):
            button = CalendarDay(str(day))
            button.setAccessibleName(trf("Day {day}", day=day))
            button.clicked.connect(self._month_clicked)
            self.month_buttons[day] = button
            grid.addWidget(button, (day - 1) // 7, (day - 1) % 7)
        grid.setColumnStretch(7, 1)
        calendar_box.addWidget(self.month_widget)
        self.month_days = QLineEdit()
        self.month_days.textChanged.connect(self._month_text_changed)
        self.month_days.hide()
        self.month_hint = meta("")
        calendar_box.addWidget(self.month_hint)
        box.addLayout(calendar_box)
        self.time_row = QBoxLayout(QBoxLayout.LeftToRight)
        self.time_row.setSpacing(16)
        night = QVBoxLayout()
        night.setSpacing(8)
        night.addWidget(field_label(tr("Night window")))
        self.window_start, self.window_end = QTimeEdit(), QTimeEdit()
        self.window_controls = QWidget()
        controls = QHBoxLayout(self.window_controls)
        controls.setContentsMargins(0, 0, 0, 0)
        for edit, name in ((self.window_start, "Начало окна"), (self.window_end, "Конец окна")):
            edit.setDisplayFormat("HH:mm")
            edit.setAccessibleName(tr(name))
            controls.addWidget(edit)
        controls.addStretch(1)
        preview = WindowPreview(self.window_start, self.window_end)
        preview.clicked.connect(
            lambda: self.window_controls.setVisible(self.window_controls.isHidden())
        )
        night.addWidget(preview)
        night.addWidget(self.window_controls)
        self.window_controls.hide()
        self.time_row.addLayout(night, 1)
        zone = QVBoxLayout()
        zone.setSpacing(8)
        zone.addWidget(field_label(tr("Site time zone")))
        self.time_zone = QComboBox()
        self.time_zone.addItem(tr("Авто — по домену сайта"), AUTO_ZONE)
        for value in time_zones():
            self.time_zone.addItem(value, value)
        self.time_zone.setAccessibleName(tr("Часовой пояс"))
        self.time_zone_field = CanvasField(self.time_zone, "public")
        zone.addWidget(self.time_zone_field)
        self.zone_note = meta("")
        zone.addWidget(self.zone_note)
        zone.addStretch(1)
        self.time_row.addLayout(zone, 1)
        box.addLayout(self.time_row)
        left.addWidget(frame)
        frame, box = card(
            "Competitors",
            "groups",
            "warning",
            subtitle=meta(tr("Include linked sites in the same schedule.")),
        )
        self.competitors = CanvasSwitch(tr("Include competitors"))
        frame.heading.addWidget(self.competitors)
        self.competitor_note = meta(
            tr("Each run uses the core's stored competitor list and scan limits.")
        )
        box.addWidget(self.competitor_note)
        left.addWidget(frame)
        self.slice_hint = meta("")
        frame, box = card("Big site, slice by slice", "splitscreen", "ok", subtitle=self.slice_hint)
        self.slice_title = frame.heading.itemAt(1).layout().itemAt(0).widget()
        frame.heading.itemAt(1).layout().setSpacing(4)
        self.slice = CanvasSwitch(tr("Slice mode"))
        frame.heading.addWidget(self.slice)
        left.addWidget(frame)
        left.addStretch(1)
        side = QWidget()
        side.setObjectName("scheduleColumn")
        right = QVBoxLayout(side)
        right.setContentsMargins(0, 0, 0, 0)
        right.setSpacing(16)
        frame, box = card("Next runs")
        self.next_run = meta("")
        self.next_run.setProperty("text_style", "section")
        self.next_run.setObjectName("scheduleNextDate")
        self.next_run.setFixedHeight(20)
        self.next_host = meta(host_name or "")
        self.next_host.setProperty("text_style", "control")
        row = QHBoxLayout()
        row.setSpacing(12)
        row.setAlignment(Qt.AlignVCenter)
        date_box = QVBoxLayout()
        date_box.setSpacing(4)
        self.next_run.setAlignment(Qt.AlignCenter)
        self.next_run.setFixedWidth(60)
        self.next_calendar = meta("")
        self.next_calendar.setAlignment(Qt.AlignCenter)
        self.next_calendar.setFixedHeight(17)
        date_box.addWidget(self.next_run)
        date_box.addWidget(self.next_calendar)
        row.setContentsMargins(0, 9, 0, 9)
        row.addLayout(date_box)
        detail = QVBoxLayout()
        detail.addWidget(self.next_host)
        self.next_limits = meta("")
        detail.addWidget(self.next_limits)
        row.addLayout(detail, 1)
        box.addLayout(row)
        right.addWidget(frame)
        self.wide_only = []
        frame, box = card("History")
        self.history_card = frame
        self.last_run = meta("")
        box.addWidget(self.last_run)
        right.addWidget(frame)
        code_card = CodeCard(
            "seohead schedule-show\nseohead schedule-set\nseohead schedule-run-due",
            "# same settings from the terminal or an agent",
        )
        right.addWidget(code_card)
        self.wide_only.append(code_card)
        info = Note(
            "info",
            "The Mac stays awake only while a scan runs.",
            "The app can live in the menu bar; runs happen without an open window.",
            icon="dark_mode",
        )
        info.setProperty("canvas_notice", True)
        info.layout().setContentsMargins(16, 16, 16, 16)
        info.layout().itemAt(1).widget().setText(
            "The Mac stays awake only while a scan runs. The app can live in the menu bar; runs happen without an open window."
        )
        info.layout().itemAt(1).widget().setTextFormat(Qt.PlainText)
        right.addWidget(info)
        self.wide_only.append(info)
        right.addStretch(1)
        layout.addWidget(Columns(primary, side))

    def resizeEvent(self, event):
        super().resizeEvent(event)
        compact = self.width() < 1000
        self.time_row.setDirection(QBoxLayout.TopToBottom if compact else QBoxLayout.LeftToRight)
        self.time_row.setSpacing(6 if compact else 16)
        self.zone_note.setVisible(not compact)
        self.competitor_note.setVisible(not compact)
        self.enabled_label.setVisible(not compact)
        self.enabled_frame.setProperty("compact", compact)
        self.enabled_frame.setFixedHeight(26 if compact else 44)
        self.enabled_frame.layout().setContentsMargins(*(0, 0, 0, 0) if compact else (16, 4, 10, 4))
        self.enabled_frame.style().unpolish(self.enabled_frame)
        self.enabled_frame.style().polish(self.enabled_frame)
        labels = (
            ("Daily", "Week days", "Month days", "Manual")
            if compact
            else ("Every day", "Days of week", "Days of month", "Manual only")
        )
        for button, label in zip(self.frequency._buttons.values(), labels):
            button.setText(label)
        self.frequency.update_widths()
        self.slice_title.setText("Slice by slice" if compact else "Big site, slice by slice")
        if self.state:
            limit = self.state["settings"]["crawl"]["max_urls"]
            self.slice_hint.setText(
                f"Up to {limit:,} URL a night"
                + ("" if compact else "; the next night continues where it stopped.")
            )
        for widget in self.wide_only:
            widget.setVisible(not compact)
        self.history_card.setVisible(not compact and bool(self.state and self.state["last_run"]))
        self.header.meta.setProperty("canvas_header_meta", not compact)
        self.header.meta.setFixedHeight(19 if compact else 22)
        polish(self.header.meta)
        if self.state:
            self.header.set_meta(
                f"Next: {next_run_text(self.state['next_run'])}"
                if self.width() < 1000
                else self.subtitle
            )

    def _frequency_changed(self, value):
        if value in {"daily", "month"}:
            for check in self.weekdays.values():
                check.setChecked(False)
        if value in {"daily", "week"}:
            self.month_days.clear()
        if value == "manual":
            self.enabled.setChecked(False)
        self._calendar_visibility()

    def _calendar_visibility(self):
        value = self.frequency.value()
        self.week_widget.setVisible(value == "week")
        monthly = value == "month" or bool(self.month_days.text().strip())
        self.month_widget.setVisible(monthly)
        self.month_hint.setVisible(monthly)
        self.calendar_label.setText(tr("Days of month" if monthly else "Days"))
        self.calendar_label.setVisible(value in {"week", "month"} or monthly)

    def _month_clicked(self):
        self.month_days.setText(
            ", ".join(str(day) for day, button in self.month_buttons.items() if button.isChecked())
        )

    def _month_text_changed(self, value):
        selected = {int(part.strip()) for part in value.split(",") if part.strip().isdigit()}
        for day, button in self.month_buttons.items():
            button.setChecked(day in selected)

        def ordinal(day):
            return f"{day}{'th' if 10 < day % 100 < 14 else {1: 'st', 2: 'nd', 3: 'rd'}.get(day % 10, 'th')}"

        picked = [ordinal(day) for day in sorted(selected)]
        self.month_hint.setText(
            "Picked: " + (" and ".join(picked) if len(picked) <= 2 else ", ".join(picked)) + "."
        )

    def fill(self, settings):
        self.enabled.setChecked(settings["enabled"])
        for name, check in self.weekdays.items():
            check.setChecked(name in settings["weekdays"])
        self.month_days.setText(", ".join(str(d) for d in settings["month_days"]))
        for edit, key in ((self.window_start, "start"), (self.window_end, "end")):
            hour, minute = (int(part) for part in settings["window"][key].split(":"))
            edit.setTime(QTime(hour, minute))
        self.time_zone.setCurrentIndex(max(0, self.time_zone.findData(settings["time_zone"])))
        self.competitors.setChecked(settings["include_competitors"])
        self.slice.setChecked(settings["crawl"]["slice"])
        self.frequency.setValue(
            "week" if settings["weekdays"] else "month" if settings["month_days"] else "daily"
        )
        self._calendar_visibility()

    def apply(self, settings):
        days = []
        for part in self.month_days.text().replace(";", ",").split(","):
            part = part.strip()
            if part:
                if not part.isdigit():
                    raise ValueError("month days")
                days.append(int(part))
        settings.update(
            {
                "enabled": self.enabled.isChecked(),
                "weekdays": [name for name, check in self.weekdays.items() if check.isChecked()],
                "month_days": sorted(set(days)),
                "window": {
                    "start": self.window_start.time().toString("HH:mm"),
                    "end": self.window_end.time().toString("HH:mm"),
                },
                "time_zone": self.time_zone.currentData(),
                "include_competitors": self.competitors.isChecked(),
            }
        )
        settings["crawl"]["slice"] = self.slice.isChecked()
        return settings

    def _show_state(self, state):
        self.status.setText(summary_text(state))
        self.status.hide()
        self.header.set_meta(
            f"Next: {next_run_text(state['next_run'])}" if self.width() < 1000 else self.subtitle
        )
        self.next_run.setText(
            state["next_run"]
            if state["settings"]["enabled"] and state["next_run"]
            else tr("Not scheduled")
        )
        self.last_run.setText(
            state["last_run"]["at"] if state["last_run"] else tr("No scheduled runs yet")
        )
        crawl = state["settings"]["crawl"]
        from .work import project_names

        _, host_name = project_names(self.host)
        mode_text = {"spider": "full crawl", "sitemap": "from sitemap", "list": "key URLs"}[
            crawl["mode"]
        ]
        self.site_host.setText(f"{host_name or ''} · {mode_text}")
        self.next_host.setText(host_name or "")
        self.slice_hint.setText(
            trf(
                "Up to {limit} URL a night; the next night continues where it stopped.",
                limit=f"{crawl['max_urls']:,}",
            )
        )
        next_date = None
        try:
            next_date = datetime.strptime(state["next_run"], "%d.%m.%Y %H:%M")
        except (TypeError, ValueError):
            self.next_calendar.setText("")
        else:
            self.next_run.setText(str(next_date.day))
            months = (
                "JAN",
                "FEB",
                "MAR",
                "APR",
                "MAY",
                "JUN",
                "JUL",
                "AUG",
                "SEP",
                "OCT",
                "NOV",
                "DEC",
            )
            days = ("MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN")
            self.next_calendar.setText(
                f"{months[next_date.month - 1]} · {days[next_date.weekday()]}"
            )
        clock = (
            next_date.strftime("%H:%M")
            if next_date and state["settings"]["enabled"]
            else "Not scheduled"
        )
        self.next_limits.setText(
            f"{clock} · up to {crawl['max_urls']:,} URL · {1 / crawl['min_delay_seconds']:g} req/s"
        )
        self.history_card.setVisible(self.width() >= 1000 and bool(state["last_run"]))
        self.zone_note.setText(
            trf("Сейчас для расписания используется пояс {zone}", zone=state["zone"])
            if state.get("zone")
            else ""
        )


class CrawlPage(_SettingsPage):
    title = "Обход и User-Agent"
    subtitle = "What to scan, who to introduce yourself as, and how gently. Applies to manual and scheduled scans."

    def build(self, layout):
        self.mode = QComboBox()
        for key, label in MODES.items():
            self.mode.addItem(tr(label), key)
        self.mode.setAccessibleName("Crawl mode")
        self.mode.currentIndexChanged.connect(self._mode_changed)
        self.mode.hide()
        mode_block = QWidget()
        mode_block.setObjectName("scheduleColumn")
        mode_box = QVBoxLayout(mode_block)
        mode_box.setContentsMargins(0, 0, 0, 0)
        mode_box.setSpacing(8)
        mode_label = field_label("What to scan")
        mode_label.setFixedHeight(20)
        mode_box.addWidget(mode_label)
        choices = QWidget()
        choices.setObjectName("scheduleColumn")
        self.mode_layout = QBoxLayout(QBoxLayout.LeftToRight, choices)
        self.mode_layout.setContentsMargins(0, 0, 0, 0)
        self.mode_layout.setSpacing(12)
        self.mode_buttons = {}
        self.mode_group = QButtonGroup(self)
        modes = (
            (
                "spider",
                "Whole site by links",
                "From the start page, like a search bot",
                "hub",
                "info",
            ),
            ("sitemap", "From sitemap", "Only what the site declares", "account_tree", "ok"),
            (
                "list",
                "List of key URLs",
                "For million-page sites: only the pages that matter",
                "checklist",
                "warning",
            ),
        )
        for key, title, description, icon, tone in modes:
            button = ModeCard(title, description, icon, tone)
            button.clicked.connect(
                lambda _checked, v=key: self.mode.setCurrentIndex(self.mode.findData(v))
            )
            self.mode_group.addButton(button)
            self.mode_buttons[key] = button
            button.wide_description = description
            self.mode_layout.addWidget(button, 1)
        mode_box.addWidget(choices)
        self.sitemap_row = meta("Sitemap address")
        self.sitemap_url = QLineEdit()
        self.sitemap_url.setPlaceholderText("https://example.test/sitemap.xml")
        self.sitemap_url.setAccessibleName("Sitemap address")
        self.urls_row = meta("Key URLs · one per line")
        self.urls_row.setContentsMargins(0, 6, 0, 0)
        self.urls_row.setFixedHeight(26)
        self.urls = QPlainTextEdit()
        self.urls.setAccessibleName("Key URLs")
        self.urls.setFixedHeight(106)
        self.urls.setProperty("mono", True)
        self.urls.textChanged.connect(
            lambda: self.urls_row.setText(
                f"Key URLs · {len([line for line in self.urls.toPlainText().splitlines() if line.strip()])}"
            )
        )
        for widget in (self.sitemap_row, self.sitemap_url, self.urls_row, self.urls):
            mode_box.addWidget(widget)
        layout.addWidget(mode_block)
        identity, box = card("Introduce yourself as", "badge")
        self.preset = QComboBox()
        labels = {
            "spiderhead": "SpiderHeadBot · default",
            "googlebot-smartphone": "Googlebot smartphone",
            "googlebot-desktop": "Googlebot desktop",
            "chrome-desktop": "Chrome",
            "safari-mobile": "Safari iPhone",
            "custom": "Custom string",
        }
        for key, label in USER_AGENT_PRESETS.items():
            self.preset.addItem(tr(labels.get(key, label)), key)
        self.preset.setAccessibleName("User-Agent preset")
        self.preset.currentIndexChanged.connect(self._preset_changed)
        self.preset_field = CanvasField(self.preset, "pest_control", "role:primary")
        box.addWidget(self.preset_field)
        self.preset_grid = QWidget()
        self.preset_grid.setObjectName("scheduleColumn")
        presets = QVBoxLayout(self.preset_grid)
        presets.setContentsMargins(0, 0, 0, 0)
        presets.setSpacing(8)
        self.preset_buttons = {}
        self.preset_group = QButtonGroup(self)
        self.overflow_buttons = []
        groups = (
            ("SPIDERHEAD", ("spiderhead",)),
            (
                "SEARCH BOTS",
                (
                    "googlebot-smartphone",
                    "googlebot-desktop",
                    "yandexbot",
                    "bingbot",
                    "applebot",
                    "googlebot",
                    "yandexmobilebot",
                    "baiduspider",
                    "duckduckbot",
                ),
            ),
            (
                "AI BOTS · BROWSERS",
                (
                    "gptbot",
                    "claudebot",
                    "perplexitybot",
                    "chrome-desktop",
                    "safari-mobile",
                    "firefox-desktop",
                    "safari-desktop",
                    "edge-desktop",
                    "chrome-mobile",
                    "custom",
                ),
            ),
        )
        for title, keys in groups:
            label = meta(title)
            label.setProperty("text_style", "overline")
            label.setContentsMargins(0, 4, 0, 0)
            label.setFixedHeight(22)
            presets.addWidget(label)
            buttons = []
            visible_count = 1 if title == "SPIDERHEAD" else 5
            menu = QMenu(self)
            overflow = CanvasPreset()
            overflow.setProperty("canvas_component", "preset")
            overflow.setPopupMode(QToolButton.InstantPopup)
            overflow.setMenu(menu)
            hidden_keys = []
            for index, key in enumerate(keys):
                button = CanvasPreset()
                button.setText(tr(labels.get(key, USER_AGENT_PRESETS[key])))
                button.setCheckable(True)
                button.setProperty("canvas_component", "preset")
                button.setProperty("custom_preset", key == CUSTOM)
                button.setSizePolicy(QSizePolicy.Fixed, QSizePolicy.Fixed)
                button.setAccessibleName(button.text())
                button.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
                button.setIconSize(QSize(16, 16))
                button.toggled.connect(
                    lambda selected, b=button, custom=key == CUSTOM: b.setIcon(
                        material_icon("check", "role:on_selected")
                        if selected
                        else material_icon("edit", "role:primary")
                        if custom
                        else material_icon("")
                    )
                )
                if key == CUSTOM:
                    button.setIcon(material_icon("edit", "role:primary"))
                button.clicked.connect(
                    lambda _checked, v=key: self.preset.setCurrentIndex(self.preset.findData(v))
                )
                self.preset_group.addButton(button)
                self.preset_buttons[key] = button
                if index < visible_count or key == CUSTOM:
                    buttons.append(button)
                else:
                    button.hide()
                    hidden_keys.append(key)
                    action = menu.addAction(button.text())
                    action.triggered.connect(
                        lambda _checked, v=key: self.preset.setCurrentIndex(self.preset.findData(v))
                    )
            if hidden_keys:
                overflow.setText(f"+{len(hidden_keys)} more")
                self.overflow_buttons.append((overflow, hidden_keys))
                buttons.append(overflow)
            presets.addWidget(ChipFlow(buttons))
        box.addWidget(self.preset_grid)
        self.preset_hint = meta(
            "Presets: SpiderHead · search bots · AI bots · browsers · custom string"
        )
        box.addWidget(self.preset_hint)
        self.custom = QLineEdit()
        self.custom.setPlaceholderText("Full User-Agent string")
        self.custom.setAccessibleName("Custom User-Agent")
        self.custom.textChanged.connect(self._update_preview)
        box.addWidget(self.custom)
        code = CodeCard("", "User-Agent preset", variant="agent")
        self.identity_preview = code.text
        box.addWidget(code)
        self.identity_note = meta(
            "Your own bot is easy to find and filter in server logs and analytics."
        )
        self.identity_note_row = QWidget()
        note = QHBoxLayout(self.identity_note_row)
        note.setContentsMargins(0, 0, 0, 0)
        note.setSpacing(8)
        note.addWidget(MaterialIconLabel("info", 18, color="role:primary"), 0, Qt.AlignTop)
        note.addWidget(self.identity_note, 1, Qt.AlignTop)
        box.addWidget(self.identity_note_row)
        politeness, box = card("Politeness", "eco", "ok")
        row = QHBoxLayout()
        row.addWidget(field_label("Minimum delay"), 1)
        self.delay_text = QLabel()
        row.addWidget(self.delay_text)
        speed = QVBoxLayout()
        self.speed_layout = speed
        speed.setSpacing(8)
        speed.addLayout(row)
        self.delay = QDoubleSpinBox()
        self.delay.setRange(*DELAY_RANGE)
        self.delay.setDecimals(1)
        self.delay.setSingleStep(0.5)
        self.delay.hide()
        self.delay_text.setText("0.5 s · ≈ 2 req/s")
        self.delay.setAccessibleName("Minimum delay")
        self.delay_slider = QSlider(Qt.Horizontal)
        self.delay_slider.setRange(5, 600)
        self.delay_slider.setAccessibleName("Minimum delay")
        self.delay_slider.setFixedHeight(22)
        self.delay_slider.valueChanged.connect(lambda value: self.delay.setValue(value / 10))
        self.delay.valueChanged.connect(lambda value: self.delay_slider.setValue(round(value * 10)))
        self.delay.valueChanged.connect(
            lambda value: self.delay_text.setText(f"{value:g} s · ≈ {1 / value:g} req/s")
        )
        speed.addWidget(self.delay_slider)
        self.delay_ticks = QWidget()
        ticks = QHBoxLayout(self.delay_ticks)
        ticks.setContentsMargins(0, 0, 0, 0)
        ticks.addWidget(meta(f"{DELAY_RANGE[0]:g} s"))
        ticks.addStretch(1)
        ticks.addWidget(meta("gentle"))
        ticks.addStretch(1)
        ticks.addWidget(meta(f"{DELAY_RANGE[1]:g} s"))
        speed.addWidget(self.delay_ticks)
        box.addLayout(speed)
        self.budget = QSpinBox()
        self.budget.setRange(*BUDGET_RANGE)
        self.budget.setSingleStep(500)
        self.budget.setAccessibleName("URL limit per scan")
        self.requests = QSpinBox()
        self.requests.setRange(1, BUDGET_RANGE[1] * 10)
        self.requests.setSingleStep(500)
        self.requests.setAccessibleName("Requests in total")
        limits = QHBoxLayout()
        limits.setSpacing(10)
        for title, control in (
            ("URL limit per scan", self.budget),
            ("Requests in total", self.requests),
        ):
            frame = QFrame()
            frame.setProperty("canvas_component", "mini")
            column = QVBoxLayout(frame)
            column.setContentsMargins(14, 12, 14, 12)
            column.setSpacing(2)
            label = meta(title)
            if control is self.budget:
                self.budget_label = label
            label.setFixedHeight(19)
            column.addWidget(label)
            control.setProperty("canvas_value", True)
            control.setLocale(QLocale(QLocale.English, QLocale.UnitedStates))
            control.setButtonSymbols(QAbstractSpinBox.NoButtons)
            control.setGroupSeparatorShown(True)
            control.setFixedHeight(20)
            column.addWidget(control)
            limits.addWidget(frame, 1)
        box.addLayout(limits)
        self.policy_rows = []
        for text in ("Respect robots.txt and Crawl-delay", "Back off on 429 and 503"):
            holder = QWidget()
            row = QHBoxLayout(holder)
            row.setContentsMargins(0, 0, 0, 0)
            row.setSpacing(10)
            row.addWidget(
                MaterialIconLabel("check_circle", 18, color="role:canvas_success", filled=True)
            )
            row.addWidget(QLabel(text), 1)
            box.addWidget(holder)
            self.policy_rows.append(holder)
        self.slice = CanvasSwitch("Continue unfinished crawl in later windows")
        self.slice_row = QWidget()
        row = QHBoxLayout(self.slice_row)
        row.setContentsMargins(0, 0, 0, 0)
        row.addWidget(meta("Continue an unfinished crawl in later windows"), 1)
        row.addWidget(self.slice)
        box.addWidget(self.slice_row)
        layout.addWidget(Columns(identity, politeness, ratio=(7, 5)))
        self._mode_changed(0)
        self._preset_changed(0)

    def _update_preview(self):
        key = self.preset.currentData()
        agent = {
            "preset": DEFAULT_PRESET if key == CUSTOM else key,
            "custom": self.custom.text() if key == CUSTOM else None,
        }
        self.identity_preview.setText(tr(user_agent_text(agent)))

    def resizeEvent(self, event):
        if hasattr(self, "mode_layout"):
            compact = self.width() < 1000
            self.mode_layout.setDirection(
                QBoxLayout.TopToBottom if compact else QBoxLayout.LeftToRight
            )
            self.preset_grid.setVisible(not compact)
            self.preset_field.setVisible(compact)
            self.preset_hint.setVisible(compact)
            self.identity_note_row.setVisible(not compact)
            self.policy_rows[1].setVisible(not compact)
            self.delay_ticks.setVisible(not compact)
            self.speed_layout.setSpacing(14 if compact else 8)
            self.budget_label.setText("URL limit" if compact else "URL limit per scan")
            for control in (self.budget, self.requests):
                control.setProperty("canvas_compact", compact)
                polish(control)
            self.header.meta.setVisible(not compact)
            self.header.meta.setProperty("canvas_header_meta", not compact)
            self.header.meta.setFixedHeight(19 if compact else 22)
            polish(self.header.meta)
            self.layout().setSpacing(16)
            self.mode_layout.setSpacing(8 if compact else 12)
            descriptions = {
                "spider": "Like a search bot",
                "sitemap": "Only what the site declares",
                "list": "Paste one URL per line",
            }
            for key, button in self.mode_buttons.items():
                size = 32 if compact else 40
                button.tile.setFixedSize(size, size)
                button.icon_label.setFixedSize(18 if compact else 20, 18 if compact else 20)
                button.description.setText(
                    descriptions[key] if compact else button.wide_description
                )
                button.title.setProperty("text_style", "control")
                button.setProperty("compact", compact)
                button.update_margins()
                polish(button)
        super().resizeEvent(event)

    def _mode_changed(self, _index):
        mode = self.mode.currentData()
        self.sitemap_row.setVisible(mode == "sitemap")
        self.sitemap_url.setVisible(mode == "sitemap")
        self.urls_row.setVisible(mode == "list")
        self.urls.setVisible(mode == "list")
        self.slice_row.hide()
        if hasattr(self, "mode_buttons"):
            self.mode_buttons[mode].setChecked(True)

    def _preset_changed(self, _index):
        key = self.preset.currentData()
        self.custom.setEnabled(key == CUSTOM)
        self.custom.setVisible(key == CUSTOM)
        if hasattr(self, "preset_buttons"):
            self.preset_buttons[key].setChecked(True)
            for button, keys in self.overflow_buttons:
                button.setText(self.preset.currentText() if key in keys else f"+{len(keys)} more")
        self._update_preview()

    def fill(self, settings):
        crawl, agent = settings["crawl"], settings["user_agent"]
        self.mode.setCurrentIndex(max(0, self.mode.findData(crawl["mode"])))
        self.sitemap_url.setText(crawl["sitemap_url"] or "")
        self.urls.setPlainText("\n".join(crawl["urls"]))
        self.slice.setChecked(crawl["slice"])
        if agent["custom"] is not None:
            self.preset.setCurrentIndex(self.preset.findData(CUSTOM))
            self.custom.setText(agent["custom"])
        else:
            self.preset.setCurrentIndex(max(0, self.preset.findData(agent["preset"])))
            self.custom.setText("")
        self.delay.setValue(float(crawl["min_delay_seconds"]))
        self.budget.setValue(int(crawl["max_urls"]))
        self.requests.setValue(int(crawl["max_requests"]))
        self._mode_changed(0)
        self._preset_changed(0)

    def apply(self, settings):
        key = self.preset.currentData()
        urls = [line.strip() for line in self.urls.toPlainText().splitlines() if line.strip()]
        settings["crawl"] = {
            **settings["crawl"],
            "mode": self.mode.currentData(),
            "sitemap_url": self.sitemap_url.text().strip() or None,
            "urls": urls,
            "slice": self.slice.isChecked(),
            "max_urls": self.budget.value(),
            "max_requests": self.requests.value(),
            "min_delay_seconds": round(self.delay.value(), 1),
        }
        if key == CUSTOM:
            settings["user_agent"] = {
                "preset": DEFAULT_PRESET,
                "custom": self.custom.text().strip(),
            }
        else:
            settings["user_agent"] = {"preset": key, "custom": None}
        return settings
