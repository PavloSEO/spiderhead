"""UrlGraph: the local link graph around the selected URL (design v2 «Локальный граф URL»).

Every node and edge comes from ``scan-link-inspect --view links`` of the saved scan. The graph grows by a bounded
breadth-first walk from the URL: each expanded URL costs two core calls (outgoing and incoming links), the walk stops
at the chosen depth, at ``MAX_EXPANDED`` expanded URLs and at ``MAX_NODES`` nodes. A link the scan does not hold is
never drawn. Read-only: nothing is written to the project.
"""

from __future__ import annotations

import math
from collections import deque
from urllib.parse import urlsplit

from PyQt5 import sip
from PyQt5.QtCore import QPointF, Qt, QUrl, pyqtSignal
from PyQt5.QtGui import QBrush, QColor, QDesktopServices, QFont, QPainter, QPen
from PyQt5.QtWidgets import (
    QFrame,
    QGraphicsEllipseItem,
    QGraphicsLineItem,
    QGraphicsScene,
    QGraphicsSimpleTextItem,
    QGraphicsView,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QStackedWidget,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import theming
from ..i18n import tr, trf
from ..ui.kit import StatePanel
from .scan_common import number
from .url_query import CoreJob, links_arguments

MAX_DEPTH = 3
PANEL_ROWS = 6            # links listed in the side panel; the table sheet lists the rest
DEFAULT_DEPTH = 2
MAX_NODES = 60
MAX_EXPANDED = 30          # URLs whose links are read: two core calls each
PAGE_LIMIT = 200           # core page size per direction; a fuller list is marked as truncated
RING = 120                 # radial distance between depths
COLUMN = 190               # horizontal distance between depths (left/right layout)
ROW = 36                   # vertical distance between nodes of one column
SECTION_ROLES = ("primary", "warning", "success", "text_3")   # sections get colours in the order they first appear
LAYOUTS = (("lr", "Вход слева · выход справа"), ("radial", "Радиально"))


def clear_layout(layout):
    while layout.count():
        widget = layout.takeAt(0).widget()
        if widget is not None:
            widget.setParent(None)
            widget.deleteLater()


def section_of(url):
    """The first path segment of the URL: «/» for the home page. Colours follow the order sections first appear."""
    parts = [part for part in urlsplit(url).path.split("/") if part]
    return parts[0] if parts else "/"


class EdgeItem(QGraphicsLineItem):
    """A link line; its anchor text shows at the midpoint while the pointer is on the line."""

    def __init__(self, start, end, anchor, pen, text_color):
        super().__init__(start.x(), start.y(), end.x(), end.y())
        self.setPen(pen)
        self.setAcceptHoverEvents(True)
        self.label = QGraphicsSimpleTextItem(anchor, self)
        self.label.setBrush(QBrush(text_color))
        self.label.setZValue(2)
        middle = self.line().center()
        self.label.setPos(middle.x() + 6, middle.y() - 6)
        self.label.hide()

    def hoverEnterEvent(self, event):
        self.label.show()
        super().hoverEnterEvent(event)

    def hoverLeaveEvent(self, event):
        self.label.hide()
        super().hoverLeaveEvent(event)


class GraphPage(QWidget):
    """UrlGraph sheet of the URL card: canvas with depth and layout controls, and the side panel of the selected URL."""

    table_requested = pyqtSignal()

    def __init__(self, ctx, parent=None):
        super().__init__(parent)
        self.ctx = ctx
        self.depth = DEFAULT_DEPTH
        self.layout_mode = "lr"
        self.panel_direction = "in"
        self.revision = 0
        self.loaded_for = None
        self.cache = {}        # (url, direction) -> core answer
        self.totals = {}       # (url, direction) -> total links in the scan
        self.job = CoreJob(ctx.host_ref, self)
        self.job.done.connect(self._done)
        self.job.failed.connect(self._failed)
        self._reset_walk()
        theming.signals.changed.connect(self._theme_changed)
        self.destroyed.connect(lambda *_: theming.signals.changed.disconnect(self._theme_changed))

        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        bar = QHBoxLayout()
        bar.setContentsMargins(12, 6, 12, 6)
        bar.setSpacing(8)
        self.depth_label = QLabel()
        self.depth_label.setProperty("text_style", "meta")
        bar.addWidget(self.depth_label)
        self.depth_buttons = {}
        for level in range(1, MAX_DEPTH + 1):
            button = QToolButton()
            button.setProperty("pill", "group")
            button.setCheckable(True)
            button.setText(str(level))
            button.setChecked(level == self.depth)
            button.clicked.connect(lambda _c=False, n=level: self.set_depth(n))
            self.depth_buttons[level] = button
            bar.addWidget(button)
        self.layout_buttons = {}
        for key, _label in LAYOUTS:
            button = QToolButton()
            button.setProperty("pill", "group")
            button.setCheckable(True)
            button.setChecked(key == self.layout_mode)
            button.clicked.connect(lambda _c=False, k=key: self.set_layout(k))
            self.layout_buttons[key] = button
            bar.addWidget(button)
        bar.addStretch(1)
        self.caption = QLabel()
        self.caption.setProperty("text_style", "meta")
        bar.addWidget(self.caption)
        self.help = QToolButton()
        self.help.setProperty("pill", "group")
        self.help.setText("?")
        self.help.setAccessibleName(tr("Как читать граф"))
        self.help.setToolTip(tr("Узел — URL скана, линия — ссылка. Цвет узла — HTTP-статус. Граф строится из сохранённых ссылок, только чтение."))
        bar.addWidget(self.help)
        layout.addLayout(bar)

        self.stack = QStackedWidget()
        layout.addWidget(self.stack, 1)
        body = QWidget()
        body_layout = QHBoxLayout(body)
        body_layout.setContentsMargins(0, 0, 0, 0)
        body_layout.setSpacing(0)
        self.scene = QGraphicsScene(self)
        self.view = QGraphicsView(self.scene)
        self.view.setRenderHints(QPainter.Antialiasing | QPainter.TextAntialiasing)
        self.view.setFrameShape(QFrame.NoFrame)
        self.view.setAccessibleName(tr("Граф ссылок вокруг URL"))
        body_layout.addWidget(self.view, 1)
        body_layout.addWidget(self._side_panel())
        self.stack.addWidget(body)
        self.state_host = QWidget()
        self.state_layout = QVBoxLayout(self.state_host)
        self.state_layout.setContentsMargins(0, 0, 0, 0)
        self.stack.addWidget(self.state_host)
        self._labels()

    # ---- side panel ---------------------------------------------------------------------------------------------
    def _side_panel(self):
        panel = QFrame()
        panel.setProperty("aside", True)
        panel.setFixedWidth(330)
        layout = QVBoxLayout(panel)
        layout.setContentsMargins(16, 16, 16, 16)
        layout.setSpacing(8)
        self.panel_title = QLabel()
        self.panel_title.setProperty("text_style", "section")
        layout.addWidget(self.panel_title)
        self.panel_url = QLabel()
        self.panel_url.setWordWrap(True)
        self.panel_url.setProperty("text_style", "meta")
        layout.addWidget(self.panel_url)
        toggles = QHBoxLayout()
        toggles.setSpacing(6)
        self.dir_buttons = {}
        for direction in ("in", "out"):
            button = QToolButton()
            button.setProperty("pill", "group")
            button.setCheckable(True)
            button.setChecked(direction == self.panel_direction)
            button.clicked.connect(lambda _c=False, d=direction: self.set_panel_direction(d))
            self.dir_buttons[direction] = button
            toggles.addWidget(button)
        toggles.addStretch(1)
        layout.addLayout(toggles)
        self.rows_host = QWidget()
        self.rows_layout = QVBoxLayout(self.rows_host)
        self.rows_layout.setContentsMargins(0, 0, 0, 0)
        self.rows_layout.setSpacing(6)
        layout.addWidget(self.rows_host)
        self.open_button = QPushButton()
        self.open_button.clicked.connect(self._open_url)
        layout.addWidget(self.open_button)
        self.table_button = QPushButton()
        self.table_button.setProperty("role", "primary")
        self.table_button.clicked.connect(self.table_requested.emit)
        layout.addWidget(self.table_button)
        layout.addStretch(1)
        self.legend = QLabel()
        self.legend.setWordWrap(True)
        self.legend.setProperty("text_style", "meta")
        layout.addWidget(self.legend)
        self.footer = QLabel()
        self.footer.setWordWrap(True)
        self.footer.setProperty("text_style", "meta")
        layout.addWidget(self.footer)
        return panel

    def _labels(self):
        self.depth_label.setText(tr("Глубина"))
        for key, text in LAYOUTS:
            self.layout_buttons[key].setText(tr(text))
            self.layout_buttons[key].setMinimumWidth(self.layout_buttons[key].sizeHint().width())
        self.panel_title.setText(tr("Ссылки этой страницы"))
        self.open_button.setText(tr("Открыть URL"))
        self.table_button.setText(tr("Входящие/исходящие таблицей"))
        self.footer.setText(tr("Локальный граф по сохранённому запуску · только чтение"))
        self.legend.setText(tr("Цвет — раздел URL. Пунктир — ответ 4xx или 5xx. Анкор — при наведении на ребро."))
        self._update_panel()

    def retranslate(self):
        self._labels()
        self.draw()

    def _theme_changed(self, _name):
        self.draw()

    def _update_panel(self):
        url = self.ctx.url or ""
        self.panel_url.setText(url)
        self.open_button.setEnabled(url.startswith(("http://", "https://")))
        for direction, text in (("in", "Входящие"), ("out", "Исходящие")):
            total = self.totals.get((url, direction))
            value = number(total) if type(total) is int else "…"
            self.dir_buttons[direction].setText(f"{tr(text)} · {value}")
        clear_layout(self.rows_layout)
        payload = self.cache.get((url, self.panel_direction)) or {}
        for link in (payload.get("items") or [])[:PANEL_ROWS]:
            other = link.get("target_url") if self.panel_direction == "out" else link.get("source_url")
            status = link.get("target_status") if self.panel_direction == "out" else link.get("source_status")
            label = QLabel()
            label.setWordWrap(True)
            label.setProperty("text_style", "meta")
            label.setText(f"{status if type(status) is int else tr('Нет ответа')} · {other}")
            label.setToolTip(link.get("anchor") or tr("[без текста]"))
            self.rows_layout.addWidget(label)
        if not payload.get("items") and url:
            self.rows_layout.addWidget(QLabel(tr("Нет данных")))

    def set_panel_direction(self, direction):
        self.panel_direction = direction
        for name, button in self.dir_buttons.items():
            button.setChecked(name == direction)
        self._update_panel()

    def _open_url(self):
        if self.ctx.url and self.ctx.url.startswith(("http://", "https://")):
            QDesktopServices.openUrl(QUrl(self.ctx.url))

    # ---- controls ----------------------------------------------------------------------------------------------------
    def set_depth(self, level):
        self.depth = level
        for number_, button in self.depth_buttons.items():
            button.setChecked(number_ == level)
        if self.ctx.url:
            self.start_walk()

    def set_layout(self, key):
        self.layout_mode = key
        for name, button in self.layout_buttons.items():
            button.setChecked(name == key)
        self.draw()

    # ---- context -----------------------------------------------------------------------------------------------------
    def context_changed(self):
        """A different URL (or none): the old walk and its answers are dropped; the graph reloads when shown."""
        self.job.cancel()
        self.cache = {}
        self.totals = {}
        self.loaded_for = None
        self._reset_walk()
        self._update_panel()
        self.scene.clear()

    def activate(self):
        key = (self.ctx.scan, self.ctx.url)
        if self.ctx.url and self.loaded_for != key:
            self.loaded_for = key
            self.start_walk()

    # ---- walk ----------------------------------------------------------------------------------------------------------
    def _reset_walk(self):
        self.nodes = {}        # url -> depth
        self.side = {}         # url -> "center" | "out" | "in" (branch used by the left/right layout)
        self.status = {}       # url -> HTTP status known from the scan
        self.edges = {}        # (source, target) -> anchor
        self.capped = False
        self.expanded = 0
        self.pending = deque()
        self.reads = deque()
        self.reading = None
        self.partial = set()   # (url, direction) whose list is longer than one page

    def start_walk(self):
        if not self.ctx.scan or not self.ctx.url:
            return
        self.revision += 1
        self.job.cancel()
        self._reset_walk()
        center = self.ctx.url
        self.nodes[center] = 0
        self.side[center] = "center"
        self.status[center] = (self.ctx.page or {}).get("status_code")
        self.pending.append(center)
        self._update_panel()
        self._show_state("loading", tr("Читаю ссылки…"))
        self._step()

    def _step(self):
        """Advance the walk one core call at a time; cached answers are absorbed without a call."""
        while True:
            if not self.reads:
                if not self.pending:
                    return self._finish()
                url = self.pending.popleft()
                level = self.nodes[url]
                if level >= self.depth:
                    continue
                if self.expanded >= MAX_EXPANDED:
                    self.capped = True
                    continue
                self.expanded += 1
                self.reads = deque([(url, "out"), (url, "in")])
            url, direction = self.reads.popleft()
            if (url, direction) in self.cache:
                self._absorb(url, direction)
                continue
            self.reading = (url, direction)
            self.caption.setText(trf("Читаю связи: {n} URL", n=number(self.expanded)))
            self.job.start(links_arguments(self.ctx.scan, url, direction=direction, limit=PAGE_LIMIT), ("graph", self.revision, url, direction))
            return

    def _absorb(self, url, direction):
        payload = self.cache[(url, direction)]
        level = self.nodes[url]
        for link in payload.get("items") or []:
            if direction == "out":
                other, status, source, target = link.get("target_url"), link.get("target_status"), url, link.get("target_url")
            else:
                other, status, source, target = link.get("source_url"), link.get("source_status"), link.get("source_url"), url
            if not isinstance(other, str) or not other:
                continue
            if other not in self.nodes:
                if len(self.nodes) >= MAX_NODES:
                    self.capped = True
                    continue
                self.nodes[other] = level + 1
                self.side[other] = (direction if url == self.ctx.url else self.side[url])
                self.pending.append(other)
            if type(status) is int:
                self.status[other] = status
            if isinstance(source, str) and isinstance(target, str) and source in self.nodes and target in self.nodes:
                self.edges.setdefault((source, target), link.get("anchor") or "")
        if payload.get("has_more") or (type(payload.get("total")) is int and payload["total"] > PAGE_LIMIT):
            self.partial.add((url, direction))

    def _finish(self):
        self.reading = None
        if len(self.nodes) <= 1 and not self.edges:
            self._show_state("empty", tr("Ссылок нет"), tr("Ядро не нашло ссылок этой страницы в сохранённом скане."))
            self.caption.setText("")
            return
        self.stack.setCurrentIndex(0)
        self.draw()
        parts = [trf("Узлов: {n}", n=number(len(self.nodes))), trf("связей: {n}", n=number(len(self.edges)))]
        if self.capped or self.partial:
            parts.append(tr("показано не всё: лимит графа"))
        self.caption.setText(" · ".join(parts))
        self._update_panel()

    def _done(self, token, payload):
        if self.reading is None or token != ("graph", self.revision, *self.reading):
            return
        key = self.reading
        if payload.get("ok") is not True:
            return self._show_state("error", tr("Не удалось прочитать граф"), payload.get("error") or payload.get("reason_code") or tr("Ядро не выполнило запрос"))
        self.cache[key] = payload
        if type(payload.get("total")) is int:
            self.totals[key] = payload["total"]
        self._absorb(*key)
        self._step()

    def _failed(self, token, text):
        if self.reading is not None and token == ("graph", self.revision, *self.reading):
            self._show_state("error", tr("Не удалось прочитать граф"), text)

    def _show_state(self, kind, title, text=""):
        clear_layout(self.state_layout)
        self.state_layout.addWidget(StatePanel(kind, title, text))
        self.stack.setCurrentIndex(1)

    # ---- drawing -------------------------------------------------------------------------------------------------------
    def positions(self):
        """Left/right: depth sets the column, the branch sets the side. Radial: depth sets the ring."""
        pos = {}
        groups = {}
        for url, level in self.nodes.items():
            groups.setdefault((level, self.side[url]), []).append(url)
        for group in groups.values():
            group.sort()
        if self.layout_mode == "radial":
            ordered = sorted(self.nodes, key=lambda u: (self.nodes[u], self.side[u], u))
            ring_counts = {}
            for url in ordered:
                ring_counts[self.nodes[url]] = ring_counts.get(self.nodes[url], 0) + 1
            index = {}
            for url in ordered:
                level = self.nodes[url]
                if level == 0:
                    pos[url] = QPointF(0, 0)
                    continue
                i = index.get(level, 0)
                index[level] = i + 1
                angle = 2 * math.pi * i / ring_counts[level] - math.pi / 2
                pos[url] = QPointF(RING * level * math.cos(angle), RING * level * math.sin(angle))
            return pos
        for (level, side), group in groups.items():
            if level == 0:
                pos[group[0]] = QPointF(0, 0)
                continue
            sign = -1 if side == "in" else 1
            for i, url in enumerate(group):
                y = (i - (len(group) - 1) / 2) * ROW
                pos[url] = QPointF(sign * COLUMN * level, y)
        return pos

    def draw(self):
        if sip.isdeleted(self) or self.stack.currentIndex() != 0 or not self.nodes:
            return
        roles = theming.roles()
        self.scene.clear()
        self.view.setBackgroundBrush(QBrush(QColor(roles["raised"])))
        pos = self.positions()
        edge_color = QColor(roles["text_muted"])
        edge_color.setAlpha(90)
        edge_pen = QPen(edge_color, 0.9)
        edge_pen.setCosmetic(True)
        anchor_color = QColor(roles["text"])
        for (source, target), anchor in self.edges.items():
            if source not in pos or target not in pos:
                continue
            line = EdgeItem(pos[source], pos[target], anchor or tr("[без текста]"), edge_pen, anchor_color)
            self.scene.addItem(line)
        label_font = QFont()
        label_font.setPointSizeF(9.5)
        sections = {}
        for url, point in pos.items():
            level = self.nodes[url]
            radius = 11 if level == 0 else 8 if level == 1 else 5.5
            section = section_of(url)
            if section not in sections:
                sections[section] = SECTION_ROLES[len(sections) % len(SECTION_ROLES)] if section != "/" else "text_muted"
            fill = QColor(roles[sections[section]])
            node = QGraphicsEllipseItem(point.x() - radius, point.y() - radius, radius * 2, radius * 2)
            node.setBrush(QBrush(fill))
            broken = type(self.status.get(url)) is int and self.status[url] >= 400
            node.setPen(QPen(QColor(roles["error"]), 2, Qt.DashLine) if broken else QPen(QColor(roles["base"]), 1.2))
            node.setToolTip(url)
            node.setAcceptHoverEvents(True)
            if url == self.ctx.url:
                node.setPen(QPen(QColor(roles["primary"]), 3))
            self.scene.addItem(node)
            if level == 0 or (level == 1 and self.layout_mode == "lr"):
                text = QGraphicsSimpleTextItem(self._short(url))
                text.setFont(label_font)
                text.setBrush(QBrush(QColor(roles["text"] if level == 0 else roles["text_2"])))
                text.setZValue(1)
                if level == 0:  # the centre keeps its label under the node, away from both columns
                    text.setPos(point.x() - text.boundingRect().width() / 2, point.y() + radius + 6)
                elif self.side[url] == "in" and self.layout_mode == "lr":
                    text.setPos(point.x() - radius - 4 - text.boundingRect().width(), point.y() - 8)
                else:
                    text.setPos(point.x() + radius + 4, point.y() - 8)
                self.scene.addItem(text)
        if self.layout_mode == "lr":
            self._column_heads(pos, roles)
        rect = self.scene.itemsBoundingRect().adjusted(-48, -56, 48, 48)
        self.scene.setSceneRect(rect)
        self.view.fitInView(rect, Qt.KeepAspectRatio)

    def _column_heads(self, pos, roles):
        """«← Входящие · N» over the left column and «Исходящие · N →» over the right one, as the board shows them."""
        xs = [point.x() for point in pos.values()]
        top = min(point.y() for point in pos.values()) - 36
        url = self.ctx.url or ""
        heads = ((min(xs) - 40, "in", "← " + tr("Входящие")), (max(xs) + 40, "out", tr("Исходящие") + " →"))
        for x, direction, text in heads:
            total = self.totals.get((url, direction))
            item = QGraphicsSimpleTextItem(f"{text} · {number(total)}" if type(total) is int else text)
            font = QFont()
            font.setPointSizeF(10.5)
            font.setWeight(QFont.Medium)
            item.setFont(font)
            item.setBrush(QBrush(QColor(roles["text_2"])))
            if direction == "out":
                x -= item.boundingRect().width()
            item.setPos(x, top)
            self.scene.addItem(item)

    @staticmethod
    def _short(url):
        return url.split("://", 1)[-1][:60]

    def resizeEvent(self, event):
        super().resizeEvent(event)
        if self.scene.items():
            self.view.fitInView(self.scene.sceneRect(), Qt.KeepAspectRatio)


class LinkGraphScreen(QWidget):
    """The «Граф ссылок» section: the GraphPage around the URL picked on the URL screen; an honest empty state without one."""

    def __init__(self, host, parent=None):
        super().__init__(parent)
        from .url_card import Context  # imported here: url_card imports this module

        self.host = host
        self.ctx = Context()
        self.ctx.host_ref = host
        self.key = None
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        self.stack = QStackedWidget()
        layout.addWidget(self.stack)
        self.empty = StatePanel("empty", tr("Выберите URL в таблице"), tr("Граф строится вокруг URL, выбранного на экране URL."))
        self.stack.addWidget(self.empty)
        self.graph = GraphPage(self.ctx)
        self.graph.table_requested.connect(lambda: self._open_urls())
        self.stack.addWidget(self.graph)
        self.stack.setCurrentWidget(self.empty)

    def _open_urls(self):
        self.host.navigation.select_section("url")

    def showEvent(self, event):
        super().showEvent(event)
        self.refresh()

    def refresh(self):
        """Follow the URL selected on the URL screen; a new scan or URL drops the old walk and reads again."""
        url_screen = getattr(self.host, "screens", {}).get("url")
        url = getattr(url_screen, "current_url", None) if url_screen is not None else None
        scan = getattr(self.host, "selected_scan_path", None)
        if not scan or not url:
            self.key = None
            self.ctx.scan = self.ctx.url = None
            self.graph.context_changed()
            self.stack.setCurrentWidget(self.empty)
            return
        key = (scan, url)
        if key != self.key:
            self.key = key
            self.ctx.scan = scan
            self.ctx.url = url
            self.ctx.detail = getattr(url_screen, "detail_data", None) if url_screen is not None else None
            self.graph.context_changed()
        self.stack.setCurrentWidget(self.graph)
        self.graph.activate()
