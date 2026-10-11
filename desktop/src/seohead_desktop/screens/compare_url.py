"""One URL compared across two saved scans (canvas CompareUrl.dc.html).

The core does not yet return the state of one normalized URL in two scans (core issue 972), so the field table,
the text fragment and the outgoing-links card show the neutral waiting state. ``set_rows`` renders one page of real
rows once a caller has them; nothing here invents values.
"""

from __future__ import annotations

from PyQt5.QtCore import Qt, pyqtSignal
from PyQt5.QtWidgets import QFrame, QGridLayout, QHBoxLayout, QLabel, QToolButton, QVBoxLayout, QWidget

from .. import theming
from ..i18n import tr, trf
from ..ui.icons import material_icon
from ..ui.kit import StatePanel, waiting_badge
from ..ui.presentation import theme_tokens

PAGE_LIMIT = 100  # one comparison page, the same bound as seohead_desktop.comparison.PAGE_LIMIT
FIELD_WIDTH, DELTA_WIDTH = 170, 110  # canvas columns (delta widened so «добавлен» is not clipped); scans share the rest
CORE_ISSUE = 972
HEADERS = ("Поле", "Скан №1", "Скан №2", "Δ")


class CompareUrlScreen(QWidget):
    back_requested = pyqtSignal()

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setObjectName("compareUrl")
        self._colors = theme_tokens()["colors"]
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)

        bar = QFrame()
        bar.setFixedHeight(52)
        head = QHBoxLayout(bar)
        head.setContentsMargins(12, 0, 16, 0)
        head.setSpacing(10)
        self.back = QToolButton()
        self.back.setIcon(material_icon("arrow_back", self._colors["on_surface"]))
        self.back.setToolTip(tr("К сравнению сканов"))
        self.back.setAccessibleName(tr("К сравнению сканов"))
        self.back.clicked.connect(self.back_requested)
        self.title = QLabel("—")
        self.title.setProperty("text_style", "section")
        self.title.setTextInteractionFlags(Qt.TextSelectableByMouse)
        self.count = QLabel()
        self.count.setProperty("badge", "warn")
        self.count.setFixedHeight(22)
        self.count.hide()
        self.badge = waiting_badge(CORE_ISSUE)
        head.addWidget(self.back)
        head.addWidget(self.title, 1)
        head.addWidget(self.count)
        head.addWidget(self.badge)
        root.addWidget(bar)

        self.table_frame = QFrame()
        self.table = QGridLayout(self.table_frame)
        self.table.setContentsMargins(0, 0, 0, 0)
        self.table.setSpacing(0)
        self.table.setColumnMinimumWidth(0, FIELD_WIDTH)
        self.table.setColumnMinimumWidth(3, DELTA_WIDTH)
        self.table.setColumnStretch(1, 1)
        self.table.setColumnStretch(2, 1)
        root.addWidget(self.table_frame)
        self.waiting = StatePanel(
            "waiting", "Сравнение полей URL недоступно",
            "Ядро ещё не отдаёт состояние этого URL в двух сканах.", issue=CORE_ISSUE,
        )
        root.addWidget(self.waiting)

        cards = QGridLayout()
        cards.setContentsMargins(16, 12, 16, 12)
        cards.setSpacing(12)
        self.text_card = self._card("Текст · фрагмент изменений")
        self.links_card = self._card("Исходящие ссылки")
        cards.addWidget(self.text_card, 0, 0)
        cards.addWidget(self.links_card, 0, 1)
        root.addLayout(cards)
        root.addStretch(1)
        self.set_rows(None)

    def _card(self, title):
        card = QFrame()
        card.setProperty("card", True)
        layout = QVBoxLayout(card)
        layout.setContentsMargins(14, 12, 14, 12)
        label = QLabel(tr(title))
        label.setProperty("text_style", "section")
        layout.addWidget(label)
        layout.addWidget(waiting_badge(CORE_ISSUE, tr("Исходящие ссылки и текст по двум сканам")), 0, Qt.AlignLeft)
        layout.addStretch(1)
        return card

    def set_url(self, url):
        self.title.setText(url or "—")

    def set_rows(self, rows):
        """Show one page of rows ``(field, before, after, delta, changed)``; ``None`` or empty shows the waiting state."""
        if rows is not None and len(rows) > PAGE_LIMIT:
            raise ValueError("A comparison page holds at most 100 rows")
        while self.table.count():
            item = self.table.takeAt(0)
            if item.widget():
                item.widget().deleteLater()
        self.count.hide()
        self.badge.setVisible(not rows)
        self.waiting.setVisible(not rows)
        self.table_frame.setVisible(bool(rows))
        if not rows:
            return
        changed = sum(1 for row in rows if row[4])
        self.count.setText(trf("Изменений: {count}", count=changed))
        self.count.show()
        for col, text in enumerate(HEADERS):
            self.table.addWidget(self._cell(tr(text), "section_head"), 0, col)
        for index, (field, before, after, delta, is_changed) in enumerate(rows, start=1):
            for col, text in enumerate((field, before, after, delta)):
                self.table.addWidget(self._cell(text, "row", is_changed and col == 2), index, col)

    def _cell(self, text, kind, highlight=False):
        label = QLabel(str(text) if text not in (None, "") else "—")
        label.setWordWrap(True)
        label.setProperty("text_style", "meta")
        roles = theming.roles()
        background = roles["raised"] if kind == "section_head" else roles["choice_bg"] if highlight else "transparent"
        label.setStyleSheet(f"padding: 8px 12px; background: {background};"
                            + (" font-weight: 500;" if kind == "section_head" else ""))
        return label
