"""Проверка ботов (canvas LogsBots.dc.html): bot authenticity from the server log, per IP and subnet.

The sheet needs a per-IP verification: PTR and forward DNS for every bot address, the subnet table, and the nginx
deny rule for fake bots. The core answers ``log-analyze --verify-bots`` with one sample per bot family and no IP rows,
so the screen cannot honestly show the table or the verdicts yet. Until the core gives them, the board keeps its
layout (KPI pills, filters, table, verification panel), every value reads «—», and the body is the waiting state.
Nothing is computed here and no sample rows exist.
"""

from __future__ import annotations

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QFrame, QHBoxLayout, QLabel, QToolButton, QVBoxLayout

from .. import i18n, theming
from ..i18n import tr
from ..ui.icons import material_icon
from ..ui.kit import StatePanel, no_project_panel
from .base import Screen

KPIS = (("IP с User-Agent ботов", "select_all"), ("подтверждено", "verified"), ("подделки", "gpp_bad"), ("не проверено", "help"))
FILTERS = (("all", "Все"), ("ok", "Подтверждён"), ("fake", "Подделка"), ("left", "Не проверен"))
BOT_GROUPS = (("Google", "Google"), ("Yandex", "Яндекс"), ("Bing", "Bing"), ("ai", "AI-боты"))
UNAVAILABLE_TIP_TEXT = "Проверка по IP появится, когда ядро отдаст вердикт по каждому адресу лога. Недоступно в этой версии ядра"


def disabled_pill(text, icon, name, group=False):
    """``text`` is already translated; the pill is disabled because the core has no per-IP verdicts yet."""
    pill = QToolButton()
    pill.setProperty("pill", "group" if group else "kpi")
    pill.setObjectName(name)
    pill.setText(text)
    pill.setIcon(material_icon(icon, theming.roles()["text_2"]))
    pill.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
    pill.setEnabled(False)
    pill.setToolTip(tr(UNAVAILABLE_TIP_TEXT))
    return pill


class LogsBotsScreen(Screen):
    slot = ""
    watches = ("project",)

    def __init__(self, host):
        super().__init__(host)
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        head = QFrame()
        head.setProperty("page_bar", True)
        head_layout = QVBoxLayout(head)
        head_layout.setContentsMargins(20, 8, 16, 8)
        head_layout.setSpacing(8)
        title_row = QHBoxLayout()
        title = QLabel(tr("Проверка ботов"))
        title.setProperty("text_style", "title")
        title_row.addWidget(title)
        title_row.addStretch(1)
        self.scope_text = QLabel()
        self.scope_text.setProperty("text_style", "meta")
        title_row.addWidget(self.scope_text)
        head_layout.addLayout(title_row)
        kpi_row = QHBoxLayout()
        kpi_row.setSpacing(6)
        self.kpis = []
        for index, (label, icon) in enumerate(KPIS):
            pill = disabled_pill(f"— {tr(label)}", icon, f"logs_bots_kpi_{index}")
            self.kpis.append(pill)
            kpi_row.addWidget(pill)
        kpi_row.addStretch(1)
        head_layout.addLayout(kpi_row)
        filter_row = QHBoxLayout()
        filter_row.setSpacing(6)
        self.filters = []
        for key, label in FILTERS:
            pill = disabled_pill(tr(label), "select_all", f"logs_bots_filter_{key}", group=True)
            self.filters.append(pill)
            filter_row.addWidget(pill)
        filter_row.addSpacing(8)
        for key, label in BOT_GROUPS:
            pill = disabled_pill(tr(label), "smart_toy", f"logs_bots_group_{key}", group=True)
            self.filters.append(pill)
            filter_row.addWidget(pill)
        filter_row.addStretch(1)
        head_layout.addLayout(filter_row)
        root.addWidget(head)
        self.body = QVBoxLayout()
        self.body.setContentsMargins(24, 16, 24, 16)
        root.addLayout(self.body, 1)
        self.foot_text = QLabel()
        self.foot_text.setProperty("text_style", "meta")
        self.foot_text.setContentsMargins(16, 6, 16, 6)
        self.foot_text.setWordWrap(True)
        root.addWidget(self.foot_text)
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
        self.layout().activate()  # a panel built while the window was hidden keeps its default 640x480 geometry otherwise

    def refresh(self):
        if not self.project_open:
            self.scope_text.setText(tr("Проект не открыт"))
            self._set_panel(no_project_panel(self.host, tr("Откройте проект, чтобы проверить ботов по логам сервера.")))
            self.foot_text.setText(tr("Лог сервера не подключён"))
            return
        self.scope_text.setText(tr("Лог сервера не загружен"))
        # The core verifies bots per family (one sample IP each); it has no per-IP verdicts to list.
        self._set_panel(StatePanel(
            "waiting", "Проверка ботов по логу не загружена",
            tr("Ядро проверяет подлинность ботов по обратному и прямому DNS, но отдаёт вердикт по семейству ботов, "
               "а не по каждому IP. Таблица IP, подсетей и цепочка проверки появятся с этим ответом ядра."),
            hint=UNAVAILABLE_TIP_TEXT,
        ))
        self.foot_text.setText(tr("Журнал сервера не подключён в этой версии ядра"))
