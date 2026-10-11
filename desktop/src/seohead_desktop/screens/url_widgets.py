"""Small painted widgets shared by the URL and Problems screens."""

from __future__ import annotations

from PyQt5.QtCore import QRectF, Qt
from PyQt5.QtGui import QColor, QPainter, QPainterPath
from PyQt5.QtWidgets import QLabel, QWidget

from .. import theming
from ..i18n import tr


class StackBar(QWidget):
    """Thin segmented bar painted from (count, colour) parts; nothing is drawn for an unmeasured total."""

    def __init__(self, height=8, parent=None):
        super().__init__(parent)
        self.parts = []
        self.setFixedHeight(height)

    def set_parts(self, parts):
        self.parts = [(count, colour) for count, colour in parts if type(count) is int and count > 0]
        self.update()

    def paintEvent(self, event):
        painter = QPainter(self)
        painter.setRenderHint(QPainter.Antialiasing)
        rect = QRectF(self.rect())
        painter.setPen(Qt.NoPen)
        painter.setBrush(QColor(theming.roles()["disabled_bg"]))
        painter.drawRoundedRect(rect, 4, 4)
        total = sum(count for count, _colour in self.parts)
        if total:
            painter.setClipPath(self._path(rect))
            left = rect.left()
            for count, colour in self.parts:
                width = rect.width() * count / total
                painter.setBrush(QColor(colour))
                painter.drawRect(QRectF(left, rect.top(), width, rect.height()))
                left += width

    @staticmethod
    def _path(rect):
        path = QPainterPath()
        path.addRoundedRect(rect, 4, 4)
        return path


def section_label(text):
    label = QLabel(tr(text))
    label.setProperty("text_style", "overline")
    return label
