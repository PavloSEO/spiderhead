"""Design-v2 building blocks used by settings and forms: Switch, Segmented, SettingRow, Note, KeyValue."""

from __future__ import annotations

from PyQt5.QtCore import QRectF, QSize, Qt, pyqtSignal
from PyQt5.QtGui import QColor, QPainter
from PyQt5.QtWidgets import (
    QAbstractButton,
    QButtonGroup,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import theming
from ..i18n import tr, trf
from .icons import MaterialIconLabel


def polish(widget):
    """Re-evaluate QSS after a dynamic property changed (only this widget)."""
    widget.style().unpolish(widget)
    widget.style().polish(widget)


class Switch(QAbstractButton):
    """36x20 switch (role=switch for accessibility). Native checkable button, painted from theme roles."""

    def __init__(self, accessible_name, checked=False, parent=None):
        super().__init__(parent)
        self.setCheckable(True)
        self.setChecked(checked)
        self.setAccessibleName(accessible_name)
        self.setCursor(Qt.PointingHandCursor)
        self.setFocusPolicy(Qt.StrongFocus)
        self.setFixedSize(36, 20)
        theming.signals.changed.connect(self._theme_changed)

    def _theme_changed(self, _name):
        self.update()

    def sizeHint(self):
        return QSize(36, 20)

    def paintEvent(self, event):
        r = theming.roles()
        painter = QPainter(self)
        painter.setRenderHint(QPainter.Antialiasing)
        if self.hasFocus():
            painter.setPen(QColor(r["ring"]))
            painter.setBrush(Qt.NoBrush)
            painter.drawRoundedRect(QRectF(0.5, 0.5, 35, 19), 9.5, 9.5)
        painter.setPen(Qt.NoPen)
        if not self.isEnabled():
            track = r["disabled_bg"]
        else:
            track = r["primary"] if self.isChecked() else r["outline"]
        painter.setBrush(QColor(track))
        painter.drawRoundedRect(QRectF(0, 0, 36, 20), 10, 10)
        painter.setBrush(
            QColor(r["on_primary"] if self.isChecked() and self.isEnabled() else r["base"])
        )
        painter.drawEllipse(QRectF(18 if self.isChecked() else 2, 2, 16, 16))


class Segmented(QFrame):
    """Exclusive option group. ``changed`` carries the option value."""

    changed = pyqtSignal(object)

    def __init__(self, options, value=None, accessible_name="", parent=None):
        super().__init__(parent)
        self.setProperty("segmented", True)
        self.setAccessibleName(accessible_name)
        layout = QHBoxLayout(self)
        layout.setContentsMargins(3, 3, 3, 3)
        layout.setSpacing(2)
        self._group = QButtonGroup(self)
        self._group.setExclusive(True)
        self._buttons = {}
        for option_value, label in options:
            button = QToolButton()
            button.setText(label)
            button.setCheckable(True)
            button.setProperty("segment", True)
            button.setFocusPolicy(Qt.StrongFocus)
            self._group.addButton(button)
            self._buttons[option_value] = button
            layout.addWidget(button)
            button.clicked.connect(lambda _checked, v=option_value: self.changed.emit(v))
        if value in self._buttons:
            self._buttons[value].setChecked(True)

    def value(self):
        return next((v for v, b in self._buttons.items() if b.isChecked()), None)

    def setValue(self, value):
        if value in self._buttons:
            self._buttons[value].setChecked(True)


class SettingRow(QFrame):
    """Title + description on the left, control on the right, optional error under the description."""

    def __init__(self, title, description="", control=None, parent=None):
        super().__init__(parent)
        self.setProperty("set_row", True)
        grid = QGridLayout(self)
        grid.setContentsMargins(0, 14, 0, 14)
        grid.setHorizontalSpacing(24)
        text = QVBoxLayout()
        text.setSpacing(2)
        self.title = QLabel(title)
        self.title.setProperty("text_style", "control")
        self.title.setWordWrap(True)
        self.title.setMinimumWidth(200)
        text.addWidget(self.title)
        self.later_badge = QLabel(
            "Заработает позже"
        )  # under the label: a badge beside it squeezes the label
        self.later_badge.setProperty("later", True)
        self.later_badge.hide()
        text.addWidget(self.later_badge)
        self.description = QLabel(description)
        self.description.setProperty("text_style", "meta")
        self.description.setWordWrap(True)
        self.description.setVisible(bool(description))
        text.addWidget(self.description)
        self._error_box = QWidget()
        error_layout = QHBoxLayout(self._error_box)
        error_layout.setContentsMargins(0, 2, 0, 0)
        error_layout.setSpacing(4)
        error_layout.addWidget(MaterialIconLabel("error", 16, color=theming.roles()["error"]))
        self._error = QLabel()
        self._error.setProperty("text_style", "meta")
        self._error.setStyleSheet(f"color: {theming.roles()['error']};")
        self._error.setWordWrap(True)
        error_layout.addWidget(self._error, 1)
        self._error_box.setVisible(False)
        text.addWidget(self._error_box)
        grid.addLayout(text, 0, 0)
        grid.setColumnStretch(0, 1)
        self.control = control
        if control is not None:
            grid.addWidget(control, 0, 1, Qt.AlignRight | Qt.AlignVCenter)
            if not control.accessibleName():
                control.setAccessibleName(title)

    def mark_later(self, reason):
        """The value is stored but changes nothing yet; say so instead of implying it works."""
        self.later_badge.setToolTip(reason)
        self.later_badge.show()
        if self.control is not None:  # a control that changes nothing must not look usable
            self.control.setEnabled(False)
            self.control.setToolTip(reason)

    def set_error(self, message):
        self._error.setText(tr(message) if message else "")
        self._error_box.setVisible(bool(message))
        if self.control is not None:
            self.control.setProperty("invalid", bool(message))
            polish(self.control)


class Note(QFrame):
    """Banner: kind = warn | info | error | success; bold first phrase, text, optional action button."""

    ICONS = {"warn": "warning", "info": "lightbulb", "error": "error", "success": "check_circle"}

    def __init__(self, kind, title, text="", action=None, parent=None, icon=None):
        super().__init__(parent)
        self.setProperty("note", kind)
        layout = QHBoxLayout(self)
        layout.setContentsMargins(12, 10, 12, 10)
        layout.setSpacing(10)
        layout.addWidget(
            MaterialIconLabel(icon or self.ICONS[kind], 20, color=theming.roles()["text_2"]),
            0,
            Qt.AlignTop,
        )
        body = QLabel(
            trf("<b>{title}</b> {text}", title=title, text=text)
            if text
            else trf("<b>{title}</b>", title=title)
        )
        body.setTextFormat(Qt.RichText)
        body.setWordWrap(True)
        layout.addWidget(body, 1)
        if action is not None:
            layout.addWidget(action, 0, Qt.AlignVCenter)


class KeyValue(QWidget):
    """Key (12 grey) / value (13) rows with thin dividers; value None renders italic 'Нет данных'."""

    def __init__(self, rows, parent=None):
        super().__init__(parent)
        grid = QGridLayout(self)
        grid.setContentsMargins(0, 0, 0, 0)
        grid.setHorizontalSpacing(
            0
        )  # gap lives in the key padding so each row's divider runs unbroken
        grid.setVerticalSpacing(0)
        grid.setColumnMinimumWidth(0, 148)
        grid.setColumnStretch(1, 1)
        for index, (key, value) in enumerate(rows):
            k = QLabel(key)
            k.setProperty("text_style", "meta")
            k.setProperty("kv", "key")
            v = QLabel(tr("Нет данных") if value is None else str(value))
            v.setProperty("kv", "value")
            v.setProperty("na", value is None)
            v.setTextInteractionFlags(Qt.TextSelectableByMouse)
            v.setWordWrap(True)
            grid.addWidget(k, index, 0)
            grid.addWidget(v, index, 1)
