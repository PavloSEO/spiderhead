"""Task board «Доска» of the Work screen (design canvas Tasks): core task rows grouped into four columns by display state.

Only core rows are shown. The core gives no assignee, description or URL count per task yet (#922, #1240), so the board
says so with waiting badges instead of inventing them. Cards are capped per column to keep the widget bounded.
"""

from __future__ import annotations

from PyQt5.QtCore import Qt, pyqtSignal
from PyQt5.QtWidgets import QFrame, QHBoxLayout, QLabel, QScrollArea, QVBoxLayout, QWidget

from ..i18n import tr, trf
from ..ui.icons import MaterialIconLabel, resolve_color
from ..ui.kit import waiting_badge
from .work import TASK_KINDS, clear_layout, number

MAX_CARDS_PER_COLUMN = 100
# (key, title, icon, icon colour role, core display states that land in the column)
BOARD_COLUMNS = (
    ("new", "Новые", "radio_button_unchecked", "role:primary", ("remaining",)),
    ("work", "В работе", "progress_activity", "role:primary", ("running", "blocked")),
    ("review", "Ждёт перепроверки", "schedule", "role:text_2", ("review", "stale", "deliverable")),
    ("done", "Подтверждено", "verified", "role:success", ("completed",)),
)


def priority_colour(priority):
    """Core priorities are «P1», «P2»… ; P1 is the strongest signal, anything else stays muted."""
    if priority == "P1":
        return "role:error"
    if priority == "P2":
        return "role:warning"
    return "role:text_muted"


class TaskCard(QFrame):
    """One core task. A click (or release inside the card) asks the screen to select it."""

    picked = pyqtSignal(str)

    def __init__(self, row, parent=None):
        super().__init__(parent)
        self.item_id = row.get("id") or ""
        self.setProperty("card", "task")
        self.setCursor(Qt.PointingHandCursor)
        self.setAccessibleName(row.get("title") or tr("Нет данных"))
        layout = QVBoxLayout(self)
        layout.setContentsMargins(12, 10, 12, 10)
        layout.setSpacing(6)
        top = QHBoxLayout()
        top.setSpacing(6)
        dot = QLabel()
        dot.setFixedSize(8, 8)
        dot.setStyleSheet(f"background: {resolve_color(priority_colour(row.get('priority')))}; border-radius: 4px;")
        meta = QLabel(f"{row.get('priority') or tr('Нет данных')} · {self.item_id}")
        meta.setProperty("text_style", "meta")
        meta.setToolTip(self.item_id)
        top.addWidget(dot)
        top.addWidget(meta, 1)
        layout.addLayout(top)
        title = QLabel(row.get("title") or tr("Нет данных"))
        title.setProperty("text_style", "control")
        title.setWordWrap(True)
        layout.addWidget(title)
        badges = QHBoxLayout()
        badges.setSpacing(6)
        kind = QLabel(tr(TASK_KINDS.get(row.get("kind"), row.get("kind") or "Нет данных")))
        kind.setProperty("badge", "mut")
        badges.addWidget(kind)
        if row.get("state") == "blocked":
            blocked = QLabel(tr("Заблокирована"))
            blocked.setProperty("badge", "err")
            badges.addWidget(blocked)
        badges.addStretch(1)
        layout.addLayout(badges)

    def mouseReleaseEvent(self, event):
        if event.button() == Qt.LeftButton and self.rect().contains(event.pos()):
            self.picked.emit(self.item_id)
        super().mouseReleaseEvent(event)


class BoardColumn(QFrame):
    def __init__(self, key, title, icon, colour, parent=None):
        super().__init__(parent)
        self.key = key
        self.setProperty("card", "column")
        self.setMinimumWidth(220)
        outer = QVBoxLayout(self)
        outer.setContentsMargins(10, 10, 10, 10)
        outer.setSpacing(8)
        head = QHBoxLayout()
        head.setContentsMargins(4, 2, 4, 2)
        head.setSpacing(6)
        head.addWidget(MaterialIconLabel(icon, 18, color=colour))
        self.title = QLabel(tr(title))
        self.title.setProperty("text_style", "control")
        self.count = QLabel()
        self.count.setProperty("text_style", "meta")
        head.addWidget(self.title, 1)
        head.addWidget(self.count)
        outer.addLayout(head)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        body = QWidget()
        self.cards = QVBoxLayout(body)
        self.cards.setContentsMargins(0, 0, 0, 0)
        self.cards.setSpacing(8)
        self.cards.addStretch(1)
        scroll.setWidget(body)
        outer.addWidget(scroll, 1)

    def fill(self, rows, on_pick):
        clear_layout(self.cards)
        shown = rows[:MAX_CARDS_PER_COLUMN]
        for row in shown:
            card = TaskCard(row)
            card.picked.connect(on_pick)
            self.cards.addWidget(card)
        self.cards.addStretch(1)
        self.count.setText(number(len(rows)))
        if len(rows) > len(shown):
            note = QLabel(trf("Ещё {n} — в списке", n=number(len(rows) - len(shown))))
            note.setProperty("text_style", "meta")
            note.setWordWrap(True)
            self.cards.insertWidget(self.cards.count() - 1, note)


class TaskBoard(QWidget):
    """Four columns of core tasks. ``card_picked`` carries the task id of the clicked card."""

    card_picked = pyqtSignal(str)

    def __init__(self, parent=None):
        super().__init__(parent)
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)
        self.columns = [BoardColumn(*column[:4]) for column in BOARD_COLUMNS]
        content = QWidget()
        row = QHBoxLayout(content)
        row.setContentsMargins(12, 12, 12, 12)
        row.setSpacing(12)
        for column in self.columns:
            row.addWidget(column, 1)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAsNeeded)
        scroll.setVerticalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        scroll.setWidget(content)
        outer.addWidget(scroll, 1)
        footer = QHBoxLayout()
        footer.setContentsMargins(12, 0, 12, 8)
        footer.setSpacing(8)
        self.outside = QLabel()
        self.outside.setProperty("text_style", "meta")
        self.outside.setWordWrap(True)
        footer.addWidget(waiting_badge(922, "Исполнитель, описание и комментарии задачи"))
        footer.addWidget(waiting_badge(1240, "Число URL, которых касается задача"))
        footer.addWidget(self.outside, 1)
        outer.addLayout(footer)

    def set_rows(self, rows):
        placed = 0
        for column, (_key, _title, _icon, _colour, states) in zip(self.columns, BOARD_COLUMNS):
            chosen = [r for r in rows if r.get("state") in states]
            placed += len(chosen)
            column.fill(chosen, self.card_picked.emit)
        rest = len(rows) - placed
        self.outside.setText(trf("Вне колонок: {n} (не согласованы, недоступны или исключены)", n=number(rest)) if rest else "")
        self.outside.setVisible(bool(rest))
