"""«Расписание» and «Обход» tabs of «Настройки проекта».

Both tabs read the project's settings through the core (schedule_core) and write them back with the revision they read.
The desktop keeps no copy: after a save the page shows what the core stored. When the core cannot answer (no CLI, or a
core without these commands yet) the tab shows the neutral waiting state and its controls stay disabled.
"""

from __future__ import annotations

from PyQt5.QtCore import QTime, Qt
from PyQt5.QtWidgets import (
    QCheckBox,
    QComboBox,
    QDoubleSpinBox,
    QFrame,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QSpinBox,
    QTimeEdit,
    QVBoxLayout,
    QWidget,
)

from ..i18n import tr, trf
from ..schedule_core import (
    BUDGET_RANGE,
    FAILED,
    MODES,
    RPS_RANGE,
    USER_AGENT_PRESETS,
    WEEKDAYS,
    ScheduleJobs,
    read_schedule,
    save_schedule,
    time_zones,
    user_agent_text,
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


def stamp_text(value):
    return value or tr("не запланирован")


def summary_text(state):
    """One line for the project overview: next and last scheduled run, or why there is none."""
    if state is None:
        return tr("Расписание недоступно в этой версии ядра")
    if not state["settings"]["schedule"]["enabled"]:
        return tr("Расписание выключено")
    last = state["last_run"]
    last_text = f"{last['at']} · {last['status']}" if last and last.get("at") else tr("нет данных")
    return trf("Следующий запуск: {next} · Последний: {last}", next=stamp_text(state["next_run"]), last=last_text)


class _SettingsPage(QWidget):
    """Loads the whole settings document, lets one section change, and saves it with the revision it read."""

    section = ""

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
        self.body = QVBoxLayout()
        self.body.setSpacing(12)
        outer.addLayout(self.body)
        self.build(self.body)
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

    # Subclasses: title, subtitle, build(layout), fill(settings), collect() -> the section's dict.
    def build(self, layout):
        raise NotImplementedError

    def fill(self, settings):
        raise NotImplementedError

    def collect(self):
        raise NotImplementedError

    def _executable(self):
        return getattr(self.host, "core_executable", None)

    def _directory(self):
        return getattr(self.host, "project_directory", None)

    def _set_available(self, available):
        self.unavailable.setVisible(not available)
        self.save_button.setEnabled(available and self.state is not None)
        self.revert.setEnabled(available)
        self.form_widget.setEnabled(available)

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
            settings = validate({**self.state["settings"], self.section: self.collect()})
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
    section = "schedule"
    title = "Расписание сканов"
    subtitle = "запуски по расписанию из файлов проекта"

    def build(self, layout):
        self.form_widget = QWidget()
        form = QVBoxLayout(self.form_widget)
        form.setContentsMargins(0, 0, 0, 0)
        form.setSpacing(12)
        frame, box = card("Расписание")
        self.enabled = QCheckBox(tr("Запускать сканы по расписанию"))
        box.addWidget(self.enabled)
        days = QHBoxLayout()
        days.setSpacing(6)
        self.weekdays = []
        for index, name in enumerate(WEEKDAYS):
            check = QCheckBox(tr(name))
            self.weekdays.append(check)
            days.addWidget(check)
        days.addStretch(1)
        box.addWidget(meta(tr("Дни недели")))
        box.addLayout(days)
        self.days_of_month = QLineEdit()
        self.days_of_month.setPlaceholderText(tr("например 1, 15; пусто — не выбирать"))
        self.days_of_month.setAccessibleName(tr("Дни месяца"))
        box.addWidget(meta(tr("Числа месяца")))
        box.addWidget(self.days_of_month)
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
        self.time_zone = QComboBox()
        self.time_zone.setEditable(True)
        self.time_zone.addItems(time_zones())
        self.time_zone.setAccessibleName(tr("Часовой пояс"))
        box.addWidget(meta(tr("Часовой пояс сайта")))
        box.addWidget(self.time_zone)
        self.competitors = QCheckBox(tr("Сканировать конкурентов по тому же расписанию"))
        box.addWidget(self.competitors)
        form.addWidget(frame)
        layout.addWidget(self.form_widget)

    def fill(self, settings):
        schedule = settings["schedule"]
        self.enabled.setChecked(schedule["enabled"])
        for index, check in enumerate(self.weekdays):
            check.setChecked(index in schedule["weekdays"])
        self.days_of_month.setText(", ".join(str(d) for d in schedule["days_of_month"]))
        for edit, key in ((self.window_start, "window_start"), (self.window_end, "window_end")):
            hour, minute = (int(part) for part in schedule[key].split(":"))
            edit.setTime(QTime(hour, minute))
        self.time_zone.setCurrentText(schedule["time_zone"])
        self.competitors.setChecked(schedule["competitors"])

    def collect(self):
        days = []
        for part in self.days_of_month.text().replace(";", ",").split(","):
            part = part.strip()
            if part:
                if not part.isdigit():
                    raise ValueError("days of month")
                days.append(int(part))
        return {
            "enabled": self.enabled.isChecked(),
            "weekdays": [i for i, check in enumerate(self.weekdays) if check.isChecked()],
            "days_of_month": days,
            "window_start": self.window_start.time().toString("HH:mm"),
            "window_end": self.window_end.time().toString("HH:mm"),
            "time_zone": self.time_zone.currentText().strip(),
            "competitors": self.competitors.isChecked(),
        }

    def _show_state(self, state):
        self.status.setText(summary_text(state))


class CrawlPage(_SettingsPage):
    section = "crawl"
    title = "Обход и User-Agent"
    subtitle = "как запускается обход по расписанию и как сайт видит сканер"

    def build(self, layout):
        self.form_widget = QWidget()
        form = QVBoxLayout(self.form_widget)
        form.setContentsMargins(0, 0, 0, 0)
        form.setSpacing(12)
        frame, box = card("Режим обхода")
        self.mode = QComboBox()
        for key, label in MODES.items():
            self.mode.addItem(tr(label), key)
        self.mode.setAccessibleName(tr("Режим обхода"))
        box.addWidget(self.mode)
        form.addWidget(frame)

        frame, box = card("User-Agent")
        self.preset = QComboBox()
        for key, (label, _agent) in USER_AGENT_PRESETS.items():
            self.preset.addItem(tr(label), key)
        self.preset.setAccessibleName(tr("Пресет User-Agent"))
        self.preset.currentIndexChanged.connect(self._preset_changed)
        self.custom = QLineEdit()
        self.custom.setPlaceholderText(tr("Строка User-Agent целиком"))
        self.custom.setAccessibleName(tr("Свой User-Agent"))
        self.custom.textChanged.connect(self._preview)
        self.effective = QLabel()
        self.effective.setProperty("text_style", "mono")
        self.effective.setWordWrap(True)
        self.effective.setTextInteractionFlags(Qt.TextSelectableByMouse)
        box.addWidget(self.preset)
        box.addWidget(self.custom)
        box.addWidget(meta(tr("Так сайт видит сканер (SpiderHeadBot попадает в логи и его можно фильтровать):")))
        box.addWidget(self.effective)
        form.addWidget(frame)

        frame, box = card("Вежливость")
        row = QHBoxLayout()
        row.setSpacing(12)
        self.rps = QDoubleSpinBox()
        self.rps.setRange(*RPS_RANGE)
        self.rps.setDecimals(1)
        self.rps.setSingleStep(0.1)
        self.rps.setSuffix(tr(" запр/с"))
        self.rps.setAccessibleName(tr("Запросов в секунду"))
        self.budget = QSpinBox()
        self.budget.setRange(*BUDGET_RANGE)
        self.budget.setSingleStep(500)
        self.budget.setSuffix(tr(" URL за запуск"))
        self.budget.setAccessibleName(tr("Лимит URL за запуск"))
        row.addWidget(meta(tr("Скорость")))
        row.addWidget(self.rps)
        row.addSpacing(12)
        row.addWidget(meta(tr("Бюджет")))
        row.addWidget(self.budget)
        row.addStretch(1)
        box.addLayout(row)
        form.addWidget(frame)
        layout.addWidget(self.form_widget)

    def _preset_changed(self, _index):
        self.custom.setEnabled(self.preset.currentData() == "custom")
        self._preview()

    def _preview(self, *_args):
        key = self.preset.currentData()
        text = self.custom.text().strip() if key == "custom" else user_agent_text({"user_agent_preset": key})
        self.effective.setText(text or tr("Строка не задана"))

    def fill(self, settings):
        crawl = settings["crawl"]
        self.mode.setCurrentIndex(max(0, self.mode.findData(crawl["mode"])))
        self.preset.setCurrentIndex(max(0, self.preset.findData(crawl["user_agent_preset"])))
        self.custom.setText(crawl["user_agent"])
        self.rps.setValue(float(crawl["requests_per_second"]))
        self.budget.setValue(int(crawl["url_budget"]))
        self._preset_changed(0)

    def collect(self):
        key = self.preset.currentData()
        return {
            "mode": self.mode.currentData(),
            "user_agent_preset": key,
            "user_agent": self.custom.text().strip() if key == "custom" else "",
            "requests_per_second": round(self.rps.value(), 1),
            "url_budget": self.budget.value(),
        }
