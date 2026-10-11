"""Внешние ссылки (canvas Backlinks.dc.html): the project's link profile from connected sources.

The sheet's data (referring domains, links, anchors, lost/new links, bad donors) is a backlink profile the core does
not store yet: ``backlinks-check`` only verifies a donor list the user supplies. Until the core gives that profile, the
screen shows the honest waiting state, the group filters are disabled with the same reason, and no sample rows exist.
"""

from __future__ import annotations

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QFrame, QHBoxLayout, QLabel, QToolButton, QVBoxLayout

from .. import i18n, theming
from ..i18n import tr
from ..ui.icons import material_icon
from ..ui.kit import StatePanel, no_project_panel, waiting_badge
from .base import Screen

PROFILE_ISSUE = 999
FILTERS = (("all", "Все"), ("follow", "Обычные"), ("nofollow", "nofollow"), ("sponsored", "Платные (sponsored)"), ("ugc", "ugc"))
UNAVAILABLE_FILTER_TIP = tr("Группы ссылок появятся с профилем внешних ссылок проекта. Недоступно в этой версии ядра")


class BacklinksScreen(Screen):
    slot = ""
    watches = ("project",)

    def __init__(self, host):
        super().__init__(host)
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        bar = QFrame()
        bar.setProperty("page_bar", True)
        bar_layout = QHBoxLayout(bar)
        bar_layout.setContentsMargins(20, 8, 16, 8)
        bar_layout.setSpacing(8)
        title = QLabel(tr("Внешние ссылки"))
        title.setProperty("text_style", "title")
        bar_layout.addWidget(title)
        bar_layout.addSpacing(12)
        self.pills = []
        for key, label in FILTERS:
            pill = QToolButton()
            pill.setProperty("pill", "group")
            pill.setObjectName(f"backlinks_filter_{key}")
            pill.setText(tr(label))
            pill.setIcon(material_icon("link", theming.roles()["text_2"]))
            pill.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
            pill.setEnabled(False)
            pill.setToolTip(UNAVAILABLE_FILTER_TIP)
            self.pills.append(pill)
            bar_layout.addWidget(pill)
        bar_layout.addStretch(1)
        root.addWidget(bar)
        self.body = QVBoxLayout()
        self.body.setContentsMargins(24, 16, 24, 16)
        root.addLayout(self.body, 1)
        foot = QFrame()
        foot.setProperty("table_head", True)
        foot_layout = QHBoxLayout(foot)
        foot_layout.setContentsMargins(16, 6, 16, 6)
        foot_layout.setSpacing(8)
        self.foot_text = QLabel()
        self.foot_text.setProperty("text_style", "meta")
        self.foot_text.setWordWrap(True)
        foot_layout.addWidget(self.foot_text, 1)
        foot_layout.addWidget(waiting_badge(PROFILE_ISSUE))
        root.addWidget(foot)
        self.panel = None
        i18n.signals.changed.connect(self._language_changed)
        self.refresh()

    def _language_changed(self, _language):
        self.refresh()

    def _set_panel(self, widget):
        while self.body.count():
            item = self.body.takeAt(0)
            if item.widget() is not None:
                item.widget().deleteLater()
        self.body.addWidget(widget, 1)
        self.panel = widget

    def refresh(self):
        if not self.project_open:
            self._set_panel(no_project_panel(self.host, tr("Откройте проект, чтобы увидеть его внешние ссылки.")))
            self.foot_text.setText(tr("Профиль ссылок проекта не загружен"))
            return
        # The core has no backlink profile yet: say so instead of showing an empty table as a real zero.
        self._set_panel(StatePanel(
            "waiting", "Профиль внешних ссылок не загружен",
            tr("Ядро пока не хранит ссылающиеся домены и ссылки проекта. Проверка доноров по списку работает отдельно."),
            issue=PROFILE_ISSUE,
        ))
        self.foot_text.setText(tr("Источники ссылок проекта не подключены в этой версии"))
