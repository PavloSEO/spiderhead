"""«Расписание» and «Обход» tabs of «Настройки проекта».

Both tabs read the project's schedule through the core (schedule_core) and write it back with the revision they read.
The desktop keeps no copy: after a save the page shows what the core stored. When the core cannot answer (no CLI, or a
core without these commands) the tab shows the neutral waiting state and its controls stay disabled.
"""

from __future__ import annotations

import copy

from PyQt5.QtCore import QRectF, Qt, QTime, pyqtSignal
from PyQt5.QtGui import QColor, QPainter, QPen
from PyQt5.QtWidgets import (
    QBoxLayout,
    QButtonGroup,
    QComboBox,
    QDoubleSpinBox,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QLineEdit,
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

from .. import theming
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
from ..ui.controls import Segmented, Switch
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import PageHeader, StatePanel


class Columns(QWidget):
    """Two panels at wide widths, the same panels stacked at compact widths."""

    def __init__(self, *panels):
        super().__init__()
        self.setObjectName("scheduleColumns")
        self.box = QBoxLayout(QBoxLayout.LeftToRight, self)
        self.box.setContentsMargins(0, 0, 0, 0)
        self.box.setSpacing(16)
        for index, panel in enumerate(panels):
            self.box.addWidget(panel, 3 if index == 0 else 2)

    def resizeEvent(self, event):
        self.box.setDirection(
            QBoxLayout.TopToBottom if self.width() < 960 else QBoxLayout.LeftToRight
        )
        super().resizeEvent(event)


class CalendarDay(QToolButton):
    """Native checkable day with the circular calendar treatment."""

    def __init__(self, text, parent=None):
        super().__init__(parent)
        self.setText(text)
        self.setCheckable(True)
        self.setFixedSize(40, 40)
        self.setCursor(Qt.PointingHandCursor)

    def paintEvent(self, event):
        painter = QPainter(self)
        painter.setRenderHint(QPainter.Antialiasing)
        roles = theming.roles()
        selected = self.isChecked()
        painter.setPen(QPen(QColor(roles["primary"] if selected else roles["outline"]), 1))
        painter.setBrush(QColor(roles["primary"] if selected else roles["base"]))
        painter.drawEllipse(QRectF(1, 1, 38, 38))
        painter.setPen(QColor(roles["on_primary"] if selected else roles["text_2"]))
        painter.drawText(self.rect(), Qt.AlignCenter, self.text())
        if self.hasFocus():
            painter.setPen(QPen(QColor(roles["ring"]), 2))
            painter.setBrush(Qt.NoBrush)
            painter.drawEllipse(QRectF(3, 3, 34, 34))


class WindowPreview(QWidget):
    """Draw the selected clock interval; calendar decisions remain in the core."""

    clicked = pyqtSignal()

    def __init__(self, start, end):
        super().__init__()
        self.start, self.end = start, end
        self.setMinimumHeight(72)
        self.setCursor(Qt.PointingHandCursor)
        self.setFocusPolicy(Qt.StrongFocus)
        self.setAccessibleName(tr("Change night window"))
        for edit in (start, end):
            edit.timeChanged.connect(self.update)

    def mouseReleaseEvent(self, event):
        if event.button() == Qt.LeftButton:
            self.clicked.emit()
        super().mouseReleaseEvent(event)

    def keyPressEvent(self, event):
        if event.key() in {Qt.Key_Space, Qt.Key_Return}:
            self.clicked.emit()
        else:
            super().keyPressEvent(event)

    def paintEvent(self, event):
        painter = QPainter(self)
        painter.setRenderHint(QPainter.Antialiasing)
        roles = theming.roles()
        width = self.width()
        painter.setPen(Qt.NoPen)
        painter.setBrush(QColor(roles["raised"]))
        painter.drawRoundedRect(QRectF(0, 0, width, 44), 10, 10)
        start, end = (
            edit.time().msecsSinceStartOfDay() / 86400000 for edit in (self.start, self.end)
        )
        painter.setBrush(QColor(roles["primary"]))
        if end > start:
            painter.drawRoundedRect(QRectF(start * width, 7, (end - start) * width, 30), 6, 6)
        elif end < start:
            painter.drawRect(QRectF(start * width, 7, (1 - start) * width, 30))
            painter.drawRect(QRectF(0, 7, end * width, 30))
        painter.setPen(QColor(roles["text"]))
        text = f"{self.start.time().toString('HH:mm')} – {self.end.time().toString('HH:mm')}"
        painter.drawText(QRectF(0, 0, width, 44), Qt.AlignCenter, text)
        painter.setPen(QColor(roles["text_3"]))
        for hour in (0, 6, 12, 18, 24):
            x = min(width - 22, hour / 24 * width)
            painter.drawText(QRectF(x, 50, 24, 20), Qt.AlignLeft, f"{hour:02d}")


def card(title, icon=None, tone="info", subtitle=None):
    frame = QFrame()
    frame.setProperty("card", "panel")
    frame.setProperty("schedule_panel", True)
    layout = QVBoxLayout(frame)
    layout.setContentsMargins(20, 18, 20, 18)
    layout.setSpacing(14)
    heading = QHBoxLayout()
    heading.setSpacing(12)
    if icon:
        tile = QFrame()
        tile.setProperty("schedule_icon", tone)
        tile.setFixedSize(40, 40)
        tile_layout = QVBoxLayout(tile)
        tile_layout.setContentsMargins(0, 0, 0, 0)
        tile_layout.addWidget(MaterialIconLabel(icon, 20, color="role:text"), 0, Qt.AlignCenter)
        heading.addWidget(tile)
    texts = QVBoxLayout()
    texts.setContentsMargins(0, 0, 0, 0)
    texts.setSpacing(4)
    caption = QLabel(tr(title))
    caption.setProperty("text_style", "section")
    texts.addWidget(caption)
    if subtitle is not None:
        texts.addWidget(subtitle)
    heading.addLayout(texts, 1)
    layout.addLayout(heading)
    frame.heading = heading
    return frame, layout


def meta(text):
    label = QLabel(text)
    label.setProperty("text_style", "meta")
    label.setWordWrap(True)
    return label


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
        outer.setSpacing(10)
        self.header = PageHeader(self.title, self.subtitle)
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
        self.body.setSpacing(12)
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
        self.save_button.setIcon(material_icon("save", "#FFFFFF"))
        self.save_button.clicked.connect(self.save)
        self.header.add_action(self.revert)
        self.header.add_action(self.save_button)
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
        self.save_button.setEnabled(available and self.state is not None)
        self.revert.setEnabled(available)
        self.availabilityChanged.emit(available)

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
        self.jobs.submit(
            lambda: read_schedule(executable, directory), self._loaded, self._failed, self
        )

    def _loaded(self, state):
        self.state = state
        self.fill(state["settings"])
        self._set_available(True)
        self._show_state(state)

    def _failed(self, text):
        self.state = None
        self._set_available(False)
        self.status.setText(text)
        self.status.show()

    def _show_state(self, state):
        self.status.setText("")

    def save(self):
        if self.state is None:
            return
        try:
            settings = validate(self.apply(copy.deepcopy(self.state["settings"])))
        except ValueError:
            self.status.setText(tr("Проверьте значения: часть полей заполнена неверно."))
            return
        executable, directory = self._executable(), self._directory()
        revision = self.state["revision"]
        self.status.setText(tr("Сохраняем в ядро…"))
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

    def _save_failed(self, text):
        self.status.setText(trf("Не сохранено: {reason}", reason=text))
        self.status.show()


class SchedulePage(_SettingsPage):
    title = "Расписание сканов"
    subtitle = "запуски по расписанию из файлов проекта"

    def build(self, layout):
        from .work import project_names
        from ..ui.settings.listing import terminal
        from ..ui.controls import Note

        _, host_name = project_names(self.host)
        primary = QWidget()
        primary.setObjectName("scheduleColumn")
        left = QVBoxLayout(primary)
        left.setContentsMargins(0, 0, 0, 0)
        left.setSpacing(16)
        self.enabled = Switch(tr("Schedule enabled"))
        enabled = QFrame()
        enabled.setProperty("schedule_enabled", True)
        enabled_box = QHBoxLayout(enabled)
        enabled_box.setContentsMargins(16, 8, 10, 8)
        enabled_box.setSpacing(12)
        enabled_box.addWidget(QLabel(tr("Enabled")))
        enabled_box.addWidget(self.enabled)
        self.header.add_action(enabled)
        self.site_host = meta(host_name or "")
        frame, box = card("Your site", "language", subtitle=self.site_host)
        box.addWidget(meta(tr("How often")))
        self.frequency = Segmented(
            [("daily", tr("Every day")), ("week", tr("Days of week")),
             ("month", tr("Days of month")), ("manual", tr("Manual only"))],
            "daily", tr("Frequency"))
        self.frequency.setObjectName("scheduleFrequency")
        self.frequency.layout().setContentsMargins(0, 0, 0, 0)
        self.frequency.layout().setSpacing(0)
        buttons = list(self.frequency._buttons.values())
        for index, button in enumerate(buttons):
            button.setProperty("schedule_segment", "first" if index == 0 else "last" if index == len(buttons)-1 else "middle")
        self.frequency.changed.connect(self._frequency_changed)
        box.addWidget(self.frequency, 0, Qt.AlignLeft)
        self.calendar_label = meta(tr("Days"))
        box.addWidget(self.calendar_label)
        self.week_widget = QWidget()
        self.week_widget.setObjectName("scheduleCalendar")
        days = QHBoxLayout(self.week_widget)
        days.setContentsMargins(0, 0, 0, 0)
        days.setSpacing(8)
        self.weekdays = {}
        for name, label in zip(DAYS, ("Mo", "Tu", "We", "Th", "Fr", "Sa", "Su")):
            check = CalendarDay(tr(label))
            check.setAccessibleName(tr(DAY_LABELS[name]))
            self.weekdays[name] = check
            days.addWidget(check)
        days.addStretch(1)
        box.addWidget(self.week_widget)
        self.month_widget = QWidget()
        self.month_widget.setObjectName("scheduleCalendar")
        grid = QGridLayout(self.month_widget)
        grid.setContentsMargins(0, 0, 0, 0)
        grid.setSpacing(8)
        self.month_buttons = {}
        for day in range(1,32):
            button = CalendarDay(str(day))
            button.setAccessibleName(trf("Day {day}", day=day))
            button.clicked.connect(self._month_clicked)
            self.month_buttons[day] = button
            grid.addWidget(button,(day-1)//7,(day-1)%7)
        grid.setColumnStretch(7,1)
        box.addWidget(self.month_widget)
        self.month_days = QLineEdit()
        self.month_days.textChanged.connect(self._month_text_changed)
        self.month_days.hide()
        self.month_hint = meta("")
        box.addWidget(self.month_hint)
        self.time_row = QBoxLayout(QBoxLayout.LeftToRight)
        self.time_row.setSpacing(16)
        night = QVBoxLayout()
        night.setSpacing(8)
        night.addWidget(meta(tr("Night window")))
        self.window_start, self.window_end = QTimeEdit(), QTimeEdit()
        self.window_controls = QWidget()
        controls = QHBoxLayout(self.window_controls)
        controls.setContentsMargins(0,0,0,0)
        for edit,name in ((self.window_start,"Начало окна"),(self.window_end,"Конец окна")):
            edit.setDisplayFormat("HH:mm")
            edit.setAccessibleName(tr(name))
            controls.addWidget(edit)
        controls.addStretch(1)
        preview = WindowPreview(self.window_start,self.window_end)
        preview.clicked.connect(lambda: self.window_controls.setVisible(self.window_controls.isHidden()))
        night.addWidget(preview)
        night.addWidget(self.window_controls)
        self.window_controls.hide()
        self.time_row.addLayout(night,1)
        zone = QVBoxLayout()
        zone.setSpacing(8)
        zone.addWidget(meta(tr("Site time zone")))
        self.time_zone = QComboBox()
        self.time_zone.addItem(tr("Авто — по домену сайта"),AUTO_ZONE)
        for value in time_zones(): self.time_zone.addItem(value,value)
        self.time_zone.setAccessibleName(tr("Часовой пояс"))
        self.time_zone.setObjectName("scheduleZone")
        zone.addWidget(self.time_zone)
        self.zone_note = meta("")
        zone.addWidget(self.zone_note)
        zone.addStretch(1)
        self.time_row.addLayout(zone,1)
        box.addLayout(self.time_row)
        left.addWidget(frame)
        frame,box = card("Competitors","groups","warning",subtitle=meta(tr("Include linked sites in the same schedule.")))
        self.competitors = Switch(tr("Include competitors"))
        frame.heading.addWidget(self.competitors)
        box.addWidget(meta(tr("Each run uses the core's stored competitor list and scan limits.")))
        left.addWidget(frame)
        self.slice_hint = meta("")
        frame,box = card("Big site, slice by slice","splitscreen","ok",subtitle=self.slice_hint)
        self.slice = Switch(tr("Slice mode"))
        frame.heading.addWidget(self.slice)
        left.addWidget(frame)
        left.addStretch(1)
        side = QWidget()
        side.setObjectName("scheduleColumn")
        right = QVBoxLayout(side)
        right.setContentsMargins(0,0,0,0)
        right.setSpacing(16)
        frame,box = card("Next runs")
        self.next_run = meta("")
        self.next_run.setProperty("text_style","section")
        self.next_run.setObjectName("scheduleNextDate")
        self.next_host = meta(host_name or "")
        row = QHBoxLayout()
        row.setSpacing(12)
        row.addWidget(self.next_run)
        detail = QVBoxLayout()
        detail.addWidget(self.next_host)
        self.next_limits = meta("")
        detail.addWidget(self.next_limits)
        row.addLayout(detail,1)
        box.addLayout(row)
        right.addWidget(frame)
        frame,box = card("History")
        self.last_run = meta("")
        box.addWidget(self.last_run)
        right.addWidget(frame)
        right.addWidget(terminal("seohead schedule-show\nseohead schedule-set\nseohead schedule-run-due", []))
        right.addWidget(Note("info",tr("The Mac stays awake only while a scan runs."),tr("Tray mode can run without an open window."),icon="dark_mode"))
        right.addStretch(1)
        layout.addWidget(Columns(primary,side))

    def resizeEvent(self,event):
        super().resizeEvent(event)
        self.time_row.setDirection(QBoxLayout.TopToBottom if self.width()<960 else QBoxLayout.LeftToRight)

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
        self.month_hint.setText(trf("Picked: {days}", days=", ".join(str(day) for day in sorted(selected))))

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
        self.site_host.setText(f'{host_name or ""} · {tr(MODES[crawl["mode"]])}')
        self.next_host.setText(host_name or "")
        self.slice_hint.setText(trf("Up to {limit} URL a night; the next night continues where it stopped.", limit=f'{crawl["max_urls"]:,}'))
        self.next_limits.setText(
            f"{crawl['max_urls']:,} URL · {crawl['min_delay_seconds']:g} s minimum delay"
        )
        self.zone_note.setText(
            trf("Сейчас для расписания используется пояс {zone}", zone=state["zone"])
            if state.get("zone")
            else ""
        )


class CrawlPage(_SettingsPage):
    title = "Обход и User-Agent"
    subtitle = "как запускается обход по расписанию и как сайт видит сканер"

    def build(self, layout):
        self.mode = QComboBox()
        for key, label in MODES.items():
            self.mode.addItem(tr(label), key)
        self.mode.setAccessibleName(tr("Режим обхода"))
        self.mode.currentIndexChanged.connect(self._mode_changed)
        self.mode.hide()
        layout.addWidget(meta(tr("What to scan")))
        choices = QWidget()
        self.mode_layout = QBoxLayout(QBoxLayout.LeftToRight, choices)
        self.mode_layout.setContentsMargins(0, 0, 0, 0)
        self.mode_layout.setSpacing(12)
        self.mode_buttons = {}
        self.mode_group = QButtonGroup(self)
        modes = [
            ("spider", "Whole site by links", "hub"),
            ("sitemap", "From sitemap", "account_tree"),
            ("list", "List of key URLs", "checklist"),
        ]
        for key, title, icon in modes:
            button = QToolButton()
            button.setText(tr(title))
            button.setIcon(material_icon(icon, "role:primary"))
            button.setToolButtonStyle(Qt.ToolButtonTextUnderIcon)
            button.setCheckable(True)
            button.setProperty("card", "choice")
            button.setMinimumHeight(70)
            button.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Preferred)
            button.setAccessibleName(tr(title))
            button.clicked.connect(
                lambda _checked, v=key: self.mode.setCurrentIndex(self.mode.findData(v))
            )
            self.mode_group.addButton(button)
            self.mode_buttons[key] = button
            self.mode_layout.addWidget(button, 1)
        layout.addWidget(choices)
        self.sitemap_row = meta(tr("Sitemap address"))
        self.sitemap_url = QLineEdit()
        self.sitemap_url.setPlaceholderText("https://example.test/sitemap.xml")
        self.sitemap_url.setAccessibleName(tr("Адрес sitemap"))
        self.urls_row = meta(tr("Key URLs · one per line"))
        self.urls = QPlainTextEdit()
        self.urls.setPlaceholderText(tr("по одному URL в строке, до 500"))
        self.urls.setAccessibleName(tr("Список URL"))
        self.urls.setMinimumHeight(106)
        self.urls.setMaximumHeight(180)
        for widget in (self.sitemap_row, self.sitemap_url, self.urls_row, self.urls):
            layout.addWidget(widget)
        identity, box = card("Introduce yourself as")
        self.preset = QComboBox()
        for key, label in USER_AGENT_PRESETS.items():
            self.preset.addItem(tr(label), key)
        self.preset.setAccessibleName(tr("Пресет User-Agent"))
        self.preset.currentIndexChanged.connect(self._preset_changed)
        box.addWidget(self.preset)
        self.preset_grid = QWidget()
        grid = QGridLayout(self.preset_grid)
        grid.setContentsMargins(0, 0, 0, 0)
        grid.setSpacing(8)
        self.preset_buttons = {}
        self.preset_group = QButtonGroup(self)
        for index, (key, label) in enumerate(USER_AGENT_PRESETS.items()):
            button = QToolButton()
            button.setText(tr(label))
            button.setCheckable(True)
            button.setProperty("segment", True)
            button.setAccessibleName(tr(label))
            button.clicked.connect(
                lambda _checked, v=key: self.preset.setCurrentIndex(self.preset.findData(v))
            )
            self.preset_group.addButton(button)
            self.preset_buttons[key] = button
            grid.addWidget(button, index // 3, index % 3)
        box.addWidget(self.preset_grid)
        self.custom = QLineEdit()
        self.custom.setPlaceholderText(tr("Строка User-Agent целиком"))
        self.custom.setAccessibleName(tr("Свой User-Agent"))
        self.custom.textChanged.connect(self._update_preview)
        box.addWidget(self.custom)
        self.identity_preview = meta("")
        self.identity_preview.setProperty("text_style", "mono")
        self.identity_preview.setTextInteractionFlags(Qt.TextSelectableByMouse)
        box.addWidget(self.identity_preview)
        box.addWidget(meta(tr("The core resolves the selected identity when the crawl starts.")))
        politeness, box = card("Politeness")
        box.addWidget(meta(tr("Minimum delay")))
        self.delay = QDoubleSpinBox()
        self.delay.setRange(*DELAY_RANGE)
        self.delay.setDecimals(1)
        self.delay.setSingleStep(0.5)
        self.delay.setSuffix(tr(" с между запросами"))
        self.delay.setAccessibleName(tr("Пауза между запросами"))
        self.delay_slider = QSlider(Qt.Horizontal)
        self.delay_slider.setRange(5, 600)
        self.delay_slider.setAccessibleName(tr("Minimum delay slider"))
        self.delay_slider.valueChanged.connect(lambda value: self.delay.setValue(value / 10))
        self.delay.valueChanged.connect(lambda value: self.delay_slider.setValue(round(value * 10)))
        box.addWidget(self.delay)
        box.addWidget(self.delay_slider)
        limits = QHBoxLayout()
        self.budget = QSpinBox()
        self.budget.setRange(*BUDGET_RANGE)
        self.budget.setSingleStep(500)
        self.budget.setAccessibleName(tr("Лимит URL за запуск"))
        self.requests = QSpinBox()
        self.requests.setRange(1, BUDGET_RANGE[1] * 10)
        self.requests.setSingleStep(500)
        self.requests.setAccessibleName(tr("Requests in total"))
        for title, control in (
            ("URL limit per scan", self.budget),
            ("Requests in total", self.requests),
        ):
            column = QVBoxLayout()
            column.addWidget(meta(tr(title)))
            column.addWidget(control)
            limits.addLayout(column, 1)
        box.addLayout(limits)
        self.slice = Switch(tr("Продолжать неполный обход в следующие ночи"))
        row = QHBoxLayout()
        row.addWidget(meta(tr("Continue an unfinished crawl in later windows")), 1)
        row.addWidget(self.slice)
        self.slice_row = QWidget()
        self.slice_row.setLayout(row)
        box.addWidget(self.slice_row)
        box.addWidget(meta(tr("Robots.txt and retry backoff follow the core crawl policy.")))
        layout.addWidget(Columns(identity, politeness))
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
            self.mode_layout.setDirection(
                QBoxLayout.TopToBottom if self.width() < 960 else QBoxLayout.LeftToRight
            )
            self.preset_grid.setVisible(self.width() >= 960)
        super().resizeEvent(event)

    def _mode_changed(self, _index):
        mode = self.mode.currentData()
        self.sitemap_row.setVisible(mode == "sitemap")
        self.sitemap_url.setVisible(mode == "sitemap")
        self.urls_row.setVisible(mode == "list")
        self.urls.setVisible(mode == "list")
        self.slice_row.setVisible(mode == "spider")
        if hasattr(self, "mode_buttons"):
            self.mode_buttons[mode].setChecked(True)

    def _preset_changed(self, _index):
        key = self.preset.currentData()
        self.custom.setEnabled(key == CUSTOM)
        self.custom.setVisible(key == CUSTOM)
        if hasattr(self, "preset_buttons"):
            self.preset_buttons[key].setChecked(True)
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

