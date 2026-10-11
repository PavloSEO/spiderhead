"""Screen «Справка» (canvas Help.dc.html): sections on the left, one article in the middle, data states on the right.

The text is the reviewed guide of ``ui/help_guide.py`` (one source for the dialog and the screen). The canvas sample
project and its example commands are not used: the screen shows no project data and runs no operation. A section
button only moves the window to that section, through the same navigation the sidebar uses.
"""

from __future__ import annotations

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import (
    QButtonGroup,
    QFrame,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QScrollArea,
    QVBoxLayout,
    QWidget,
)

from ..ui.help_guide import EXPLANATIONS, SECTIONS, START, VIEW_TITLES
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.presentation import StateBadge
from ..ui.workspace import VIEW_IDS
from .base import Screen

GROUPS = (("Быстрый старт", START), ("Разделы и настройки", SECTIONS))
ITEMS = tuple((group, icon, heading, text, view) for group, entries in GROUPS for icon, heading, text, view in entries)
ARTICLE_WIDTH = 760
NAV_WIDTH = 320
COMPACT_WIDTH = 1100  # below it the states column is hidden (canvas: compact window)
ASIDE_WIDTH = 300


def _label(text="", style="", wrap=True):
    label = QLabel(text)
    label.setTextFormat(Qt.PlainText)
    label.setWordWrap(wrap)
    label.setMinimumWidth(0)
    label.setTextInteractionFlags(Qt.TextSelectableByMouse | Qt.TextSelectableByKeyboard)
    if style:
        label.setProperty("text_style", style)
    return label


