"""Fatal states screen: shown instead of data when the core or the project folder cannot serve it. Reachable as extra "fatal".

It never calls the core: conditions come from facts the window already holds (core path, project folder).
"""

from __future__ import annotations

from PyQt5.QtWidgets import QGridLayout, QLabel, QVBoxLayout

from ..i18n import tr
from ..ui.fatal import FatalCard, WaitingCard, clear_layout, fatal_conditions, fill_grid
from ..ui.kit import StatePanel
from .base import Screen


class FatalStatesScreen(Screen):
    chrome_free = True  # SHELL-CANON §7: no project / scan switchers while a fatal state is shown

    def __init__(self, host):
        super().__init__(host)
        self.setObjectName("fatalPage")
        root = QVBoxLayout(self)
        root.setContentsMargins(40, 28, 40, 20)
        root.setSpacing(16)
        title = QLabel(tr("Фатальные состояния"))
        title.setProperty("text_style", "title")
        subtitle = QLabel(tr("Окно не может работать с данными. Каждое состояние говорит, что случилось и что сделать — без потери сканов."))
        subtitle.setProperty("text_style", "meta")
        subtitle.setWordWrap(True)
        root.addWidget(title)
        root.addWidget(subtitle)
        self.notice = QVBoxLayout()
        self.grid = QGridLayout()
        self.grid.setSpacing(16)
        root.addLayout(self.notice)
        root.addLayout(self.grid)
        root.addStretch(1)
        self._cards = []
        self.refresh()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        fill_grid(self.grid, self._cards, self.width() or 1440)

    def refresh(self):
        """Re-read the real facts and rebuild the cards; cheap, no core call."""
        conditions = fatal_conditions(self.host.core_executable, self.host.project_directory)
        actions = {
            "core_missing": (("Мастер установки", lambda: self.host.show_screen("onboarding")),),
            "disk_low": (("Проверить снова", self.refresh),),
            "no_write_access": (("Проверить снова", self.refresh),),
        }
        clear_layout(self.notice)
        clear_layout(self.grid)
        self._cards = [FatalCard(condition, actions.get(condition.key, ())) for condition in conditions] + [WaitingCard()]
        if not conditions:
            self.notice.addWidget(StatePanel("empty", "Фатальных состояний нет",
                                             "Ядро найдено, папка проекта доступна для записи и на диске хватает места.")
                                  if self.host.project_directory else
                                  StatePanel("empty", "Проект не открыт", "Состояние папки проекта появится после открытия проекта."))
        fill_grid(self.grid, self._cards, self.width() or 1440)
