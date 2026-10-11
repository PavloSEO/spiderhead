"""Граф ссылок (canvas LinkGraph.dc.html): the link graph of a saved scan, with its toolbar, canvas and filter aside.

The core answers link questions per URL (``scan_link_inspect``) but has no bounded whole-graph read yet (#975), so the
canvas shows an honest state instead of a drawing. The colour, size and filter controls keep the sheet's layout and are
disabled with the reason; no sample graph is drawn and no count is invented.
"""

from __future__ import annotations

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QFrame, QHBoxLayout, QLabel, QLineEdit, QPushButton, QScrollArea, QStackedWidget, QToolButton, QVBoxLayout

from ..i18n import tr
from ..ui.icons import material_icon
from ..ui.kit import StatePanel, no_project_panel, unavailable_tip, waiting_badge
from .base import Screen

GRAPH_ISSUE = 975
GRAPH_HINT = "Граф сохранённого скана: узлы и рёбра по разделу или вокруг URL"
COLOR_MODES = ("Раздел", "Код ответа", "Индексируемость", "Глубина")
SIZE_MODES = ("Входящие", "PageRank-внутр.", "Слова")
FILTER_SECTIONS = ("Учитывать ссылки из", "Разделы", "Глубина от главной")
UNAVAILABLE_REASON = "Граф не загружен: ядро не отдаёт узлы и рёбра скана одним запросом"


def group(buttons, label):
    """A labelled pill group in the toolbar; all buttons stay disabled until the graph can be drawn."""
    caption = QLabel(tr(label))
    caption.setProperty("text_style", "meta")
    buttons_row = QHBoxLayout()
    buttons_row.setSpacing(4)
    buttons_row.addWidget(caption)
    for index, text in enumerate(buttons):
        button = QToolButton()
        button.setProperty("pill", "group")
        button.setCheckable(True)
        button.setChecked(index == 0)
        button.setText(tr(text))
        button.setEnabled(False)
        button.setToolTip(unavailable_tip(UNAVAILABLE_REASON))
        button.setMinimumWidth(button.sizeHint().width())  # whole words stay readable; the toolbar scrolls instead
        buttons_row.addWidget(button)
    return buttons_row


class LinkGraphScreen(Screen):
    slot = "graph"
    watches = ("project", "scan")

    def __init__(self, host):
        super().__init__(host)
        self.state = None
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        root.addWidget(self._toolbar())
        middle = QHBoxLayout()
        middle.setContentsMargins(0, 0, 0, 0)
        middle.setSpacing(0)
        middle.addWidget(self._canvas(), 1)
        middle.addWidget(self._aside())
        root.addLayout(middle, 1)
        root.addWidget(self._footer())
        self.refresh()

    def _toolbar(self):
        bar = QFrame()
        bar.setProperty("page_bar", True)
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(12, 0, 12, 0)
        layout.setSpacing(10)
        self.search = QLineEdit()
        self.search.setPlaceholderText(tr("Найти узел: адрес…"))
        self.search.setAccessibleName(tr("Найти узел"))
        self.search.setFixedWidth(200)
        self.search.setEnabled(False)
        self.search.setToolTip(unavailable_tip(UNAVAILABLE_REASON))
        self.search.addAction(material_icon("search"), QLineEdit.LeadingPosition)
        layout.addWidget(self.search)
        layout.addLayout(group(COLOR_MODES, "Цвет"))
        layout.addLayout(group(SIZE_MODES, "Размер"))
        layout.addStretch(1)
        scroll = QScrollArea()  # a narrow window scrolls the toolbar rather than squeezing its labels
        scroll.setFrameShape(QFrame.NoFrame)
        scroll.setWidgetResizable(True)
        scroll.setVerticalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        scroll.setFixedHeight(46)
        scroll.setWidget(bar)
        return scroll

    def _canvas(self):
        self.canvas_host = QStackedWidget()
        self.canvas_host.setProperty("canvas", True)
        return self.canvas_host

    def _aside(self):
        aside = QFrame()
        aside.setProperty("aside", True)
        aside.setFixedWidth(330)
        layout = QVBoxLayout(aside)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        head = QFrame()
        head.setProperty("aside_head", True)
        head.setFixedHeight(40)
        head_layout = QHBoxLayout(head)
        head_layout.setContentsMargins(16, 0, 8, 0)
        title = QLabel(tr("Фильтры графа"))
        title.setProperty("text_style", "section")
        head_layout.addWidget(title)
        head_layout.addStretch(1)
        reset = QPushButton(tr("Сбросить"))
        reset.setProperty("role", "text")
        reset.setEnabled(False)
        reset.setToolTip(unavailable_tip(UNAVAILABLE_REASON))
        head_layout.addWidget(reset)
        layout.addWidget(head)
        body = QVBoxLayout()
        body.setContentsMargins(16, 12, 16, 12)
        body.setSpacing(14)
        for section in FILTER_SECTIONS:
            caption = QLabel(tr(section))
            caption.setProperty("text_style", "overline")
            body.addWidget(caption)
            note = QLabel(tr("Нет данных"))
            note.setProperty("text_style", "meta")
            note.setToolTip(unavailable_tip(UNAVAILABLE_REASON))
            body.addWidget(note)
        body.addStretch(1)
        layout.addLayout(body, 1)
        return aside

    def _footer(self):
        bar = QFrame()
        bar.setProperty("table_head", True)
        bar.setFixedHeight(52)
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(12, 0, 12, 0)
        layout.setSpacing(14)
        mode = QLabel(tr("Граф по сохранённому запуску · только чтение"))
        mode.setProperty("text_style", "meta")
        layout.addWidget(mode)
        layout.addStretch(1)
        self.counts = QLabel()
        self.counts.setProperty("text_style", "meta")
        layout.addWidget(self.counts)
        self.badge = waiting_badge(GRAPH_ISSUE, GRAPH_HINT)
        layout.addWidget(self.badge)
        return bar

    def _set_canvas(self, kind):
        if kind == self.state:
            return
        while self.canvas_host.count():
            old = self.canvas_host.widget(0)
            self.canvas_host.removeWidget(old)
            old.deleteLater()
        if kind == "noproject":
            panel = no_project_panel(self.host, "Откройте проект, чтобы увидеть граф ссылок его сохранённого скана.")
        else:
            panel = StatePanel("waiting", "Граф ссылок пока не показан",
                               "Ядро не отдаёт граф сохранённого скана целиком. Ссылки одной страницы — в карточке URL.",
                               hint=GRAPH_HINT)
        self.canvas_host.addWidget(panel)
        self.state = kind

    def refresh(self):
        self._set_canvas("scan" if self.project_open else "noproject")
        self.counts.setText(tr("Узлов: Нет данных · рёбер: Нет данных"))
