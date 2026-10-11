"""Screen «Граф ссылок · раскладки» (canvas GraphLayouts): the picker of the link-graph layout.

Four QGraphicsView previews show how the same graph is laid out: force, radial, clusters by section and hierarchy by
depth. The previews are schematic geometry only: no URL, count or label comes from a project, so nothing here is sample
data. The picker starts no core call and writes nothing; the choice lives in this window.
"""

from __future__ import annotations

from PyQt5.QtCore import Qt
from PyQt5.QtGui import QBrush, QColor, QFont, QPainter, QPen
from PyQt5.QtWidgets import (
    QCheckBox,
    QFrame,
    QGraphicsEllipseItem,
    QGraphicsLineItem,
    QGraphicsScene,
    QGraphicsSimpleTextItem,
    QGraphicsView,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from .. import theming
from ..i18n import tr, trf
from ..ui.icons import MaterialIconLabel
from ..ui.kit import PageHeader

PREVIEW_SIZE = (660, 300)
SECTION_ROLES = ("success", "warning", "text_3")
# node = (x, y, radius, role); role «orphan» is an unlinked page (dashed ring), an edge = (a, b[, width])
FORCE = (
    [(330, 150, 16, "primary"), (200, 95, 11, "success"), (470, 90, 11, "warning"), (455, 215, 10, "text_3"),
     (195, 215, 9, "success"), (560, 160, 8, "text_3"), (150, 60, 5, "success"), (135, 110, 5, "success"),
     (230, 50, 4, "success"), (255, 120, 6, "success"), (520, 55, 5, "warning"), (430, 45, 4, "warning"),
     (535, 100, 6, "warning"), (410, 110, 5, "warning"), (500, 250, 6, "text_3"), (410, 255, 4, "text_3"),
     (520, 205, 5, "text_3"), (140, 250, 4, "error"), (120, 200, 5, "success"), (245, 255, 4, "success"),
     (610, 250, 5, "orphan"), (60, 40, 5, "orphan")],
    [(0, 1), (0, 2), (0, 3), (0, 4), (0, 5), (1, 6), (1, 7), (1, 8), (1, 9), (2, 10), (2, 11), (2, 12), (2, 13),
     (9, 13), (3, 14), (3, 15), (3, 16), (4, 17), (4, 18), (4, 19), (6, 7), (10, 12), (14, 16)],
)
RADIAL = (
    [(330, 150, 14, "primary"), (330, 95, 9, "success"), (378, 178, 9, "warning"), (282, 178, 8, "text_3"),
     (280, 63, 5, "success"), (380, 63, 5, "success"), (428, 160, 6, "warning"), (400, 240, 4, "warning"),
     (232, 160, 5, "text_3"), (262, 245, 4, "error"), (420, 35, 4, "success"), (468, 200, 4, "warning"),
     (196, 70, 4, "text_3")],
    [(0, 1), (0, 2), (0, 3), (1, 4), (1, 5), (5, 10), (2, 6), (2, 7), (6, 11), (3, 8), (3, 9), (4, 12)],
)
CLUSTER = (
    [(330, 150, 18, "primary"), (170, 100, 34, "success"), (490, 100, 46, "warning"),
     (470, 225, 26, "text_3"), (190, 225, 16, "text_2")],
    [(0, 1, 6), (0, 2, 9), (0, 3, 3), (0, 4, 2), (1, 2, 7), (2, 3, 5), (1, 4, 2)],
)
TREE = (
    [(60, 150, 12, "primary"), (160, 70, 8, "success"), (160, 150, 8, "warning"), (160, 230, 7, "text_3"),
     (260, 40, 5, "success"), (260, 100, 6, "success"), (260, 150, 5, "warning"), (260, 210, 4, "text_3"),
     (260, 260, 4, "error"), (360, 30, 4, "success"), (360, 80, 4, "success"), (360, 120, 5, "success"),
     (360, 200, 3, "text_3"), (460, 120, 4, "success"), (460, 230, 4, "orphan")],
    [(0, 1), (0, 2), (0, 3), (1, 4), (1, 5), (2, 6), (3, 7), (3, 8), (4, 9), (5, 10), (5, 11), (7, 12),
     (8, 14), (11, 13)],
)
LAYOUTS = (
    ("force", "hub", "Силовая, как в Obsidian", "разделы сами собираются в облака", FORCE),
    ("radial", "sensors", "Радиальная от главной", "кольцо = клики от главной", RADIAL),
    ("cluster", "workspaces", "Кластеры по разделам", "раздел = один узел, «раскрыть» по клику", CLUSTER),
    ("tree", "account_tree", "Иерархия по глубине", "слева направо: глубина 0 → 4", TREE),
)
LEGEND = (
    ("primary", "Главная страница"),
    ("success", "Раздел — свой цвет"),
    ("error", "Ответ 4xx"),
    ("orphan", "Сирота: нет входящих ссылок"),
)


def _color(roles, name, width_alpha=255):
    color = QColor(roles[name])
    color.setAlpha(width_alpha)
    return color


def _text(scene, text, x, y, roles, size=9.5, role="text_muted"):
    item = QGraphicsSimpleTextItem(text)
    font = QFont()
    font.setPointSizeF(size)
    item.setFont(font)
    item.setBrush(QBrush(_color(roles, role)))
    item.setPos(x, y)
    scene.addItem(item)


def paint_preview(scene, key, roles):
    """Draws one layout; the geometry is fixed, the colours are theme roles."""
    scene.clear()
    scene.setBackgroundBrush(QBrush(QColor(roles["raised"])))
    scene.setSceneRect(0, 0, *PREVIEW_SIZE)
    nodes, edges = next(geometry for name, _i, _t, _h, geometry in LAYOUTS if name == key)
    edge_pen = QPen(_color(roles, "outline", 200), 1)
    edge_pen.setCosmetic(True)
    for edge in edges:
        (x1, y1, _r1, _c1), (x2, y2, _r2, _c2) = nodes[edge[0]], nodes[edge[1]]
        line = QGraphicsLineItem(x1, y1, x2, y2)
        pen = QPen(edge_pen)
        pen.setWidthF(edge[2] if len(edge) > 2 else 1)
        line.setPen(pen)
        scene.addItem(line)
    if key == "radial":
        ring_pen = QPen(_color(roles, "divider"), 1)
        ring_pen.setCosmetic(True)
        for radius in (55, 100, 140):
            ring = QGraphicsEllipseItem(330 - radius, 150 - radius, radius * 2, radius * 2)
            ring.setPen(ring_pen)
            ring.setBrush(QBrush(Qt.NoBrush))
            scene.addItem(ring)
        for label, x in (("1", 388), ("2", 433), ("3+", 473)):
            _text(scene, label, x, 140, roles)
        _text(scene, tr("Глубокие страницы (3+ клика) видны сразу"), 20, 278, roles, role="text")
    if key == "cluster":
        _text(scene, tr("толщина ребра = число ссылок между разделами"), 20, 278, roles, role="text")
    if key == "tree":
        for depth in range(5):
            _text(scene, str(depth), 60 + depth * 100 - 4, 286, roles)
    for x, y, radius, role in nodes:
        node = QGraphicsEllipseItem(x - radius, y - radius, radius * 2, radius * 2)
        if role == "orphan":
            node.setBrush(QBrush(_color(roles, "base")))
            node.setPen(QPen(_color(roles, "error"), 2, Qt.DashLine))
        else:
            node.setBrush(QBrush(_color(roles, role)))
            node.setPen(QPen(_color(roles, "base"), 1.2))
        scene.addItem(node)


class LayoutCard(QFrame):
    """One layout: icon, name, hint, the «Выбрать» button and its preview."""

    def __init__(self, key, icon, title, hint, pick, parent=None):
        super().__init__(parent)
        self.key = key
        self.setProperty("card", "panel")
        column = QVBoxLayout(self)
        column.setContentsMargins(0, 0, 0, 0)
        column.setSpacing(0)
        top = QHBoxLayout()
        top.setContentsMargins(14, 10, 14, 0)
        top.setSpacing(8)
        top.addWidget(MaterialIconLabel(icon, 18))
        name = QLabel(tr(title))
        name.setProperty("text_style", "section")
        name.setWordWrap(True)
        top.addWidget(name, 1)
        column.addLayout(top)
        bottom = QHBoxLayout()
        bottom.setContentsMargins(14, 0, 14, 10)
        bottom.setSpacing(8)
        meta = QLabel(tr(hint))
        meta.setProperty("text_style", "meta")
        meta.setWordWrap(True)
        bottom.addWidget(meta, 1)
        self.button = QPushButton(tr("Выбрать"))
        self.button.clicked.connect(lambda: pick(key))
        bottom.addWidget(self.button)
        column.addLayout(bottom)
        self.scene = QGraphicsScene(self)
        self.view = QGraphicsView(self.scene)
        self.view.setRenderHints(QPainter.Antialiasing)
        self.view.setFrameShape(QFrame.NoFrame)
        self.view.setAccessibleName(tr(title))
        self.view.setMinimumHeight(180)
        column.addWidget(self.view, 1)

    def refresh(self, roles):
        paint_preview(self.scene, self.key, roles)
        self.view.fitInView(self.scene.sceneRect(), Qt.KeepAspectRatio)

    def set_picked(self, picked):
        self.setProperty("picked", "true" if picked else "")
        self.style().unpolish(self)
        self.style().polish(self)
        self.button.setText(tr("Выбрано") if picked else tr("Выбрать"))
        self.button.setEnabled(not picked)


class GraphLayoutsScreen(QWidget):
    """Canvas GraphLayouts: a 2x2 grid of layout previews, the legend, the header toggle and the chosen layout."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.choice = LAYOUTS[0][0]
        self.setProperty("spaciousPage", False)
        self._on_theme_changed = lambda _name: self._refresh()
        theming.signals.changed.connect(self._on_theme_changed)
        self.destroyed.connect(lambda: theming.signals.changed.disconnect(self._on_theme_changed))
        root = QVBoxLayout(self)
        root.setContentsMargins(16, 12, 16, 12)
        root.setSpacing(12)
        header = PageHeader(tr("Граф ссылок · раскладки"), tr("Вид графа ссылок вокруг URL · только чтение"))
        self.header_switch = QCheckBox(tr("Учитывать шапку и подвал"))
        self.header_switch.setToolTip(tr("Ссылки шапки и подвала входят в граф, когда переключатель включён"))
        header.add_action(self.header_switch)
        root.addWidget(header)
        self.legend = QLabel()
        self.legend.setTextFormat(Qt.RichText)
        self.legend.setProperty("text_style", "meta")
        root.addWidget(self.legend)
        grid = QGridLayout()
        grid.setSpacing(12)
        self.cards = {}
        for index, (key, icon, title, hint, _geometry) in enumerate(LAYOUTS):
            card = LayoutCard(key, icon, title, hint, self.choose)
            self.cards[key] = card
            grid.addWidget(card, index // 2, index % 2)
        root.addLayout(grid, 1)
        self.footer = QLabel()
        self.footer.setProperty("text_style", "meta")
        root.addWidget(self.footer)
        self.choose(self.choice)

    def choose(self, key):
        self.choice = key
        for name, card in self.cards.items():
            card.set_picked(name == key)
        title = next(tr(t) for name, _i, t, _h, _g in LAYOUTS if name == key)
        self.footer.setText(trf("Выбрано: {name}", name=title))

    def _refresh(self):
        roles = theming.roles()
        parts = [
            f'<span style="color:{roles[role]}">●</span>&nbsp;{tr(text)}' if role != "orphan"
            else f'<span style="color:{roles["error"]}">◌</span>&nbsp;{tr(text)}'
            for role, text in LEGEND
        ]
        self.legend.setText("&nbsp;&nbsp;&nbsp;".join(parts))
        for card in self.cards.values():
            card.refresh(roles)

    def showEvent(self, event):
        super().showEvent(event)
        self._refresh()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        for card in self.cards.values():
            card.view.fitInView(card.scene.sceneRect(), Qt.KeepAspectRatio)
