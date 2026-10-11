"""«Расписание» and «Обход» tabs of «Настройки проекта».

Both tabs read the project's schedule through the core (schedule_core) and write it back with the revision they read.
The desktop keeps no copy: after a save the page shows what the core stored. When the core cannot answer (no CLI, or a
core without these commands) the tab shows the neutral waiting state and its controls stay disabled.
"""

from __future__ import annotations

import copy

from PyQt5.QtCore import QTime
from PyQt5.QtWidgets import (
    QCheckBox,
    QComboBox,
    QDoubleSpinBox,
    QFrame,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPlainTextEdit,
    QPushButton,
    QSpinBox,
    QTimeEdit,
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
    validate,
)
from ..ui.icons import material_icon
from ..ui.kit import PageHeader, StatePanel


def card(title):
    frame = QFrame()
    frame.setProperty("card", "panel")
    layout = QVBoxLayout(frame)
    layout.setContentsMargins(16, 14, 16, 14)
    layout.setSpacing(10)
    caption = QLabel(tr(title).upper())
    caption.setProperty("text_style", "overline")
    layout.addWidget(caption)
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
    last_text = f"{last['at']} · {tr('ok') if last['status'] == 'ok' else tr('с ошибкой')}" if last and last.get("at") else tr("ещё не запускалось")
    return trf("Следующий запуск: {next} · Последний: {last}", next=state["next_run"] or tr("не запланирован"), last=last_text)


class _SettingsPage(QWidget):
    """Loads the whole schedule document, changes one section, and saves it with the revision it read."""

    def __init__(self, host):
        super().__init__()
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
        self.body = QVBoxLayout(self.form_widget)
        self.body.setContentsMargins(0, 0, 0, 0)
        self.body.setSpacing(12)
        self.build(self.body)
        outer.addWidget(self.form_widget)
        footer = QHBoxLayout()
        footer.addStretch(1)
        self.revert = QPushButton(tr("Отменить"))
        self.revert.clicked.connect(self.reload)
        self.save_button = QPushButton(tr("Сохранить"))
        self.save_button.setProperty("role", "primary")
        self.save_button.setIcon(material_icon("save", "#FFFFFF"))
        self.save_button.clicked.connect(self.save)
        footer.addWidget(self.revert)
        footer.addWidget(self.save_button)
        outer.addLayout(footer)
        outer.addStretch(1)
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

    def reload(self):
        executable, directory = self._executable(), self._directory()
        if not executable or not directory:
            self.state = None
            self._set_available(False)
            self.status.setText(tr("Откройте проект с установленным ядром, чтобы менять расписание."))
            return
        self.status.setText(tr("Читаем настройки проекта из ядра…"))
        self.jobs.submit(lambda: read_schedule(executable, directory), self._loaded, self._failed, self)

    def _loaded(self, state):
        self.state = state
        self.fill(state["settings"])
        self._set_available(True)
        self._show_state(state)

    def _failed(self, text):
        self.state = None
        self._set_available(False)
        self.status.setText(text)

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
        self.jobs.submit(lambda: save_schedule(executable, directory, revision, settings), self._saved, self._save_failed, self)

    def _saved(self, state):
        self.state = state
        self.fill(state["settings"])
        self._show_state(state)

    def _save_failed(self, text):
        self.status.setText(trf("Не сохранено: {reason}", reason=text))


class SchedulePage(_SettingsPage):
    title = "Расписание сканов"
    subtitle = "запуски по расписанию из файлов проекта"

    def build(self, layout):
        frame, box = card("Расписание")
        self.enabled = QCheckBox(tr("Запускать сканы по расписанию"))
        box.addWidget(self.enabled)
        box.addWidget(meta(tr("Дни недели · пусто — каждый день")))
        days = QHBoxLayout()
        days.setSpacing(6)
        self.weekdays = {}
        for name in DAYS:
            check = QCheckBox(tr(DAY_LABELS[name]))
            self.weekdays[name] = check
            days.addWidget(check)
        days.addStretch(1)
        box.addLayout(days)
        box.addWidget(meta(tr("Числа месяца · через запятую, пусто — не выбирать")))
        self.month_days = QLineEdit()
        self.month_days.setPlaceholderText(tr("например 1, 15"))
        self.month_days.setAccessibleName(tr("Числа месяца"))
        box.addWidget(self.month_days)
        window = QHBoxLayout()
        window.setSpacing(8)
        self.window_start, self.window_end = QTimeEdit(), QTimeEdit()
        for edit, name in ((self.window_start, "Начало окна"), (self.window_end, "Конец окна")):
            edit.setDisplayFormat("HH:mm")
            edit.setAccessibleName(tr(name))
        window.addWidget(meta(tr("Окно запуска, с")))
        window.addWidget(self.window_start)
        window.addWidget(meta(tr("до")))
        window.addWidget(self.window_end)
        window.addStretch(1)
        box.addLayout(window)
        box.addWidget(meta(tr("Часовой пояс")))
        self.time_zone = QComboBox()
        self.time_zone.addItem(tr("Авто — по домену сайта"), AUTO_ZONE)
        for zone in time_zones():
            self.time_zone.addItem(zone, zone)
        self.time_zone.setAccessibleName(tr("Часовой пояс"))
        box.addWidget(self.time_zone)
        self.zone_note = meta("")
        box.addWidget(self.zone_note)
        self.competitors = QCheckBox(tr("Сканировать конкурентов по тому же расписанию"))
        box.addWidget(self.competitors)
        layout.addWidget(frame)

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

    def apply(self, settings):
        days = []
        for part in self.month_days.text().replace(";", ",").split(","):
            part = part.strip()
            if part:
                if not part.isdigit():
                    raise ValueError("month days")
                days.append(int(part))
        settings.update({
            "enabled": self.enabled.isChecked(),
            "weekdays": [name for name, check in self.weekdays.items() if check.isChecked()],
            "month_days": sorted(set(days)),
            "window": {"start": self.window_start.time().toString("HH:mm"), "end": self.window_end.time().toString("HH:mm")},
            "time_zone": self.time_zone.currentData(),
            "include_competitors": self.competitors.isChecked(),
        })
        return settings

    def _show_state(self, state):
        self.status.setText(summary_text(state))
        self.zone_note.setText(trf("Сейчас для расписания используется пояс {zone}", zone=state["zone"]) if state.get("zone") else "")


