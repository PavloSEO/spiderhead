"""Screen «Граф ссылок» (sheet GraphDark, shell section «Граф ссылок»).

The canvas needs site-wide nodes and edges. The core does not give them yet (paged link graph is core issue #975),
so the body is an honest waiting state: no sample nodes, no invented counts. The three footer counters stay «Нет данных».
"""

from __future__ import annotations

from PyQt5.QtWidgets import QHBoxLayout, QVBoxLayout, QWidget

from ..i18n import tr
from ..ui.kit import Gate, Kpi, PageHeader, StatePanel

GRAPH_ISSUE = 975
GRAPH_HINT = "Граф всего сайта: узлы, рёбра и сироты из сохранённого скана"


class GraphScreen(QWidget):
    def __init__(self, host):
        super().__init__()
        self.host = host
        self.setProperty("spaciousPage", False)
        self.header = PageHeader("Граф ссылок", tr("Ссылки из контента"))
        stats = QHBoxLayout()
        stats.setSpacing(8)
        self.kpis = [Kpi(label) for label in ("Узлы", "Рёбра", "Сироты")]
        for kpi in self.kpis:
            stats.addWidget(kpi)
        body = QWidget()
        body_layout = QVBoxLayout(body)
        body_layout.setContentsMargins(0, 0, 0, 0)
        body_layout.setSpacing(12)
        body_layout.addLayout(stats)
        body_layout.addWidget(StatePanel("waiting", "Граф ссылок ещё не отдаёт ядро",
                                         "Здесь появится граф сайта из сохранённого скана проекта.",
                                         issue=GRAPH_ISSUE, hint=GRAPH_HINT), 1)
        self.gate = Gate(body, self._state, host.choose_project)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(12)
        layout.addWidget(self.header)
        layout.addWidget(self.gate, 1)
        host.data_changed.connect(lambda _kind: self.gate.refresh())

    def _state(self):
        return "content" if self.host.project_directory else "open"
