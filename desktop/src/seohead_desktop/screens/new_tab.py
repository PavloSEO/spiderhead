"""Screen «Новая вкладка» (canvas NewTab.dc.html): pick what to open in a new workspace tab.

Entries only switch navigation sections of the window; nothing here reads data or calls the core. Saved views are
not read by this build, so the section says the list is not connected yet instead of showing sample rows.
"""

from __future__ import annotations

from PyQt5.QtCore import QSize, Qt
from PyQt5.QtWidgets import QFrame, QGridLayout, QLabel, QScrollArea, QSizePolicy, QToolButton, QVBoxLayout, QWidget

from .. import i18n
from ..ui.icons import material_icon
from ..ui.kit import PageHeader, StatePanel
from .base import Screen

tr = i18n.tr

# (navigation section, icon, title, hint) in canvas order
OPTIONS = (
    ("url", "table_view", "URL скана", "Таблица, фильтры, детали URL"),
    ("issues", "rule", "Проблемы", "Проверки, затронутые URL, перепроверка"),
    ("compare", "compare_arrows", "Сравнение", "Два скана: что улучшилось и что сломалось"),
    ("content_search", "find_in_page", "Поиск в HTML", "Текст или фрагмент кода по сохранённым страницам"),
    ("scans", "sensors", "Наблюдение", "Идущий скан: счётчики, скорость, события"),
    ("reports", "description", "Отчёт", "Задачи программисту, SEO и контенту"),
)


class NewTabScreen(Screen):
    """Chooser of sections for a new tab. Not a navigation slot: registered as an extra, like the start screen."""

    watches = ()
    chrome_free = True  # SHELL-CANON §7: no project / scan switchers in the top bar

    def __init__(self, host):
        super().__init__(host)
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        outer.addWidget(scroll)
        page = QWidget()
        page.setObjectName("newTabPage")
        scroll.setWidget(page)
        body = QVBoxLayout(page)
        body.setContentsMargins(48, 40, 48, 40)
        body.setSpacing(24)

        self.header = PageHeader("Что открыть в новой вкладке",
                                 tr("Вкладка запоминает раздел, запуск, фильтры и выделение."))
        body.addWidget(self.header)

        grid = QGridLayout()
        grid.setSpacing(12)
        self.options = []
        for index, (section, icon, title, hint) in enumerate(OPTIONS):
            button = QToolButton()
            button.setProperty("card", "choice")
            button.setIcon(material_icon(icon))
            button.setIconSize(QSize(24, 24))
            button.setText(f"{tr(title)}\n{tr(hint)}")
            button.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
            button.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
            button.setMinimumHeight(64)
            button.setCursor(Qt.PointingHandCursor)
            button.clicked.connect(lambda _checked=False, target=section: self.open_section(target))
            grid.addWidget(button, index // 3, index % 3)
            self.options.append(button)
        body.addLayout(grid)

        heading = QLabel(tr("Сохранённые виды"))
        heading.setProperty("text_style", "section")
        body.addWidget(heading)
        body.addWidget(StatePanel("waiting", "Виды проекта",
                                  "Список сохранённых видов открытого проекта в этой сборке не подключён."))
        body.addStretch(1)

    def open_section(self, section):
        """Switch the window to the chosen section; the window decides what it shows."""
        self.host.navigation.select_section(section)

    def refresh(self):
        return None
