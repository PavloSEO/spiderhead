"""Settings dialog: section list with search on the left, section page on the right, 'Готово' footer."""

from __future__ import annotations

from PyQt5.QtCore import QSize, Qt
from PyQt5.QtWidgets import (
    QDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QScrollArea,
    QSizePolicy,
    QStackedWidget,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from ... import i18n, theming
from ..controls import SettingRow
from ..icons import material_icon
from . import sections, wiring
from .context import SettingsContext


class SettingsDialog(QDialog):
    def __init__(self, store, context=None, parent=None, section="general"):
        super().__init__(parent)
        self.store = store
        self.context = context or SettingsContext()
        self.setWindowTitle("Настройки")
        self.setModal(True)
        self.resize(1180, 780)
        self.setMinimumSize(640, 480)
        self._sections = sections()
        self._pages = {}
        self._nav = {}

        root = QHBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        nav = QFrame()
        nav.setObjectName("settingsNav")
        nav.setFixedWidth(260)
        nav_layout = QVBoxLayout(nav)
        nav_layout.setContentsMargins(12, 16, 12, 12)
        nav_layout.setSpacing(2)
        title = QLabel("Настройки")
        title.setProperty("text_style", "dialog")
        nav_layout.addWidget(title)
        self.search = QLineEdit()
        self.search.setPlaceholderText("Найти настройку…")
        self.search.setAccessibleName("Найти настройку")
        self.search.setClearButtonEnabled(True)
        self.search.textChanged.connect(self.filter_sections)
        nav_layout.addWidget(self.search)
        nav_layout.addSpacing(8)
        for module in self._sections:
            button = QToolButton()
            button.setObjectName("settingsNavItem")
            button.setText(module.TITLE)
            button.setIcon(material_icon(module.ICON))
            button.setIconSize(QSize(20, 20))
            button.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
            button.setCheckable(True)
            button.setAutoExclusive(True)
            button.setAccessibleName(module.TITLE)
            button.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
            button.clicked.connect(lambda _c, sid=module.ID: self.show_section(sid))
            nav_layout.addWidget(button)
            self._nav[module.ID] = button
        nav_layout.addStretch(1)
        root.addWidget(nav)

        right = QVBoxLayout()
        right.setContentsMargins(0, 0, 0, 0)
        right.setSpacing(0)
        header = QHBoxLayout()
        header.setContentsMargins(36, 28, 36, 8)
        titles = QVBoxLayout()
        titles.setSpacing(4)
        self.section_title = QLabel()
        self.section_title.setProperty("text_style", "title")
        self.section_hint = QLabel()
        self.section_hint.setProperty("text_style", "meta")
        self.section_hint.setWordWrap(True)
        titles.addWidget(self.section_title)
        titles.addWidget(self.section_hint)
        header.addLayout(titles, 1)
        self.reset_button = QPushButton("Сбросить раздел")
        self.reset_button.setProperty("role", "text")
        self.reset_button.setIcon(material_icon("restart_alt", theming.roles()["primary"]))
        self.reset_button.clicked.connect(self.reset_section)
        header.addWidget(self.reset_button, 0, Qt.AlignTop)
        right.addLayout(header)
        self.stack = QStackedWidget()
        right.addWidget(self.stack, 1)
        footer = QFrame()
        footer.setObjectName("dialogFooter")
        footer.setFixedHeight(60)
        footer_layout = QHBoxLayout(footer)
        footer_layout.setContentsMargins(24, 0, 24, 0)
        self.footer_hint = QLabel("Изменения сохраняются на этом компьютере")
        self.footer_hint.setProperty("text_style", "meta")
        footer_layout.addWidget(self.footer_hint, 1)
        done = QPushButton("Готово")
        done.setProperty("role", "primary")
        done.setProperty("size", "lg")
        done.setDefault(True)
        done.clicked.connect(self.accept)
        footer_layout.addWidget(done)
        right.addWidget(footer)
        root.addLayout(right, 1)
        i18n.retranslate(self)
        self.show_section(section)

    def _build(self, module):
        page = module.build_page(self.store, self.context)
        wiring.apply(page)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        holder = QWidget()
        holder_layout = QVBoxLayout(holder)
        holder_layout.setContentsMargins(36, 8, 36, 24)
        page.setMaximumWidth(760)
        holder_layout.addWidget(page)
        holder_layout.addStretch(1)
        scroll.setWidget(holder)
        self.stack.addWidget(scroll)
        i18n.retranslate(scroll)
        self._pages[module.ID] = (scroll, page)
        return scroll

    def current_section(self):
        return next((sid for sid, b in self._nav.items() if b.isChecked()), None)

    def show_section(self, section_id):
        module = next((m for m in self._sections if section_id == m.ID), self._sections[0])
        if module.ID not in self._pages:
            self._build(module)
        self._nav[module.ID].setChecked(True)
        self.stack.setCurrentWidget(self._pages[module.ID][0])
        self.section_title.setText(i18n.tr(module.TITLE))
        self.section_hint.setText(i18n.tr(module.HINT))

    def reset_section(self):
        section_id = self.current_section()
        module = next(m for m in self._sections if section_id == m.ID)
        self.store.reset(module.ID + ".")
        scroll, _page = self._pages.pop(section_id)
        self.stack.removeWidget(scroll)
        scroll.deleteLater()
        self.show_section(section_id)

    def filter_sections(self, text):
        """Hide sections with no match in their title or any row title; jump to the first match."""
        needle = text.strip().lower()
        first = None
        for module in self._sections:
            if module.ID not in self._pages:
                self._build(module)
            rows = [r.title.text().lower() for r in self._pages[module.ID][1].findChildren(SettingRow)]
            hit = not needle or needle in i18n.tr(module.TITLE).lower() or any(needle in r for r in rows)
            self._nav[module.ID].setVisible(hit)
            if hit and first is None:
                first = module.ID
        if needle and first and not self._nav[self.current_section()].isVisible():
            self.show_section(first)
