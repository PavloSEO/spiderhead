"""Widgets of the sources screens: service tile, list row, back header, bounded table, state line.

The tile is the official mark on a white tile (ui.brand_logos.BrandTile) or, for a service without a shipped mark,
a letter monogram on the same tile as the sheets draw it - never an invented logo.
"""

from __future__ import annotations

from PyQt5.QtCore import QAbstractTableModel, Qt, pyqtSignal
from PyQt5.QtGui import QColor, QFont, QPainter, QPainterPath, QPen
from PyQt5.QtWidgets import (
    QAbstractItemView,
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QSizePolicy,
    QTableView,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from ... import i18n
from ...i18n import joined, tr
from ..brand_logos import BORDER, TILE, WHITE, BrandTile, logo_path
from ..icons import material_icon
from .listing import action_button, badge
from .source_catalogue import NAMES, name_of

# Letter on the white tile. Not a theme role: the tile stays white in every theme, so its ink must not follow the theme.
_MONOGRAM_INK = "#45464F"


class MonogramTile(QWidget):
    def __init__(self, letters, size=TILE, parent=None):
        super().__init__(parent)
        self.letters = letters
        self.setFixedSize(size, size)
        self.setAttribute(Qt.WA_TransparentForMouseEvents)

    def paintEvent(self, _event):
        painter = QPainter(self)
        painter.setRenderHints(QPainter.Antialiasing | QPainter.TextAntialiasing)
        tile = self.rect().adjusted(0, 0, -1, -1)
        shape = QPainterPath()
        shape.addRoundedRect(tile.x() + 0.5, tile.y() + 0.5, tile.width(), tile.height(), 10, 10)
        painter.fillPath(shape, QColor(WHITE))
        painter.setPen(QPen(QColor(BORDER), 1))
        painter.drawPath(shape)
        painter.setPen(QColor(_MONOGRAM_INK))
        font = QFont(self.font())
        font.setPixelSize(15 if len(self.letters) < 2 else 12)
        font.setWeight(QFont.Bold)
        painter.setFont(font)
        painter.drawText(self.rect(), Qt.AlignCenter, self.letters)


def service_tile(pid, size=TILE):
    _name, logo, letters = NAMES.get(pid, (pid, None, pid[:2].upper()))
    if logo and logo_path(logo) is not None:
        return BrandTile(logo, size)
    return MonogramTile(letters, size)


class SourceRow(QFrame):
    """Divider-separated list row: tile, name (+ paid badge) and sub line, status badge, «Настроить»; the whole row opens the detail."""

    opened = pyqtSignal(str)

    def __init__(self, pid, paid, kind, text, icon, sub, parent=None):
        super().__init__(parent)
        self.pid = pid
        self.setProperty("list_item", True)
        self.setCursor(Qt.PointingHandCursor)
        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 8, 0, 8)
        layout.setSpacing(12)
        layout.addWidget(service_tile(pid), 0, Qt.AlignVCenter)
        texts = QVBoxLayout()
        texts.setSpacing(2)
        head = QHBoxLayout()
        head.setSpacing(8)
        title = QLabel(name_of(pid))
        title.setProperty("text_style", "control")
        title.setWordWrap(True)
        head.addWidget(title, 1)
        texts.addLayout(head)
        if paid:  # in the sub line, so a narrow window keeps the row without a sideways scroll
            sub = joined(" · ", [p for p in ("платный" if paid == "paid" else "платный по желанию", sub) if p])
        meta = QLabel(sub)
        meta.setProperty("text_style", "meta")
        meta.setWordWrap(True)
        meta.setVisible(bool(sub))
        texts.addWidget(meta)
        layout.addLayout(texts, 1)
        self.state = badge(kind, text, icon)
        layout.addWidget(self.state, 0, Qt.AlignVCenter)
        self.button = icon_button("tune", "Настроить", lambda: self.opened.emit(self.pid))
        layout.addWidget(self.button, 0, Qt.AlignVCenter)

    def mouseReleaseEvent(self, event):
        if event.button() == Qt.LeftButton and self.rect().contains(event.pos()):
            self.opened.emit(self.pid)
        super().mouseReleaseEvent(event)


