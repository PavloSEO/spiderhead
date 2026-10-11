"""Result list of «Поиск в HTML»: one core record per row (a page of at most 100), painted by a delegate.

The core returns one record per searched document with an optional snippet; the position of the marker inside the page and
the number of occurrences are not reported (#939), so the highlight is computed from the snippet text only.
"""

from __future__ import annotations

import re
from itertools import pairwise
from urllib.parse import urlsplit

from PyQt5.QtCore import (
    QAbstractTableModel,
    QObject,
    QRectF,
    QSize,
    QSortFilterProxyModel,
    Qt,
    pyqtSignal,
)
from PyQt5.QtGui import QColor, QFont
from PyQt5.QtWidgets import QApplication, QStyle, QStyledItemDelegate, QStyleOptionViewItem

from .. import theming
from ..i18n import tr, trf
from .scan_common import number

ROW_ROLE = Qt.UserRole + 50
ROW_HEIGHT = 56
# label, core scope
SCOPES = (("Видимый текст", "body_text"), ("HTML-код", "raw_html"), ("Head", "head_markup"), ("Элемент", "selector_markup"))
REPRESENTATIONS = (("Исходный HTML", "static"), ("Сохранённый DOM", "rendered"))
MODES = (("Содержит строку", "contains"), ("Не содержит строку", "not_contains"))
REASONS = (("non_html", "Документ не является HTML"), ("body_absent", "Тело страницы не сохранено"),
           ("decode_fidelity", "Кодировка не позволяет подтвердить отсутствие строки"), ("body_integrity", "Сохранённое тело повреждено"))
PRESENCE = {True: ("ok", "Найдена"), False: ("mut", "Не найдена"), None: ("warn", "Не проверено")}


# (list kind, one row, count ends in 1 but not 11) -> caption of the list footer; literal strings so the dictionary can find them
ROWS = {
    ("found", False, False): "Строки {a}–{b} из {n} найденных", ("found", False, True): "Строки {a}–{b} из {n} найденной",
    ("found", True, False): "Строка {a} из {n} найденных", ("found", True, True): "Строка {a} из {n} найденной",
    ("checked", False, False): "Строки {a}–{b} из {n} проверенных", ("checked", False, True): "Строки {a}–{b} из {n} проверенной",
    ("checked", True, False): "Строка {a} из {n} проверенных", ("checked", True, True): "Строка {a} из {n} проверенной",
    ("without", False, False): "Строки {a}–{b} из {n} без строки", ("without", False, True): "Строки {a}–{b} из {n} без строки",
    ("without", True, False): "Строка {a} из {n} без строки", ("without", True, True): "Строка {a} из {n} без строки",
}
SUMMARY = {
    (True, False): "Найдено на {n} страницах · проверено {m}", (True, True): "Найдено на {n} странице · проверено {m}",
    (False, False): "Без строки на {n} страницах · проверено {m}", (False, True): "Без строки на {n} странице · проверено {m}",
}


def ends_in_one(number):
    """Russian singular agreement: 1, 21, 101 but not 11."""
    return number % 10 == 1 and number % 100 != 11


def rows_caption(first, last, total, kind, partial=False):
    """«Строки 1–100 из 1 326 найденных», «Строка 1 из 1 найденной»; a partial scan adds its note."""
    text = trf(ROWS[(kind, first == last, ends_in_one(total))], a=number(first), b=number(last), n=number(total))
    return text + (" · " + tr("просмотрены не все страницы") if partial else "")


def summary_caption(contains, matching, measured):
    """«Найдено на 1 326 страницах · проверено 1 330»; the counts are the core's, absent ones read «Нет данных»."""
    if not isinstance(matching, int) or not isinstance(measured, int):
        return trf("{what}: {n}", what=tr("Найдено на страницах" if contains else "Без строки на страницах"), n=tr("Нет данных"))
    return trf(SUMMARY[(contains, ends_in_one(matching))], n=number(matching), m=number(measured))


class Choice(QObject):
    """One selected entry out of (label, data) pairs; answers like a QComboBox for code that saves and restores it."""

    changed = pyqtSignal()

    def __init__(self, entries, parent=None):
        super().__init__(parent)
        self.entries = list(entries)
        self.index = 0

    def count(self):
        return len(self.entries)

    def currentData(self):
        return self.entries[self.index][1]

    def findData(self, data):
        return next((i for i, (_label, value) in enumerate(self.entries) if value == data), -1)

    def setCurrentIndex(self, index):
        if 0 <= index < len(self.entries) and index != self.index:
            self.index = index
            self.changed.emit()


def short_url(url):
    """Path and query of a URL (the host is the same for the whole scan); the full address stays in the tooltip."""
    parts = urlsplit(url or "")
    path = (parts.path or "/") + (f"?{parts.query}" if parts.query else "")
    return path if parts.netloc else (url or "")


def find_ranges(text, query, case_sensitive):
    if not query or not text:
        return []
    return [match.span() for match in re.finditer(re.escape(query), text, 0 if case_sensitive else re.IGNORECASE)]


def snippet_window(text, query, case_sensitive, before=24, after=90):
    """The part of a snippet around its first marker: (text, ranges, cut at the left, cut at the right)."""
    ranges = find_ranges(text, query, case_sensitive)
    start = max(0, ranges[0][0] - before) if ranges else 0
    end = min(len(text), start + before + after + (ranges[0][1] - ranges[0][0] if ranges else 0))
    shifted = [(a - start, min(b, end) - start) for a, b in ranges if a < end]
    return text[start:end], shifted, start > 0, end < len(text)


