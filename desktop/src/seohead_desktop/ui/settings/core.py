"""Settings → Ядро (sheet SetCore). Installation facts come from the application via ``context.request("core_info")``."""

from __future__ import annotations

import os
import shutil

from PyQt5.QtCore import QUrl
from PyQt5.QtGui import QDesktopServices
from PyQt5.QtWidgets import QHBoxLayout, QLabel, QLineEdit, QWidget

from ... import theming
from ...i18n import joined, trf
from ...settings_store import Setting
from ..controls import Note, SettingRow
from .helpers import group_label, keyed, page, segmented_row, switch_row
from .listing import (
    Columns,
    Mono,
    action_button,
    badge,
    buttons_row,
    hint,
    key_values,
    list_item,
    tilde,
)

ID, ICON, TITLE = "core", "memory", "Ядро"
HINT = "Приложение — оболочка над ядром seohead; CLI той же версии"

POLL_CHOICES = (("0.5", "0,5 с"), ("1", "1 с"), ("2", "2 с"))

SCHEMA = (
    Setting("core.custom", False, bool),
    Setting("core.custom_path", "", str),
    Setting("core.poll", "0.5", str, choices=tuple(v for v, _ in POLL_CHOICES)),
    Setting("core.log_level", "INFO", str, choices=("INFO", "DEBUG")),
)

# status -> (badge kind, badge text, icon, icon colour role)
STATUS = {
    "ok": ("ok", "ок", "check_circle", "success"),
    "warn": ("warn", "внимание", "warning", "warning"),
    "err": ("err", "ошибка", "cancel", "error"),
}


def _installation(context):
    info = context.request("core_info")
    info = info if isinstance(info, dict) else {}
    version = info.get("version")
    if version and info.get("commit"):
        version = f"{version} · {info['commit']}"
    compatible = info.get("compatible")
    compat = None if compatible is None else _compat_widget(compatible, info.get("required"))
    cli = shutil.which("seohead")
    process = joined(" · ", [p for p in (f"PID {info['pid']}" if info.get("pid") else "",
                                         trf("{n} МБ", n=info["memory_mb"]) if info.get("memory_mb") else "",
                                         trf("работает {uptime}", uptime=info["uptime"]) if info.get("uptime") else "") if p]) or None
    return key_values([
        ("Версия ядра", version),
        ("Совместимость", compat),
        ("Путь ядра", Mono(tilde(context.core_executable)) if context.core_executable else None),
        ("CLI", Mono(tilde(cli)) if cli else "Не найден в PATH"),
        ("Процесс", process),
    ])


def _compat_widget(compatible, required):
    box = QWidget()
    layout = QHBoxLayout(box)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.addWidget(badge("ok", "совместимо", "check_circle") if compatible else badge("err", "несовместимо", "cancel"))
    if required:
        layout.addWidget(QLabel(trf("нужно ≥ {required}", required=required)))
    return box


def _custom_path_row(store):
    field = QLineEdit(store.get("core.custom_path"))
    field.setFixedWidth(300)
    field.setCursorPosition(0)
    field.setProperty("mono", True)
    field.setPlaceholderText("/путь/к/.venv/bin/seohead")
    row = keyed(SettingRow("Путь к своему ядру", "Используется вместо найденного при следующем запуске приложения", field), "core.custom_path")
    field.editingFinished.connect(lambda: row.set_error(store.set("core.custom_path", field.text().strip())))
    row.setVisible(store.get("core.custom"))
    store.changed.connect(lambda key: _toggle(row, store) if key.startswith("core.custom") else None)
    return row


def _toggle(row, store):
    try:
        row.setVisible(store.get("core.custom"))
    except RuntimeError:  # page already deleted
        pass


def _diagnostics(context):
    result = context.request("diagnostics")
    result = result if isinstance(result, dict) else None
    run = action_button("Запустить", "stethoscope", enabled=context.can("run_diagnostics"))
    run.clicked.connect(lambda: context.request("run_diagnostics"))
    widgets = [group_label("Диагностика")]
    if result is None:
        widgets += [_header_row(hint("Нет данных о последней диагностике"), run)]
        return widgets
    items = result.get("items", [])
    counts = {status: sum(1 for i in items if i.get("status") == status) for status in STATUS}
    parts = [result.get("when", ""), trf("{n} ок", n=counts["ok"]), trf("{n} внимание", n=counts["warn"]), trf("{n} ошибка", n=counts["err"])]
    summary = hint(joined(" · ", [part for part in parts if part]))
    widgets.append(_header_row(summary, run))
    for item in items:
        kind, text, icon, color = STATUS.get(item.get("status"), STATUS["warn"])
        widgets.append(list_item(icon, item.get("title", ""), item.get("sub", ""), _plain_badge(kind, text),
                                 icon_color=theming.roles()[color]))
    if result.get("note"):
        widgets.append(Note("error", result["note"]))
    return widgets


def _header_row(text, button):
    row = QWidget()
    layout = QHBoxLayout(row)
    layout.setContentsMargins(0, 6, 0, 6)
    layout.addWidget(text, 1)
    layout.addWidget(button)
    return row


def _plain_badge(kind, text):
    label = QLabel(text)
    label.setProperty("badge", kind)
    label.setFixedHeight(theming.metrics()["control"]["badge"])
    return label


def _open_logs(context):
    path = context.log_directory
    if path and os.path.isdir(path):
        QDesktopServices.openUrl(QUrl.fromLocalFile(path))


def build_page(store, context):
    logs_ok = bool(context.log_directory and os.path.isdir(context.log_directory))
    open_logs = action_button("Папка логов", "folder_open", enabled=logs_ok)
    open_logs.clicked.connect(lambda: _open_logs(context))
    restart = action_button("Перезапустить ядро", role="text", enabled=context.can("restart_core"))
    restart.clicked.connect(lambda: context.request("restart_core"))
    left = [
        group_label("Установка"),
        _installation(context),
        switch_row(store, "core.custom", "Своё ядро", "Путь к другой установке seohead (разработка)"),
        _custom_path_row(store),
        segmented_row(store, "core.poll", "Опрос активного скана", "Интервал, пока есть активный запуск", list(POLL_CHOICES)),
        group_label("Логи"),
        segmented_row(store, "core.log_level", "Уровень логов", "DEBUG — только для разбора ошибок", [("INFO", "INFO"), ("DEBUG", "DEBUG")]),
        key_values([
            ("Папка логов", Mono(tilde(context.log_directory)) if context.log_directory else None),
            ("Размер", context.request("log_size")),
        ]),
        buttons_row(open_logs, restart),
    ]
    return page(Columns(left, _diagnostics(context)))