class CrawlPage(_SettingsPage):
    title = "Обход и User-Agent"
    subtitle = "как запускается обход по расписанию и как сайт видит сканер"

    def build(self, layout):
        frame, box = card("Режим обхода")
        self.mode = QComboBox()
        for key, label in MODES.items():
            self.mode.addItem(tr(label), key)
        self.mode.setAccessibleName(tr("Режим обхода"))
        self.mode.currentIndexChanged.connect(self._mode_changed)
        box.addWidget(self.mode)
        self.sitemap_url = QLineEdit()
        self.sitemap_url.setPlaceholderText(tr("https://example.com/sitemap.xml"))
        self.sitemap_url.setAccessibleName(tr("Адрес sitemap"))
        self.sitemap_row = meta(tr("Адрес sitemap"))
        box.addWidget(self.sitemap_row)
        box.addWidget(self.sitemap_url)
        self.urls = QPlainTextEdit()
        self.urls.setPlaceholderText(tr("по одному URL в строке, до 500"))
        self.urls.setAccessibleName(tr("Список URL"))
        self.urls_row = meta(tr("Важные URL"))
        box.addWidget(self.urls_row)
        box.addWidget(self.urls)
        self.slice = QCheckBox(tr("Продолжать неполный обход в следующие ночи"))
        box.addWidget(self.slice)
        layout.addWidget(frame)

        frame, box = card("User-Agent")
        self.preset = QComboBox()
        for key, label in USER_AGENT_PRESETS.items():
            self.preset.addItem(tr(label), key)
        self.preset.setAccessibleName(tr("Пресет User-Agent"))
        self.preset.currentIndexChanged.connect(self._preset_changed)
        self.custom = QLineEdit()
        self.custom.setPlaceholderText(tr("Строка User-Agent целиком"))
        self.custom.setAccessibleName(tr("Свой User-Agent"))
        box.addWidget(self.preset)
        box.addWidget(self.custom)
        box.addWidget(meta(tr("SpiderHeadBot попадает в логи сайта, и его можно фильтровать.")))
        layout.addWidget(frame)

        frame, box = card("Вежливость")
        row = QHBoxLayout()
        row.setSpacing(12)
        self.delay = QDoubleSpinBox()
        self.delay.setRange(*DELAY_RANGE)
        self.delay.setDecimals(1)
        self.delay.setSingleStep(0.5)
        self.delay.setSuffix(tr(" с между запросами"))
        self.delay.setAccessibleName(tr("Пауза между запросами"))
        self.budget = QSpinBox()
        self.budget.setRange(*BUDGET_RANGE)
        self.budget.setSingleStep(500)
        self.budget.setSuffix(tr(" URL за запуск"))
        self.budget.setAccessibleName(tr("Лимит URL за запуск"))
        row.addWidget(meta(tr("Пауза")))
        row.addWidget(self.delay)
        row.addSpacing(12)
        row.addWidget(meta(tr("Бюджет")))
        row.addWidget(self.budget)
        row.addStretch(1)
        box.addLayout(row)
        layout.addWidget(frame)

    def _mode_changed(self, _index):
        mode = self.mode.currentData()
        self.sitemap_row.setVisible(mode == "sitemap")
        self.sitemap_url.setVisible(mode == "sitemap")
        self.urls_row.setVisible(mode == "list")
        self.urls.setVisible(mode == "list")
        self.slice.setVisible(mode == "spider")

    def _preset_changed(self, _index):
        self.custom.setEnabled(self.preset.currentData() == CUSTOM)

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
            "min_delay_seconds": round(self.delay.value(), 1),
        }
        if key == CUSTOM:
            settings["user_agent"] = {"preset": DEFAULT_PRESET, "custom": self.custom.text().strip()}
        else:
            settings["user_agent"] = {"preset": key, "custom": None}
        return settings
