"""Shared bits of the data / updates / about sections: honest action buttons, N/A key-value rows, responsive columns."""

from __future__ import annotations

import os
import shutil

from PyQt5.QtCore import QObject, QRunnable, Qt, pyqtSignal
from PyQt5.QtWidgets import QGridLayout, QHBoxLayout, QLabel, QPushButton, QVBoxLayout, QWidget

from ... import theming
from ...i18n import Num, tr, trf
from ..controls import KeyValue, polish
from ..icons import material_icon

UNAVAILABLE = "Недоступно в этой сборке"


def action_button(text, context, action, icon=None, role=None, parent=None):
    """Button wired to ``context.request(action)``; disabled with a tooltip when the app offers no such action."""
    button = QPushButton(text, parent)
    if role:
        button.setProperty("role", role)
    if icon:
        color = theming.roles()["error" if role == "danger" else "primary" if role == "text" else "text_2"]
        button.setIcon(material_icon(icon, color))
    if context.can(action):
        button.clicked.connect(lambda: context.request(action))
    else:
        button.setEnabled(False)
        button.setToolTip(UNAVAILABLE)
    return button


def button_row(*buttons):
    holder = QWidget()
    layout = QHBoxLayout(holder)
    layout.setContentsMargins(0, 10, 0, 10)
    layout.setSpacing(8)
    for button in buttons:
        layout.addWidget(button)
    layout.addStretch(1)
    return holder


def spacer(height):
    box = QWidget()
    box.setFixedHeight(height)
    return box


def meta_label(text, rich=False, na=False):
    label = QLabel(text)
    label.setProperty("text_style", "meta")
    label.setWordWrap(True)
    if rich:
        label.setTextFormat(Qt.RichText)
    label.setContentsMargins(0, 6, 0, 6)
    if na:
        label.setProperty("na", True)
    return label


def mono(text):
    return f"<span style=\"font-family:'{theming.substitution()['mono_family']}'\">{text}</span>"


def key_values(rows, mono_rows=()):
    """KeyValue plus a ``set_value(key, text_or_None)`` updater (None renders as 'Нет данных')."""
    box = KeyValue(rows)
    values = [w for w in box.findChildren(QLabel) if w.property("kv") == "value"]
    by_key = {key: label for (key, _v), label in zip(rows, values)}
    for key in mono_rows:
        by_key[key].setProperty("text_style", "mono")

    def set_value(key, text):
        label = by_key[key]
        label.setText(tr("Нет данных") if text is None else str(text))
        label.setProperty("na", text is None)
        polish(label)

    box.set_value = set_value
    return box


def format_size(size):
    for unit, step in (("ГБ", 1 << 30), ("МБ", 1 << 20), ("КБ", 1 << 10)):
        if size >= step:
            value = size / step
            return trf("{value} {unit}", value=Num(value, 0 if value >= 10 else 1), unit=unit)
    return trf("{size} Б", size=size)


def directory_size(path):
    total = 0
    for root, _dirs, files in os.walk(path):
        for name in files:
            try:
                total += os.lstat(os.path.join(root, name)).st_size
            except OSError:
                pass
    return total


class _Result(QObject):
    done = pyqtSignal(dict)


class _SizeJob(QRunnable):
    """Measures directories and free disk space off the UI thread."""

    def __init__(self, sinks, disk_path, result):
        super().__init__()
        self.sinks, self.disk_path, self.result = sinks, disk_path, result

    def run(self):
        out = {}
        for key, path in self.sinks.items():
            out[key] = directory_size(path) if os.path.isdir(path) else None
        try:
            path = self.disk_path
            while path and not os.path.exists(path) and os.path.dirname(path) != path:
                path = os.path.dirname(path)  # nearest existing ancestor lives on the same volume
            usage = shutil.disk_usage(path) if path else None
            out["disk"] = (usage.free, usage.total) if usage else None
        except OSError:
            out["disk"] = None
        try:
            self.result.done.emit(out)
        except RuntimeError:  # page was closed while measuring
            pass


def measure(owner, sinks, disk_path, callback):
    """Run the size job in the global thread pool; ``callback(dict)`` runs on the UI thread."""
    from PyQt5.QtCore import QThreadPool

    result = _Result(owner)
    result.done.connect(callback)
    QThreadPool.globalInstance().start(_SizeJob(sinks, disk_path, result))


class Columns(QWidget):
    """Two stacks side by side; folds into one column when narrower than ``fold`` px."""

    def __init__(self, left, right, fold=720, parent=None):
        super().__init__(parent)
        self._fold, self._folded = fold, None
        self.setMinimumWidth(320)  # explicit, so the two-column minimum does not force a scrollbar
        self._grid = QGridLayout(self)
        self._grid.setContentsMargins(0, 0, 0, 0)
        self._grid.setHorizontalSpacing(32)
        self._grid.setVerticalSpacing(0)
        self._boxes = []
        for column in (left, right):
            box = QWidget()
            layout = QVBoxLayout(box)
            layout.setContentsMargins(0, 0, 0, 0)
            layout.setSpacing(0)
            for widget in column:
                layout.addWidget(widget)
            layout.addStretch(1)
            self._boxes.append(box)
        self._place(False)

    def _place(self, folded):
        if folded == self._folded:
            return
        self._folded = folded
        for box in self._boxes:
            self._grid.removeWidget(box)
        self._grid.addWidget(self._boxes[0], 0, 0)
        self._grid.addWidget(self._boxes[1], 1 if folded else 0, 0 if folded else 1)
        self._grid.setColumnStretch(0, 1)
        self._grid.setColumnStretch(1, 0 if folded else 1)

    def resizeEvent(self, event):
        self._place(event.size().width() < self._fold)
        super().resizeEvent(event)
