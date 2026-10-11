"""Логи × скан (canvas LogsCompare.dc.html): four joins of a server access log with the scan's URLs.

No join exists in the core yet (#1009) and the desktop cannot import an access log (#1180), so every card shows the
honest waiting state with the neutral «Недоступно в этой версии ядра» badge, never sample numbers. The header, the
disabled bot and period filters and the 2×2 card grid are kept as drawn, so the data can be wired without a redesign.
"""

from __future__ import annotations

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QFrame, QGridLayout, QHBoxLayout, QLabel, QToolButton, QVBoxLayout

from .. import theming
from ..i18n import tr
from ..ui.icons import material_icon
from ..ui.kit import StatePanel, waiting_badge
from .base import Screen

JOIN_ISSUE = 1009
UNAVAILABLE_TIP = tr("Недоступно в этой версии ядра")
FILTERS = ("Google", "Яндекс", "Все боты", "14 дн.", "30 дн.", "60 дн.")
# (icon, badge tone, title, subtitle, waiting text)
CARDS = (
    ("visibility_off", "err", "Не видит бота", "URL из скана без визитов дольше порога",
     "Визиты бота к URL скана появятся после импорта лога"),
    ("travel_explore", "warn", "Боты ходят, в скане нет", "сироты, мусорные адреса и параметры",
     "Адреса из логов сопоставятся со сканом после импорта"),
    ("payments", "err", "Бюджет обхода впустую", "визиты на 3xx, 4xx, 5xx и неиндексируемые",
     "Доля визитов по причинам появится после сопоставления"),
    ("schedule", "info", "Частота визитов по разделам", "как часто бот возвращается в каждый раздел",
     "Частота по разделам появится после импорта лога"),
)


class LogsCompareScreen(Screen):
    slot = "logs_compare"

    def __init__(self, host):
        super().__init__(host)
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        root.addWidget(self._header())
        root.addWidget(self._filters())
        grid = QGridLayout()
        grid.setContentsMargins(20, 12, 20, 12)
        grid.setSpacing(12)
        for index, card in enumerate(CARDS):
            grid.addWidget(self._card(*card), index // 2, index % 2)
        grid.setRowStretch(0, 1)
        grid.setRowStretch(1, 1)
        root.addLayout(grid, 1)
        self.refresh()

    def refresh(self):
        # Nothing to read until the core joins logs with scans (#1009); the screen never polls or invents rows.
        pass

    def _header(self):
        bar = QFrame()
        bar.setProperty("page_bar", True)
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(20, 8, 16, 8)
        layout.setSpacing(8)
        title = QLabel(tr("Логи × скан"))
        title.setProperty("text_style", "title")
        layout.addWidget(title)
        layout.addStretch(1)
        layout.addWidget(waiting_badge(JOIN_ISSUE, tr("Сопоставление логов с результатами скана")))
        return bar

    def _filters(self):
        bar = QFrame()
        bar.setProperty("page_bar", True)
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(20, 6, 16, 6)
        layout.setSpacing(6)
        for label in FILTERS:
            pill = QToolButton()
            pill.setProperty("pill", "group")
            pill.setCheckable(True)
            pill.setText(tr(label))
            pill.setEnabled(False)
            pill.setToolTip(UNAVAILABLE_TIP)
            layout.addWidget(pill)
        layout.addStretch(1)
        hint = QLabel(tr("Импорт лога появится в этом разделе"))
        hint.setProperty("text_style", "meta")
        layout.addWidget(hint)
        return bar

    def _card(self, icon, tone, title, subtitle, waiting):
        badges = theming.theme()["badges"]
        background, foreground = badges[tone]
        panel = QFrame()
        panel.setProperty("card", "panel")
        layout = QVBoxLayout(panel)
        layout.setContentsMargins(12, 10, 12, 10)
        layout.setSpacing(8)
        head = QHBoxLayout()
        head.setSpacing(10)
        tile = QLabel()
        tile.setFixedSize(32, 32)
        tile.setAlignment(Qt.AlignCenter)
        tile.setStyleSheet(f"background: {background}; border-radius: 8px;")
        tile.setPixmap(material_icon(icon, foreground).pixmap(18, 18))
        head.addWidget(tile)
        texts = QVBoxLayout()
        texts.setSpacing(1)
        name = QLabel(tr(title))
        name.setProperty("text_style", "section")
        note = QLabel(tr(subtitle))
        note.setProperty("text_style", "meta")
        texts.addWidget(name)
        texts.addWidget(note)
        head.addLayout(texts, 1)
        layout.addLayout(head)
        layout.addWidget(StatePanel("waiting", tr("Нет данных"), tr(waiting)), 1)
        return panel
