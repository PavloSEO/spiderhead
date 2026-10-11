"""Screen «Сбор / дособор ядра» (sheet SemPipeline, extra screen).

The stage rail lists the twelve collection stages of the sheet. Stage order and the paid marker come from the core
(``seohead.semantics``: ``STAGES``, ``PAID_STAGES``), so the screen cannot drift from what the core runs. The core does
not yet hand the Desktop per-project stage status, a cost estimate or a stage journal (#1208), so the screen shows
no stage state, no sum and no event: parameters and the run button are disabled behind a neutral «Недоступно» badge.
It never reads SQLite and never calls the core.
"""

from __future__ import annotations

from pathlib import Path

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import (
    QButtonGroup,
    QFrame,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QScrollArea,
    QStackedWidget,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from ..i18n import tr, trf
from ..ui.icons import material_icon
from ..ui.kit import StatePanel, no_project_panel, waiting_badge
from .base import Screen

ISSUE = 1208  # core gap: stage status, cost estimate and run journal (hint text lives in ui/kit.ISSUE_HINTS)

# (stage id in seohead.semantics, Russian title, icon, one-line description; no sample values)
STEPS = (
    ("import", "Импорт", "upload", "Загрузка маркеров и готовых списков запросов."),
    (
        "collect",
        "Wordstat",
        "travel_explore",
        "Расширение маркеров через Wordstat по региону, подсказки и запросы из Search Console и Вебмастера.",
    ),
    (
        "clean",
        "Чистка",
        "cleaning_services",
        "Минус-слова, стоп-фразы, дубликаты и нецелевые гео. Ручная чистка — в таблице «Семантика».",
    ),
    (
        "graph",
        "Граф",
        "hub",
        "Граф совместной встречаемости лемм: омонимы и слова-маркеры для групп.",
    ),
    (
        "cluster",
        "Кластеры",
        "workspaces",
        "Группировка запросов. Локальные алгоритмы бесплатны; группировка по выдаче ТОП-10 снимает SERP и стоит денег.",
    ),
    ("competitors", "Конкуренты", "groups", "Домены из ТОП-10 по кластерам и их видимость."),
    (
        "synonyms",
        "Синонимы",
        "sync_alt",
        "Подбор синонимов и словоформ для добора маркеров через языковую модель.",
    ),
    (
        "mine",
        "Добор",
        "add_circle",
        "Добор запросов из графа и конкурентов без повторного платного сбора.",
    ),
    ("sitematch", "URL", "link", "Кластер → посадочный URL скана по title, H1, тексту и показам."),
    (
        "exact",
        "Точная",
        "target",
        "Точная частотность «[!слово]» по каждому запросу. Один раз в конце, после чистки.",
    ),
    (
        "report",
        "Отчёт",
        "description",
        "Сводка по ядру: кластеры, посадочные, пробелы и рекомендации.",
    ),
    ("excel", "Excel", "table", "Выгрузка ядра с группами, частотами, URL и метриками в XLSX."),
)


def core_stages():
    """(STAGES, PAID_STAGES) of the core; imported lazily so the screen module stays light."""
    from seohead.semantics import PAID_STAGES, STAGES

    return STAGES, PAID_STAGES


class StageButton(QToolButton):
    """One step of the rail: number, icon, title and the ₽ mark for a stage the core bills."""

    def __init__(self, index, title, icon, paid, parent=None):
        super().__init__(parent)
        self.setCheckable(True)
        self.setAutoRaise(True)
        self.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
        self.setIcon(material_icon(icon))
        suffix = "  ₽" if paid else ""
        self.setText(f"{index} {tr(title)}{suffix}")
        self.setProperty("role", "stage")
        self.setProperty("paid", paid)
        self.setAccessibleName(tr(title))
        self.setToolTip(
            tr("Платная стадия: запуск требует сметы и подтверждения")
            if paid
            else tr("Бесплатная стадия: работает по данным ядра")
        )


class SemanticsScreen(Screen):
    """Extra screen (not a navigation slot): opened from the semantics area, closed back to the section."""

    watches = ("project",)
    chrome_free = True  # SHELL-CANON §7: no project / scan switchers in the top bar

    def __init__(self, host):
        super().__init__(host)
        stages, paid = core_stages()
        self.steps = tuple(step for step in STEPS if step[0] in stages)
        self.paid = {step[0]: step[0] in paid for step in self.steps}
        self.current = 0
        layout = QVBoxLayout(self)
        layout.setContentsMargins(20, 16, 20, 16)
        layout.setSpacing(12)
        layout.addLayout(self._header())
        layout.addWidget(self._rail())
        body = QHBoxLayout()
        body.setSpacing(16)
        body.addWidget(self._parameters(), 0)
        self.state_stack = QStackedWidget()
        self.no_project = no_project_panel(
            host, "Откройте проект, чтобы увидеть стадии его семантического ядра."
        )
        self.waiting = StatePanel(
            "waiting",
            "Состояние стадий ещё не передано в приложение",
            "Стадии, смета платных шагов и журнал появятся, когда ядро отдаст их приложению.",
            issue=ISSUE,
        )
        self.state_stack.addWidget(self.no_project)
        self.state_stack.addWidget(self.waiting)
        body.addWidget(self.state_stack, 1)
        layout.addLayout(body, 1)
        self.select(0)

    def _header(self):
        row = QHBoxLayout()
        self.title = QLabel(tr("Сбор / дособор ядра"))
        self.title.setProperty("text_style", "section")
        self.meta = QLabel(tr("Проект не открыт"))
        self.meta.setProperty("text_style", "meta")
        names = QVBoxLayout()
        names.setSpacing(2)
        names.addWidget(self.title)
        names.addWidget(self.meta)
        row.addLayout(names, 1)
        self.close_button = QPushButton(tr("Закрыть"))
        self.close_button.clicked.connect(lambda: self.host.leave_start())
        row.addWidget(self.close_button, 0, Qt.AlignTop)
        return row

    def _rail(self):
        """The twelve steps in one row; the row scrolls sideways instead of eliding the titles."""
        strip = QWidget()
        strip.setObjectName("stepRail")
        row = QHBoxLayout(strip)
        row.setContentsMargins(8, 4, 8, 4)
        row.setSpacing(2)
        frame = QScrollArea()
        frame.setFrameShape(QFrame.NoFrame)
        frame.setWidgetResizable(True)
        frame.setVerticalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        frame.setWidget(strip)
        self.group = QButtonGroup(self)
        self.group.setExclusive(True)
        self.buttons = []
        for index, (stage, title, icon, _text) in enumerate(self.steps):
            button = StageButton(index + 1, title, icon, self.paid[stage], strip)
            button.clicked.connect(lambda _checked=False, position=index: self.select(position))
            self.group.addButton(button)
            self.buttons.append(button)
            row.addWidget(button)
        row.addStretch(1)
        return frame

    def _parameters(self):
        pane = QFrame()
        pane.setProperty("role", "pane")
        pane.setFixedWidth(296)
        column = QVBoxLayout(pane)
        column.setContentsMargins(16, 16, 16, 16)
        column.setSpacing(10)
        self.step_title = QLabel()
        self.step_title.setProperty("text_style", "section")
        self.step_text = QLabel()
        self.step_text.setProperty("text_style", "meta")
        self.step_text.setWordWrap(True)
        self.estimate_head = QLabel(tr("Смета перед запуском"))
        self.estimate_head.setProperty("text_style", "meta")
        self.estimate_note = QLabel()
        self.estimate_note.setProperty("text_style", "meta")
        self.estimate_note.setWordWrap(True)
        self.badge = waiting_badge(ISSUE)
        self.run_button = QPushButton(tr("Запустить стадию"))
        self.run_button.setProperty("role", "primary")
        self.run_button.setEnabled(False)  # core contract for the run and its confirmation is #1208
        column.addWidget(self.step_title)
        column.addWidget(self.step_text)
        column.addSpacing(8)
        column.addWidget(self.estimate_head)
        column.addWidget(self.estimate_note)
        column.addWidget(self.badge, 0, Qt.AlignLeft)
        column.addStretch(1)
        column.addWidget(self.run_button)
        return pane

    def select(self, position):
        self.current = position
        stage, title, _icon, text = self.steps[position]
        self.buttons[position].setChecked(True)
        self.step_title.setText(
            trf("Стадия {number} · {title}", number=position + 1, title=tr(title))
        )
        self.step_text.setText(tr(text))
        if self.paid[stage]:
            self.estimate_note.setText(tr("Смета платного запуска появится, когда ядро её отдаст."))
            self.estimate_note.show()
            self.badge.show()
        else:
            self.estimate_note.setText(tr("Стадия бесплатная — расходов провайдеров нет."))
            self.estimate_note.show()
            self.badge.hide()
        self.estimate_head.setVisible(self.paid[stage])

    def refresh(self):
        name = Path(str(self.host.project_directory)).name if self.host.project_directory else ""
        self.meta.setText(name or tr("Проект не открыт"))
        self.state_stack.setCurrentWidget(self.waiting if self.project_open else self.no_project)
