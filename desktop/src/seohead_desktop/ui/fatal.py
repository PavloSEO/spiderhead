"""Fatal states: the core or the project folder cannot serve data. One card per real condition, never a sample.

``fatal_conditions`` reads only facts the window already has (core path, project folder, free space, write access).
Conditions the core does not report yet (core version compatibility, scan database integrity) are shown as neutral
``waiting_badge`` pills, not as a guess.
"""

from __future__ import annotations

import os
import shutil
from dataclasses import dataclass

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QFrame, QGridLayout, QHBoxLayout, QLabel, QPushButton, QVBoxLayout

from ..i18n import tr, trf
from .icons import MaterialIconLabel
from .kit import waiting_badge
from .presentation import ElidedLabel

MIN_FREE_BYTES = 10 * 1024**3  # the scan runner keeps the same floor (storage.min_free_bytes)
CORE_COMPATIBILITY_ISSUE = 979  # core version and compatibility report
SCAN_INTEGRITY_ISSUE = 1163  # per-scan integrity status without a full open


@dataclass(frozen=True)
class FatalCondition:
    key: str      # "core_missing" | "disk_low" | "no_write_access"
    icon: str     # Material Symbols name
    tone: str     # "error" | "warning": colours the icon tile and the icon
    title: str
    text: str
    mono: str     # the real path or code the condition is about, copyable, shown as is


def _tilde(path):
    home = os.path.expanduser("~")
    return "~" + path[len(home):] if path == home or path.startswith(home + os.sep) else path


def _gigabytes(size):
    return f"{size / 1024**3:.1f}".replace(".", ",") + " ГБ"


def fatal_conditions(core_executable, project_directory, *, free_bytes=None, writable=None):
    """Real blocking conditions in display order. ``free_bytes``/``writable`` default to measuring the project folder."""
    out = []
    if not core_executable or not os.path.isfile(core_executable):
        out.append(FatalCondition(
            "core_missing", "link_off", "error", tr("Ядро не найдено"),
            tr("Нет исполняемого seohead. Сканы и проекты целы — оболочке не с чем работать."),
            core_executable or tr("seohead не найден в PATH")))
    if project_directory:
        if free_bytes is None:
            try:
                free_bytes = shutil.disk_usage(project_directory).free
            except OSError:
                free_bytes = None
        if writable is None:
            writable = os.access(project_directory, os.W_OK)
        if free_bytes is not None and free_bytes < MIN_FREE_BYTES:
            out.append(FatalCondition(
                "disk_low", "hard_drive", "warning", tr("Диск заполнен"),
                trf("Свободно {free} — меньше порога {limit}. Данные проекта не удаляются автоматически.",
                    free=_gigabytes(free_bytes), limit="10 ГБ"),
                _tilde(project_directory)))
        if not writable:
            out.append(FatalCondition(
                "no_write_access", "folder_off", "error", tr("Нет прав на папку"),
                tr("Нельзя записать в папку проекта. Обычно macOS не выдала доступ к этой папке."),
                _tilde(project_directory)))
    return out


class FatalCard(QFrame):
    """Icon tile, title, one-line explanation, mono detail and at most two actions (primary first)."""

    def __init__(self, condition, actions=(), parent=None):
        super().__init__(parent)
        self.setProperty("fatal_card", "true")
        self.setProperty("fatal", condition.key)
        self.setMinimumWidth(260)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(20, 20, 20, 20)
        layout.setSpacing(10)
        tile = QFrame()
        tile.setProperty("fatal_icon", condition.tone)
        tile.setFixedSize(44, 44)
        tile_layout = QHBoxLayout(tile)
        tile_layout.setContentsMargins(0, 0, 0, 0)
        tile_layout.addWidget(MaterialIconLabel(condition.icon, 24, color=f"role:{condition.tone}"), 0, Qt.AlignCenter)
        layout.addWidget(tile)
        title = QLabel(condition.title)
        title.setProperty("text_style", "section")
        title.setWordWrap(True)
        layout.addWidget(title)
        text = QLabel(condition.text)
        text.setProperty("text_style", "meta")
        text.setWordWrap(True)
        layout.addWidget(text)
        mono = ElidedLabel(condition.mono)
        mono.setProperty("text_style", "mono")
        mono.setProperty("fatal_mono", "true")
        mono.setToolTip(condition.mono)
        mono.setTextInteractionFlags(Qt.TextSelectableByMouse)
        layout.addWidget(mono)
        layout.addStretch(1)
        row = QHBoxLayout()
        row.setSpacing(8)
        for index, (label, callback) in enumerate(actions[:2]):
            button = QPushButton(tr(label))
            if index == 0:
                button.setProperty("role", "primary")
            button.clicked.connect(callback)
            row.addWidget(button)
        row.addStretch(1)
        layout.addLayout(row)


class WaitingCard(QFrame):
    """Checks the core does not report yet: an honest neutral pill per check, never a guessed value."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setProperty("fatal_card", "true")
        self.setProperty("fatal", "waiting")
        self.setMinimumWidth(260)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(20, 20, 20, 20)
        layout.setSpacing(10)
        tile = QFrame()
        tile.setProperty("fatal_icon", "waiting")
        tile.setFixedSize(44, 44)
        tile_layout = QHBoxLayout(tile)
        tile_layout.setContentsMargins(0, 0, 0, 0)
        tile_layout.addWidget(MaterialIconLabel("schedule", 24, color="role:text_muted"), 0, Qt.AlignCenter)
        layout.addWidget(tile)
        title = QLabel(tr("Совместимость ядра и база скана"))
        title.setProperty("text_style", "section")
        title.setWordWrap(True)
        layout.addWidget(title)
        text = QLabel(tr("Версию ядра и целостность баз сканов приложение пока не получает. Эти состояния не показаны, пока ядро их не сообщит."))
        text.setProperty("text_style", "meta")
        text.setWordWrap(True)
        layout.addWidget(text)
        for issue, label in ((CORE_COMPATIBILITY_ISSUE, "Версия ядра"), (SCAN_INTEGRITY_ISSUE, "Целостность базы скана")):
            row = QHBoxLayout()
            name = QLabel(tr(label))
            name.setProperty("text_style", "meta")
            row.addWidget(name)
            row.addStretch(1)
            row.addWidget(waiting_badge(issue))
            layout.addLayout(row)
        layout.addStretch(1)


def fill_grid(grid, cards, width):
    """Place cards into an existing grid as many columns as the width allows (3 at 1440, 2 at ~960, 1 at the 800 minimum)."""
    while grid.count():
        grid.takeAt(0)  # detaches only; the widgets stay alive
    columns = 3 if width >= 1040 else 2 if width >= 700 else 1
    for index, card in enumerate(cards):
        grid.addWidget(card, index // columns, index % columns)
    for column in range(3):
        grid.setColumnStretch(column, 1 if column < columns else 0)


def clear_layout(layout):
    while layout.count():
        item = layout.takeAt(0)
        if item.widget() is not None:
            item.widget().deleteLater()
        elif item.layout() is not None:
            clear_layout(item.layout())
