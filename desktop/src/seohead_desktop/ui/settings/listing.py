"""Shared pieces of the Scan / Core / MCP sections: badges, list items, key-value tables, responsive columns."""

from __future__ import annotations

import os
from html import escape

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import (
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from ... import theming
from ...i18n import joined
from ..controls import KeyValue, polish
from ..icons import MaterialIconLabel, material_icon

UNAVAILABLE = "Недоступно в этой сборке"


class Mono(str):
    """A key-value cell rendered in the monospace font."""


def tilde(path):
    home = os.path.expanduser("~")
    return "~" + path[len(home):] if path and path.startswith(home) else path


def mono_html(text):
    return f"<span style=\"font-family:'{theming.substitution()['mono_family']}'\">{escape(text)}</span>"


def badge(kind, text, icon):
    """QLabel[badge] with a 16 px icon on its left (kind: ok | info | warn | err | mut | goal)."""
    label = QLabel(text)
    label.setProperty("badge", kind)
    label.setIndent(20)
    label.setFixedHeight(theming.metrics()["control"]["badge"])
    layout = QHBoxLayout(label)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.addWidget(MaterialIconLabel(icon, 16, color=theming.theme()["badges"][kind][1]))
    layout.addStretch(1)
    return label


def action_button(text, icon=None, role=None, size=None, enabled=True, tooltip=None):
    """Button whose action has no backend here is disabled with the 'unavailable' tooltip."""
    button = QPushButton(text)
    if icon:
        button.setIcon(material_icon(icon, theming.roles()["primary" if role in ("text", "tonal") else "text"]))
    if role:
        button.setProperty("role", role)
    if size:
        button.setProperty("size", size)
    button.setEnabled(enabled)
    if tooltip or not enabled:
        button.setToolTip(tooltip or UNAVAILABLE)
    return button


def buttons_row(*buttons):
    row = QWidget()
    layout = QHBoxLayout(row)
    layout.setContentsMargins(0, 10, 0, 6)
    layout.setSpacing(8)
    for button in buttons:
        layout.addWidget(button)
    layout.addStretch(1)
    return row


def hint(text, rich=False):
    label = QLabel(text)
    label.setProperty("text_style", "meta")
    label.setWordWrap(True)
    if rich:
        label.setTextFormat(Qt.RichText)
    label.setContentsMargins(0, 6, 0, 6)
    return label


def no_data(text="Нет данных"):
    label = QLabel(text)
    label.setProperty("na", True)
    label.setContentsMargins(0, 10, 0, 10)
    return label


def list_item(icon, title, sub="", *trailing, icon_color=None, title_mono=False, sub_mono=""):
    """One divider-separated list row: icon, title (+ sub line), trailing badges / buttons."""
    frame = QFrame()
    frame.setProperty("list_item", True)
    layout = QHBoxLayout(frame)
    layout.setContentsMargins(0, 8, 0, 8)
    layout.setSpacing(10)
    layout.addWidget(MaterialIconLabel(icon, 18, color=icon_color or theming.roles()["text_3"]), 0, Qt.AlignTop)
    text = QVBoxLayout()
    text.setSpacing(2)
    head = QLabel(title)
    head.setWordWrap(True)
    if title_mono:
        head.setProperty("text_style", "mono")
    text.addWidget(head)
    if sub or sub_mono:
        parts = ([mono_html(sub_mono)] if sub_mono else []) + ([escape(sub, quote=False)] if sub else [])
        line = QLabel(joined(" · ", parts))
        line.setTextFormat(Qt.RichText)
        line.setProperty("text_style", "meta")
        line.setWordWrap(True)
        text.addWidget(line)
    layout.addLayout(text, 1)
    for widget in trailing:
        layout.addWidget(widget, 0, Qt.AlignVCenter)
    return frame


def key_values(rows):
    """KeyValue table; a value may be None (Нет данных), str, Mono or a QWidget."""
    view = KeyValue([(key, "" if isinstance(value, QWidget) else value) for key, value in rows])
    grid = view.layout()
    for index, (_key, value) in enumerate(rows):
        label = grid.itemAtPosition(index, 1).widget()
        if isinstance(value, Mono):
            label.setProperty("text_style", "mono")
            polish(label)
        elif isinstance(value, QWidget):
            grid.removeWidget(label)
            label.deleteLater()
            cell = QFrame()
            cell.setProperty("kv_cell", True)
            cell_layout = QHBoxLayout(cell)
            cell_layout.setContentsMargins(0, 3, 0, 3)
            cell_layout.addWidget(value)
            cell_layout.addStretch(1)
            grid.addWidget(cell, index, 1)
    return view


def terminal(command, lines):
    """Read-only console panel; ``lines`` are plain strings."""
    frame = QFrame()
    frame.setProperty("terminal", True)
    layout = QVBoxLayout(frame)
    layout.setContentsMargins(14, 10, 14, 10)
    body = QLabel(joined("<br>", [f"~ $ {escape(command)}", *(escape(line, quote=False) for line in lines)]))
    body.setTextFormat(Qt.RichText)
    body.setWordWrap(True)
    body.setTextInteractionFlags(Qt.TextSelectableByMouse)
    layout.addWidget(body)
    return frame


class Columns(QWidget):
    """Two stacks of rows side by side; one column when narrower than ``breakpoint``."""

    def __init__(self, left, right, breakpoint=800):
        super().__init__()
        self._breakpoint = breakpoint
        self._narrow = None
        self._grid = QGridLayout(self)
        self._grid.setContentsMargins(0, 0, 0, 0)
        self._grid.setHorizontalSpacing(32)
        self._grid.setVerticalSpacing(0)
        self._columns = []
        for widgets in (left, right):
            box = QWidget()
            box_layout = QVBoxLayout(box)
            box_layout.setContentsMargins(0, 0, 0, 0)
            box_layout.setSpacing(0)
            for widget in widgets:
                box_layout.addWidget(widget)
            box_layout.addStretch(1)
            self._columns.append(box)
        self._place(False)

    def _place(self, narrow):
        self._narrow = narrow
        for box in self._columns:
            self._grid.removeWidget(box)
        self._grid.addWidget(self._columns[0], 0, 0)
        self._grid.addWidget(self._columns[1], 1 if narrow else 0, 0 if narrow else 1)
        self._grid.setColumnStretch(0, 1)
        self._grid.setColumnStretch(1, 0 if narrow else 1)

    def minimumSizeHint(self):
        """Narrow enough for one column: the wide layout must never force a sideways scroll."""
        hint = super().minimumSizeHint()
        hint.setWidth(max(box.minimumSizeHint().width() for box in self._columns))
        return hint

    def resizeEvent(self, event):
        super().resizeEvent(event)
        narrow = self.width() < self._breakpoint
        if narrow != self._narrow:
            self._place(narrow)

    def is_narrow(self):
        return bool(self._narrow)