class HelpScreen(Screen):
    chrome_free = True  # SHELL-CANON §7: the help page has no project or scan context
    slot = ""
    watches = ()

    def __init__(self, host):
        super().__init__(host)
        self.setObjectName("helpPage")
        self.setAccessibleName("Справка")
        self.current = 0
        self.entries = []  # (item index, button, group label or None for group rows)
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        root.addWidget(self._header())
        body = QHBoxLayout()
        body.setContentsMargins(0, 0, 0, 0)
        body.setSpacing(0)
        body.addWidget(self._navigation())
        body.addWidget(self._article(), 1)
        self.aside = self._aside()
        body.addWidget(self.aside)
        root.addLayout(body, 1)
        self._show(0)

    # --- layout -------------------------------------------------------------
    def _header(self):
        bar = QFrame()
        bar.setObjectName("helpHeader")
        bar.setFixedHeight(52)
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(24, 0, 24, 0)
        layout.setSpacing(10)
        layout.addWidget(MaterialIconLabel("help", 20, bar))
        layout.addWidget(_label("Справка", "title", wrap=False))
        self.crumb = _label("", "meta", wrap=False)
        layout.addWidget(self.crumb)
        layout.addStretch()
        return bar

    def _navigation(self):
        nav = QFrame()
        nav.setObjectName("helpNav")
        nav.setFixedWidth(NAV_WIDTH)
        layout = QVBoxLayout(nav)
        layout.setContentsMargins(12, 12, 12, 12)
        layout.setSpacing(4)
        self.search = QLineEdit()
        self.search.setPlaceholderText("Поиск по справке")
        self.search.setAccessibleName("Поиск по справке")
        self.search.textChanged.connect(self._filter)
        layout.addWidget(self.search)
        layout.addSpacing(8)
        self.group = QButtonGroup(self)
        self.group.setExclusive(True)
        self.group_labels = {}
        for title, _entries in GROUPS:
            caption = _label(title.upper(), "overline", wrap=False)
            self.group_labels[title] = caption
            layout.addWidget(caption)
            for index, (group, _icon, heading, _text, _view) in enumerate(ITEMS):
                if group != title:
                    continue
                button = QPushButton(heading)
                button.setCheckable(True)
                button.setProperty("helpItem", True)
                button.setAccessibleName(heading)
                button.setCursor(Qt.PointingHandCursor)
                self.group.addButton(button, index)
                self.entries.append((index, button, caption))
                layout.addWidget(button)
        layout.addStretch()
        self.group.idClicked.connect(self._show)
        return nav

    def _article(self):
        scroll = QScrollArea()
        scroll.setObjectName("helpScroll")
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QScrollArea.NoFrame)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        content = QWidget()
        row = QHBoxLayout(content)
        row.setContentsMargins(40, 24, 40, 24)
        column = QWidget()
        column.setMaximumWidth(ARTICLE_WIDTH)
        layout = QVBoxLayout(column)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(8)
        self.article_title = _label("", "title")
        self.article_meta = _label("", "meta")
        self.article_text = _label("")
        self.open_button = QPushButton("Открыть раздел")
        self.open_button.setIcon(material_icon("open_in_new"))
        self.open_button.setProperty("role", "secondary")
        self.open_button.clicked.connect(self._open_view)
        layout.addWidget(self.article_title)
        layout.addWidget(self.article_meta)
        layout.addSpacing(8)
        layout.addWidget(self.article_text)
        layout.addSpacing(12)
        layout.addWidget(self.open_button, 0, Qt.AlignLeft)
        layout.addStretch()
        row.addWidget(column, 1)
        scroll.setWidget(content)
        return scroll

    def _aside(self):
        aside = QFrame()
        aside.setObjectName("helpAside")
        aside.setFixedWidth(ASIDE_WIDTH)
        layout = QVBoxLayout(aside)
        layout.setContentsMargins(16, 16, 16, 16)
        layout.setSpacing(12)
        layout.addWidget(_label("СОСТОЯНИЯ ДАННЫХ", "overline", wrap=False))
        for state, title, text in EXPLANATIONS:
            layout.addWidget(_label(title, "control"))
            badge = StateBadge(state)
            layout.addWidget(badge, 0, Qt.AlignLeft)
            layout.addWidget(_label(text, "meta"))
        layout.addStretch()
        # the states column is taller than a 900px window: scroll it instead of clipping the explanations
        scroll = QScrollArea()
        scroll.setObjectName("helpAsideScroll")
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QScrollArea.NoFrame)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        scroll.setFixedWidth(ASIDE_WIDTH)
        scroll.setWidget(aside)
        return scroll

    # --- behaviour ----------------------------------------------------------
    def resizeEvent(self, event):
        super().resizeEvent(event)
        self.aside.setVisible(event.size().width() >= COMPACT_WIDTH)
    def refresh(self):
        """Static content: nothing to reload when the window's data change."""

    def _show(self, index):
        group, _icon, heading, text, view = ITEMS[index]
        self.current = index
        button = self.group.button(index)
        if button is not None:
            button.setChecked(True)
        self.crumb.setText(f"Справка · {group}")
        self.article_title.setText(heading)
        self.article_meta.setText(f"{group} · {VIEW_TITLES[view]}" if view else group)
        self.article_text.setText(text)
        self.open_button.setVisible(view is not None)
        self.open_button.setEnabled(view is not None and view in VIEW_IDS)
        self.open_button.setToolTip("" if view is None else "Перейти к разделу, без запуска операций")

    def _open_view(self):
        view = ITEMS[self.current][4]
        if view not in VIEW_IDS:
            return
        row = VIEW_IDS.index(view)
        self.host.navigation.setCurrentRow(row)
        self.host.navigate(row)  # the row may already be current: the sidebar signal then stays silent

    def _filter(self, query):
        needle = query.strip().lower()
        visible_groups = set()
        for index, button, caption in self.entries:
            _group, _icon, heading, text, _view = ITEMS[index]
            shown = not needle or needle in heading.lower() or needle in text.lower()
            button.setVisible(shown)
            if shown:
                visible_groups.add(caption)
        for caption in self.group_labels.values():
            caption.setVisible(caption in visible_groups)
        if not self.group.button(self.current) or not self.group.button(self.current).isVisible():
            first = next((index for index, button, _caption in self.entries if button.isVisible()), None)
            if first is not None:
                self._show(first)