def back_header(title, back_text, on_back, subtitle="", pid=None, actions=(), paid=""):
    """«← Все источники» + (tile) + title (+ paid badge) / subtitle + right-aligned actions."""
    box = QWidget()
    outer = QVBoxLayout(box)
    outer.setContentsMargins(0, 0, 0, 8)
    outer.setSpacing(8)
    back = action_button(back_text, icon="arrow_back", role="text")
    back.clicked.connect(on_back)
    back.setObjectName("sourcesBack")
    outer.addWidget(back, 0, Qt.AlignLeft)
    row = QHBoxLayout()
    row.setSpacing(12)
    if pid:
        row.addWidget(service_tile(pid))
    texts = QVBoxLayout()
    texts.setSpacing(2)
    head = QLabel(title)
    head.setProperty("text_style", "section")
    heading = QHBoxLayout()
    heading.setSpacing(8)
    heading.addWidget(head)
    if paid:
        heading.addWidget(badge("warn", tr("платный" if paid == "paid" else "платный по желанию"), "payments"), 0, Qt.AlignVCenter)
    heading.addStretch(1)
    texts.addLayout(heading)
    if subtitle:
        meta = QLabel(subtitle)
        meta.setProperty("text_style", "meta")
        meta.setWordWrap(True)
        texts.addWidget(meta)
    row.addLayout(texts, 1)
    for widget in actions:
        row.addWidget(widget, 0, Qt.AlignVCenter)
    outer.addLayout(row)
    return box


class _Model(QAbstractTableModel):
    def __init__(self, headers, rows, parent):
        super().__init__(parent)
        self.headers, self.rows = headers, rows
        i18n.signals.changed.connect(self._language_changed)

    def _language_changed(self, _language):
        self.headerDataChanged.emit(Qt.Horizontal, 0, len(self.headers) - 1)
        self.layoutChanged.emit()

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.headers)

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid():
            return None
        value = self.rows[index.row()][index.column()]
        if role in (Qt.DisplayRole, Qt.ToolTipRole):
            return tr("Нет данных") if value is None else i18n.convert(str(value))
        if role == Qt.TextAlignmentRole and index.column() > 0 and isinstance(value, str) and value[:1].isdigit():
            return int(Qt.AlignRight | Qt.AlignVCenter)
        return None

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if role == Qt.DisplayRole and orientation == Qt.Horizontal:
            return tr(self.headers[section])
        return None


def table(headers, rows, name, limit=12):
    """Bounded read-only table (at most ``limit`` rows, 32 px each); the caller says how many were cut."""
    view = QTableView()
    view.setModel(_Model(headers, rows[:limit], view))
    view.setEditTriggers(QAbstractItemView.NoEditTriggers)
    view.setSelectionBehavior(QAbstractItemView.SelectRows)
    view.horizontalHeader().setSectionResizeMode(QHeaderView.Stretch)
    view.horizontalHeader().setFixedHeight(34)
    view.verticalHeader().hide()
    view.verticalHeader().setDefaultSectionSize(32)
    view.setShowGrid(False)
    view.setVerticalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
    shown = min(len(rows), limit)
    view.setFixedHeight(34 + 32 * shown + 2)
    view.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
    view.setAccessibleName(name)
    return view


def icon_button(icon, tip, callback=None, enabled=True):
    button = QToolButton()
    button.setProperty("role", "icon")
    button.setIcon(material_icon(icon, "role:text_2"))
    button.setToolTip(tip)
    button.setAccessibleName(tip)
    button.setEnabled(enabled)
    if callback:
        button.clicked.connect(callback)
    return button