def reason_text(record):
    reason = record.get("reason") or ""
    return next((tr(label) for prefix, label in REASONS if reason.startswith(prefix)), reason)


class ResultModel(QAbstractTableModel):
    """The loaded page of records (never more than the 100 the core pages)."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.rows = []

    def set_rows(self, rows):
        self.beginResetModel()
        self.rows = list(rows)
        self.endResetModel()

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return 1

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid():
            return None
        record = self.rows[index.row()]
        if role == ROW_ROLE:
            return record
        if role == Qt.DisplayRole:
            return record.get("url")
        if role == Qt.ToolTipRole:
            return "\n".join(part for part in (record.get("url"), record.get("snippet"), reason_text(record)) if part)
        return None


class MatchProxy(QSortFilterProxyModel):
    """«Только найденные»: acts on the loaded page, the core cannot filter its result yet (#939)."""

    matched_only = False

    def set_matched_only(self, value):
        self.matched_only = bool(value)
        self.invalidateFilter()

    def filterAcceptsRow(self, row, parent):
        return not self.matched_only or self.sourceModel().rows[row].get("status") == "matched"


class ResultDelegate(QStyledItemDelegate):
    """Two lines: the path with a presence badge, and the snippet around the marker (or the reason it is missing)."""

    query, case_sensitive = "", False

    def sizeHint(self, option, index):
        return QSize(option.rect.width(), ROW_HEIGHT)

    def paint(self, painter, option, index):
        record = index.data(ROW_ROLE)
        cell = QStyleOptionViewItem(option)
        self.initStyleOption(cell, index)
        cell.text = ""
        (option.widget.style() if option.widget else QApplication.style()).drawControl(QStyle.CE_ItemViewItem, cell, painter, option.widget)
        roles, badges = theming.roles(), theming.theme()["badges"]
        selected = bool(option.state & QStyle.State_Selected)
        mono = QFont(theming.substitution()["mono_family"])
        mono.setPixelSize(12)
        painter.save()
        painter.setRenderHint(painter.Antialiasing)
        rect = QRectF(option.rect).adjusted(16, 8, -12, -8)
        kind, label = PRESENCE[record.get("presence")]
        label = tr(label)
        badge_font = QFont(painter.font())
        badge_font.setPixelSize(12)
        badge_font.setWeight(QFont.Medium)
        painter.setFont(badge_font)
        badge_width = painter.fontMetrics().horizontalAdvance(label) + 16
        badge = QRectF(rect.right() - badge_width, rect.top() - 1, badge_width, 20)
        background, ink = badges[kind]
        painter.setPen(Qt.NoPen)
        painter.setBrush(QColor(background))
        painter.drawRoundedRect(badge, 6, 6)
        painter.setPen(QColor(ink))
        painter.drawText(badge, Qt.AlignCenter, label)
        painter.setFont(mono)
        painter.setPen(QColor(roles["on_selected"] if selected else roles["primary"]))
        url = painter.fontMetrics().elidedText(short_url(record.get("url")), Qt.ElideMiddle, int(badge.left() - rect.left() - 10))
        painter.drawText(QRectF(rect.left(), rect.top(), badge.left() - rect.left() - 10, 18), Qt.AlignLeft | Qt.AlignVCenter, url)
        line = QRectF(rect.left(), rect.top() + 22, rect.width(), 18)
        body = QFont(option.font)
        body.setPixelSize(12)
        painter.setFont(body)
        snippet = record.get("snippet")
        if snippet:
            text, ranges, left, right = snippet_window(" ".join(snippet.split()), self.query, self.case_sensitive)
            self._draw_marked(painter, line, ("… " if left else "") + text + (" …" if right else ""), ranges, len("… " if left else ""), roles, badges["warn"], selected)
        else:
            painter.setPen(QColor(roles["text_3"]))
            reason = reason_text(record) or tr("Фрагмент не запрошен или не найден")
            painter.drawText(line, Qt.AlignLeft | Qt.AlignVCenter, painter.fontMetrics().elidedText(reason, Qt.ElideRight, int(line.width())))
        painter.restore()

    @staticmethod
    def _draw_marked(painter, line, text, ranges, shift, roles, mark, selected):
        """Draw ``text`` left to right with the ranges on the mark colours; stop with an ellipsis at the edge."""
        metrics = painter.fontMetrics()
        cuts = sorted({0, len(text), *(point + shift for span in ranges for point in span)})
        x, limit = line.left(), line.right()
        for start, end in pairwise(cuts):
            piece = text[start:end]
            marked = any(a + shift <= start and end <= b + shift for a, b in ranges)
            width = metrics.horizontalAdvance(piece)
            fit = x + width <= limit
            if not fit:
                piece = metrics.elidedText(piece, Qt.ElideRight, int(limit - x))
                width = metrics.horizontalAdvance(piece)
            box = QRectF(x, line.top(), width, line.height())
            if marked:
                painter.setPen(Qt.NoPen)
                painter.setBrush(QColor(mark[0]))
                painter.drawRoundedRect(box.adjusted(-1, 1, 1, -1), 2, 2)
            painter.setPen(QColor(mark[1] if marked else roles["on_selected"] if selected else roles["text_2"]))
            painter.drawText(box, Qt.AlignLeft | Qt.AlignVCenter, piece)
            x += width
            if not fit:
                break
