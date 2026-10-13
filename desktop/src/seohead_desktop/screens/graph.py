"""Screen «Граф ссылок»: nodes, edges and orphans of the selected saved scan, read from ``seohead scan-graph``.

One core call per page of nodes (200 rows) gives the totals for the three counters and the rows of the table, so the
screen never holds more than a page. The drawing of a single URL's neighbourhood lives in the URL card.
"""

from __future__ import annotations

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import (
    QAbstractItemView,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QTableWidget,
    QTableWidgetItem,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from ..i18n import tr, trf
from ..ui.icons import material_icon
from ..ui.kit import Gate, Kpi, PageHeader, StatePanel, style_table
from .scan_common import number, selected_scan
from .url_query import PAGE, CoreJob, reason_text

COLUMNS = (
    ("url", "Адрес"),
    ("status_code", "HTTP"),
    ("depth", "Глубина обхода"),
    ("inlinks", "Входящих"),
    ("word_count", "Слов"),
)


def graph_arguments(scan, offset=0):
    return [
        "scan-graph",
        "--scan",
        scan,
        "--node-limit",
        str(PAGE),
        "--node-offset",
        str(offset),
        "--edge-limit",
        "1",
    ]


class GraphScreen(QWidget):
    def __init__(self, host):
        super().__init__()
        self.host = host
        self.setProperty("spaciousPage", False)
        self.scan_path = None
        self.offset = 0
        self.token = 0
        self.job = CoreJob(host, self)
        self.job.done.connect(self._done)
        self.job.failed.connect(lambda token, reason: self._failed(token, reason))
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
        self.table = style_table(QTableWidget(0, len(COLUMNS)), "compact")
        self.table.setHorizontalHeaderLabels([tr(title) for _key, title in COLUMNS])
        self.table.setSelectionMode(QAbstractItemView.NoSelection)
        self.table.horizontalHeader().setStretchLastSection(False)
        self.table.horizontalHeader().setSectionResizeMode(0, QHeaderView.Stretch)
        self.panel = QVBoxLayout()
        body_layout.addWidget(self.table, 1)
        body_layout.addLayout(self.panel)
        foot = QHBoxLayout()
        self.foot_text = QLabel()
        self.foot_text.setProperty("text_style", "meta")
        foot.addWidget(self.foot_text, 1)
        self.prev = QToolButton()
        self.prev.setIcon(material_icon("chevron_left"))
        self.prev.setToolTip(tr("Предыдущая страница"))
        self.prev.clicked.connect(lambda: self._go(-1))
        self.next = QToolButton()
        self.next.setIcon(material_icon("chevron_right"))
        self.next.setToolTip(tr("Следующая страница"))
        self.next.clicked.connect(lambda: self._go(1))
        foot.addWidget(self.prev)
        foot.addWidget(self.next)
        body_layout.addLayout(foot)
        self.gate = Gate(body, self._state, host.choose_project)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(12)
        layout.addWidget(self.header)
        layout.addWidget(self.gate, 1)
        host.data_changed.connect(self._data_changed)
        self._sync(force=True)

    def _state(self):
        return "content" if self.host.project_directory else "open"

    def _data_changed(self, kind):
        if kind in ("project", "scans", "scan_status"):
            self._sync()

    def _sync(self, force=False):
        self.gate.refresh()
        scan = selected_scan(self.host) if self.host.project_directory else None
        path = scan.get("path") if scan else None
        if force or path != self.scan_path:
            self.scan_path = path
            self.offset = 0
            self._reload()

    def _reload(self):
        self.job.cancel()
        self._clear_panel()
        if not self.scan_path:
            self._show("empty", "Скан не выбран", "Выберите скан проекта, чтобы увидеть его граф.")
            return self._reset_view()
        self.token += 1
        self.job.start(graph_arguments(self.scan_path, self.offset), self.token)
        self._show("loading", "Читаю граф", "Ядро считает узлы, рёбра и сироты скана.")

    def _reset_view(self):
        for kpi in self.kpis:
            kpi.set_value(None)
        self.table.setRowCount(0)
        self.foot_text.setText("")
        self.prev.setEnabled(False)
        self.next.setEnabled(False)

    def _done(self, token, payload):
        if token != self.token:
            return
        if payload.get("ok") is not True or payload.get("state") not in ("complete", "partial"):
            self._reset_view()
            return self._show(
                "partial",
                "Граф недоступен",
                reason_text(payload) if not payload.get("ok") else payload.get("reason_code") or "",
            )
        self._clear_panel()
        nodes = payload.get("nodes") or []
        total = payload.get("filtered_total")
        stats = payload.get("statistics") or {}
        self.kpis[0].set_value(number(total))
        self.kpis[1].set_value(number(payload.get("edge_total")))
        orphans = stats.get("orphans")
        self.kpis[2].set_value(number(orphans) if type(orphans) is int else None)
        self.table.setRowCount(len(nodes))
        for row, node in enumerate(nodes):
            for column, (key, _title) in enumerate(COLUMNS):
                value = node.get(key)
                item = QTableWidgetItem("" if value is None else str(value))
                if key != "url":
                    item.setTextAlignment(Qt.AlignRight | Qt.AlignVCenter)
                self.table.setItem(row, column, item)
        last = self.offset + len(nodes)
        self.foot_text.setText(
            trf(
                "{first}–{last} из {total}",
                first=number(self.offset + 1 if nodes else 0),
                last=number(last),
                total=number(total) or "—",
            )
        )
        self.prev.setEnabled(self.offset > 0)
        self.next.setEnabled(bool(payload.get("node_has_more")))

    def _failed(self, token, reason):
        if token == self.token:
            self._reset_view()
            self._show("partial", "Граф недоступен", reason)

    def _go(self, step):
        self.offset = max(0, self.offset + step * PAGE)
        self._reload()

    def _show(self, kind, title, text=""):
        self._clear_panel()
        self.panel.addWidget(StatePanel(kind, title, text))

    def _clear_panel(self):
        while self.panel.count():
            widget = self.panel.takeAt(0).widget()
            if widget is not None:
                widget.setParent(None)
                widget.deleteLater()
