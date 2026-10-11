"""Settings → Уведомления (sheet SetNotify): event x channel matrix and quiet hours."""

from __future__ import annotations

import re

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QFrame, QHBoxLayout, QLabel, QLineEdit, QPushButton, QWidget

from ...i18n import trf
from ...settings_store import Setting
from ..controls import Note, SettingRow, Switch, polish
from ..icons import material_icon
from .helpers import keyed, page, switch_row, column_stack

ID, ICON, TITLE = "notify", "notifications", "Уведомления"
HINT = "Тосты в окне, системные уведомления и звук"

CHANNELS = (("w", "В окне"), ("s", "Системные"), ("z", "Звук"))
CHANNEL_NAMES = {"w": "в окне", "s": "системные", "z": "звук"}
# id, title, description, default (window, system, sound)
EVENTS = (
    ("done", "Скан завершён или остановлен", "С итогом и ссылкой", (True, True, False)),
    ("err", "Ошибка ядра или скана", "Остаётся до закрытия", (True, True, True)),
    ("agent", "Агент предложил задачи или цель", "Ссылка во «Входящие»", (True, False, False)),
    ("pay", "Агент просит платный запрос", "Нужно ваше «да»", (True, True, True)),
    ("stale", "Наблюдение устарело", "Только плашкой в окне", (True, False, False)),
    ("exp", "Экспорт и отчёт готовы", "С кнопкой «Открыть папку»", (True, False, False)),
    ("upd", "Доступно обновление", "Раз в день, не чаще", (True, False, False)),
)
COLUMN_WIDTH = 90
TIME_ERROR = "Время в формате ЧЧ:ММ"


class TimeSetting(Setting):
    """HH:MM, 24 hours; a single-digit hour is accepted and stored as two digits."""

    def validate(self, value):
        match = re.fullmatch(r"(\d{1,2}):(\d{2})", str(value).strip())
        if not match or int(match[1]) > 23 or int(match[2]) > 59:
            return False, value
        return True, f"{int(match[1]):02d}:{match[2]}"


def event_key(event_id, channel):
    return f"notify.{event_id}_{channel}"


SCHEMA = (
    *(Setting(event_key(event_id, channel), default[index], bool)
      for event_id, _t, _d, default in EVENTS for index, (channel, _label) in enumerate(CHANNELS)),
    Setting("notify.quiet", True, bool),
    TimeSetting("notify.quiet_from", "20:00", str, error=TIME_ERROR),
    TimeSetting("notify.quiet_to", "08:00", str, error=TIME_ERROR),
)


def _columns(widgets):
    """Three fixed-width cells so the header and every row line up."""
    box = QWidget()
    layout = QHBoxLayout(box)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(0)
    for widget in widgets:
        widget.setFixedWidth(COLUMN_WIDTH)
        layout.addWidget(widget)
    return box


def _header():
    head = QFrame()
    head.setProperty("table_head", True)
    layout = QHBoxLayout(head)
    layout.setContentsMargins(0, 8, 0, 6)
    layout.addWidget(_overline("Событие"), 1)
    layout.addWidget(_columns([_overline(label, Qt.AlignCenter) for _c, label in CHANNELS]))
    return head


def _overline(text, align=Qt.AlignLeft):
    label = QLabel(text.upper())
    label.setProperty("text_style", "overline")
    label.setAlignment(align | Qt.AlignVCenter)
    return label


def _event_row(store, event_id, title, description):
    cells = []
    for channel, _label in CHANNELS:
        key = event_key(event_id, channel)
        switch = Switch(trf("{title} — {channel}", title=title, channel=CHANNEL_NAMES[channel]), bool(store.get(key)))
        switch.toggled.connect(lambda checked, k=key: store.set(k, checked))
        cell = QWidget()
        cell_layout = QHBoxLayout(cell)
        cell_layout.setContentsMargins(0, 0, 0, 0)
        cell_layout.addWidget(switch, 0, Qt.AlignCenter)
        cells.append(cell)
    row = keyed(SettingRow(title, description, _columns(cells)), "notify." + event_id)
    row.layout().setContentsMargins(0, 7, 0, 7)
    row.layout().setHorizontalSpacing(0)
    return row


def _quiet_period_row(store):
    fields = {}
    box = QWidget()
    layout = QHBoxLayout(box)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(6)
    for index, (key, name) in enumerate((("notify.quiet_from", "Начало"), ("notify.quiet_to", "Конец"))):
        field = QLineEdit(store.get(key))
        field.setFixedWidth(72)
        field.setAccessibleName(name)
        fields[key] = field
        layout.addWidget(field)
        if index == 0:
            layout.addWidget(QLabel("—"))
    row = keyed(SettingRow("С … до", "", box), "notify.quiet_from")

    def commit(key):
        error = store.set(key, fields[key].text())
        if error is None:
            fields[key].setText(store.get(key))
        for other, field in fields.items():
            field.setProperty("invalid", bool(error) and other == key)
            polish(field)
        row.set_error(error)

    for key, field in fields.items():
        field.editingFinished.connect(lambda k=key: commit(k))
    row.fields = fields
    return row


def build_page(store, context):
    preview = QPushButton("Как выглядят уведомления")
    preview.setIcon(material_icon("notifications_active"))
    if context.can("show_notification_preview"):
        preview.clicked.connect(lambda: context.request("show_notification_preview"))
    else:
        preview.setEnabled(False)
        preview.setToolTip("Недоступно в этой сборке")
    preview_row = QWidget()
    preview_layout = QHBoxLayout(preview_row)
    preview_layout.setContentsMargins(0, 10, 0, 0)
    preview_layout.addWidget(preview)
    preview_layout.addStretch(1)

    left = [
        switch_row(store, "notify.quiet", "Тихие часы", "Без системных уведомлений и звука"),
        _quiet_period_row(store),
    ]
    right = [
        Note("info", "Что это меняет.", "Окно в фоне.<br>Тосты видны только в открытом окне; когда оно свёрнуто, срабатывают системные уведомления macOS."),
        preview_row,
    ]
    return page(_header(), *(_event_row(store, i, t, d) for i, t, d, _default in EVENTS), column_stack(left, right))
