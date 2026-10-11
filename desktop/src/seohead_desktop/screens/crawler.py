"""Screen «Краулер» (canvas Crawler): the project-less crawl board, drawn with the design-v2 layout.

The core cannot run a project-less crawl with a run log and per-host pacing yet (core gap #942), so the screen shows
no addresses, counts or progress: the start control and the table are waiting states with the neutral «Недоступно»
badge. Nothing is started from here and no sample data is shown. Temporary results (banner, selected address, status)
appear only through set_temporary_results() once the core can return such a crawl; until then they stay hidden.
"""

from __future__ import annotations

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import (
    QButtonGroup,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMenu,
    QPushButton,
    QScrollArea,
    QSizePolicy,
    QSplitter,
    QTabBar,
    QTabWidget,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import theming
from ..i18n import tr, trf
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import UNAVAILABLE, StatePanel

WAITING_ISSUE = (
    942  # core gap: project-less crawl run log and per-host rate limit (code only, never shown)
)
COMPACT_WIDTH = (
    1000  # below this the toolbar moves Очистить and Сохранить как проект into the overflow menu
)
CHECK_TABS = (
    "Внутренние",
    "Внешние",
    "Безопасность",
    "Коды ответа",
    "URL",
    "Title",
    "Description",
    "H1",
    "H2",
    "Контент",
    "Изображения",
    "Canonical",
    "Директивы",
    "Ссылки",
    "Sitemap",
)
OVERVIEW = (
    ("folder", "Всё", 0),
    ("folder_open", "Внутренние", 1),
    ("description", "HTML", 2),
    ("javascript", "JavaScript", 2),
    ("code", "CSS", 2),
    ("photo_size_select_large", "Изображения", 2),
    ("folder_open", "Коды ответа", 0),
    ("check_circle", "2xx успешно", 1),
    ("subdirectory_arrow_right", "3xx редирект", 1),
    ("warning", "4xx ошибка клиента", 1),
    ("error", "5xx ошибка сервера", 1),
    ("folder_open", "Title", 0),
    ("remove", "Отсутствует", 1),
    ("content_copy", "Дубли", 1),
    ("fit_width", "Длиннее 60 знаков", 1),
    ("folder", "Canonical", 0),
    ("folder", "Директивы", 0),
    ("folder", "Hreflang", 0),
)
# column: (header, stretch or fixed width); the same proportions as the board
COLUMNS = (
    ("Адрес", 1),
    ("Тип", 150),
    ("Код", 62),
    ("Индексация", 132),
    ("Title", 0.7),
    ("Длина", 64),
    ("Глубина", 72),
)
COLUMN_MIN = 80  # stretch columns never shrink below this; the header scrolls sideways instead
DETAIL_TABS = (
    "Сведения",
    "Входящие",
    "Исходящие",
    "Изображения",
    "Ресурсы",
    "Сниппет",
    "Исходник",
    "HTTP-заголовки",
)
DETAIL_FIELDS = (("Адрес", "Код ответа", "Индексация"), ("Title", "Глубина", "Время ответа"))
DEFAULT_DETAILS = "Сведения появятся, когда будет выбран адрес краула"
DEFAULT_STATUS = "Без проекта · данные краула недоступны"
TEMPORARY_STATUS = "Без проекта · данные во временной папке до закрытия окна"


class CrawlerScreen(QWidget):
    """Board «Краулер»: toolbar, check tabs, results table, overview tree and status line, all waiting."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setProperty("spaciousPage", False)
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        root.addWidget(self._toolbar())
        root.addWidget(self._temporary_note())
        root.addWidget(self._check_tabs())
        body = QHBoxLayout()
        body.setContentsMargins(0, 0, 0, 0)
        body.setSpacing(0)
        splitter = QSplitter(Qt.Vertical)
        splitter.setChildrenCollapsible(False)
        main = QWidget()
        main.setLayout(self._main())
        splitter.addWidget(main)
        splitter.addWidget(self._detail_pane())
        splitter.setStretchFactor(0, 3)
        splitter.setStretchFactor(1, 2)
        body.addWidget(splitter, 1)
        body.addWidget(self._overview())
        root.addLayout(body, 1)
        root.addWidget(self._status())
        self._fit_toolbar()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._fit_toolbar()

    def set_temporary_results(self, active, selected_url=None):
        """Show or hide the temporary-results banner, the selected address and the status text together."""
        self._note.setVisible(active)
        self._status_note.setText(tr(TEMPORARY_STATUS if active else DEFAULT_STATUS))
        if active and selected_url:
            self._details.setText(trf("Выбрано: {url}", url=selected_url))
        else:
            self._details.setText(tr(DEFAULT_DETAILS))

    def _fit_toolbar(self):
        compact = self.width() < COMPACT_WIDTH
        self._clear.setVisible(not compact)
        self._save.setVisible(not compact)
        self._overflow.setVisible(compact)

    def _toolbar(self):
        bar = QFrame()
        bar.setProperty("role", "toolbar")
        row = QHBoxLayout(bar)
        row.setContentsMargins(12, 8, 12, 8)
        row.setSpacing(8)
        chip = QLabel(tr("Краулер · без проекта"))
        chip.setProperty("badge", "info")
        row.addWidget(chip)
        address = QLineEdit()
        address.setPlaceholderText(tr("Адрес сайта"))
        address.setEnabled(False)
        address.setMinimumWidth(120)
        row.addWidget(address, 1)
        segmented = QFrame()
        segmented.setProperty("segmented", "true")
        seg_row = QHBoxLayout(segmented)
        seg_row.setContentsMargins(2, 2, 2, 2)
        seg_row.setSpacing(2)
        group = QButtonGroup(segmented)
        group.setExclusive(True)
        for text in ("Сайт", "Список", "Sitemap"):
            button = QToolButton()
            button.setProperty("segment", "true")
            button.setText(tr(text))
            button.setCheckable(True)
            button.setEnabled(False)
            group.addButton(button)
            seg_row.addWidget(button)
        row.addWidget(segmented)
        start = QPushButton(tr("Старт"))
        start.setProperty("role", "primary")
        start.setEnabled(False)
        start.setIcon(material_icon("play_arrow"))
        start.setToolTip(tr(UNAVAILABLE))
        row.addWidget(start)
        tune = QToolButton()
        tune.setIcon(material_icon("tune"))
        tune.setToolTip(tr("Настройки скана"))
        tune.setEnabled(False)
        row.addWidget(tune)
        self._clear = QPushButton(tr("Очистить"))
        self._clear.setEnabled(False)
        row.addWidget(self._clear)
        self._save = QPushButton(tr("Сохранить как проект"))
        self._save.setToolTip(tr("Сохранять пока нечего: краул без проекта не запускается"))
        self._save.setEnabled(False)
        row.addWidget(self._save)
        self._overflow = QToolButton()
        self._overflow.setIcon(material_icon("more_horiz"))
        self._overflow.setFixedWidth(36)
        self._overflow.setToolTip(tr("Ещё действия"))
        self._overflow.setPopupMode(QToolButton.InstantPopup)
        self._overflow.setEnabled(False)
        menu = QMenu(self._overflow)
        for text in ("Очистить", "Сохранить как проект"):
            menu.addAction(tr(text)).setEnabled(False)
        self._overflow.setMenu(menu)
        row.addWidget(self._overflow)
        return bar

    def _temporary_note(self):
        self._note = QFrame()
        self._note.setProperty("note", "info")
        row = QHBoxLayout(self._note)
        row.setContentsMargins(12, 10, 12, 10)
        row.setSpacing(10)
        row.addWidget(
            MaterialIconLabel("info", 20, color=theming.roles()["text_2"]), 0, Qt.AlignTop
        )
        text = QLabel(
            trf(
                "<b>{title}</b> {text}",
                title="Результат временный:",
                text="без проекта нет истории, задач и сравнения сканов. Сохраните как проект, чтобы перепроверять исправления.",
            )
        )
        text.setTextFormat(Qt.RichText)
        text.setWordWrap(True)
        row.addWidget(text, 1)
        self._note.setVisible(False)
        return self._note

    def _check_tabs(self):
        tabs = QTabBar()
        tabs.setProperty("tabs", "underline")
        tabs.setEnabled(False)
        tabs.setExpanding(False)
        tabs.setUsesScrollButtons(True)
        tabs.setElideMode(Qt.ElideNone)
        for text in CHECK_TABS:
            tabs.addTab(tr(text))
        return tabs

    def _main(self):
        column = QVBoxLayout()
        column.setContentsMargins(0, 0, 0, 0)
        column.setSpacing(0)
        filters = QHBoxLayout()
        filters.setContentsMargins(10, 8, 10, 8)
        filters.setSpacing(8)
        selector = QPushButton(tr("Фильтр: Всё"))
        selector.setEnabled(False)
        filters.addWidget(selector)
        search = QLineEdit()
        search.setPlaceholderText(tr("Поиск в адресах…"))
        search.setEnabled(False)
        search.setMinimumWidth(120)
        search.setMaximumWidth(260)
        filters.addWidget(search, 1)
        export = QPushButton(tr("Экспорт"))
        export.setEnabled(False)
        export.setSizePolicy(QSizePolicy.Fixed, QSizePolicy.Fixed)
        filters.addWidget(export)
        column.addLayout(filters)
        column.addWidget(self._table_header())
        column.addWidget(
            StatePanel(
                "waiting",
                "Краул без проекта пока не запускается",
                "Ядро ещё не отдаёт обход без проекта с журналом и ограничением скорости. "
                "Адреса, коды ответа и проверки появятся после этого.",
                issue=WAITING_ISSUE,
            ),
            1,
        )
        return column

    def _detail_pane(self):
        pane = QWidget()
        column = QVBoxLayout(pane)
        column.setContentsMargins(0, 0, 0, 0)
        column.setSpacing(0)
        self._details = QLabel(tr(DEFAULT_DETAILS))
        self._details.setProperty("text_style", "meta")
        self._details.setContentsMargins(16, 10, 16, 10)
        column.addWidget(self._details)
        tabs = QTabWidget()
        tabs.tabBar().setProperty("tabs", "underline")
        tabs.tabBar().setUsesScrollButtons(True)
        tabs.setEnabled(False)
        for name in DETAIL_TABS:
            page = QWidget()
            if name == DETAIL_TABS[0]:
                grid = QGridLayout(page)
                grid.setContentsMargins(16, 12, 16, 12)
                grid.setHorizontalSpacing(24)
                grid.setVerticalSpacing(8)
                for column_index, keys in enumerate(DETAIL_FIELDS):
                    for row_index, key in enumerate(keys):
                        caption = QLabel(tr(key))
                        caption.setProperty("text_style", "meta")
                        grid.addWidget(caption, row_index, column_index * 2)
                        grid.addWidget(QLabel(), row_index, column_index * 2 + 1)
                grid.setRowStretch(len(DETAIL_FIELDS[0]), 1)
            tabs.addTab(page, tr(name))
        column.addWidget(tabs, 1)
        return pane

    def _table_header(self):
        header = QFrame()
        header.setProperty("role", "table_header")
        row = QHBoxLayout(header)
        row.setContentsMargins(16, 6, 16, 6)
        row.setSpacing(8)
        needed = 16 * 2 + 8 * (len(COLUMNS) - 1)
        for text, width in COLUMNS:
            label = QLabel(tr(text))
            label.setProperty("text_style", "overline")
            if isinstance(width, int) and width > 1:
                label.setFixedWidth(width)
                row.addWidget(label)
                needed += width
            else:
                label.setMinimumWidth(COLUMN_MIN)
                row.addWidget(label, int(width * 10))
                needed += COLUMN_MIN
        header.setMinimumWidth(needed)
        scroll = QScrollArea()
        scroll.setFrameShape(QFrame.NoFrame)
        scroll.setWidgetResizable(True)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAsNeeded)
        scroll.setVerticalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        scroll.setWidget(header)
        scroll.setFixedHeight(
            header.sizeHint().height() + scroll.horizontalScrollBar().sizeHint().height()
        )
        return scroll

    def _overview(self):
        panel = QFrame()
        panel.setFixedWidth(300)
        panel.setProperty("role", "overview")
        column = QVBoxLayout(panel)
        column.setContentsMargins(0, 0, 0, 0)
        column.setSpacing(0)
        tabs = QTabBar()
        tabs.setProperty("tabs", "underline")
        tabs.setEnabled(False)
        for text in ("Обзор", "Проблемы", "Структура"):
            tabs.addTab(tr(text))
        column.addWidget(tabs)
        for icon_name, text, depth in OVERVIEW:
            row = QHBoxLayout()
            row.setContentsMargins(6 + depth * 16, 2, 10, 2)
            row.setSpacing(6)
            row.addWidget(MaterialIconLabel(icon_name, 16, color="role:text_muted"))
            label = QLabel(tr(text))
            label.setEnabled(False)
            row.addWidget(label)
            count = QLabel("—")
            count.setProperty("text_style", "meta")
            row.addWidget(count)
            row.addStretch(1)
            column.addLayout(row)
        column.addStretch(1)
        return panel

    def _status(self):
        bar = QFrame()
        bar.setProperty("role", "status")
        row = QHBoxLayout(bar)
        row.setContentsMargins(12, 4, 16, 4)
        row.setSpacing(20)
        state = QLabel(tr("Краул не запущен"))
        state.setProperty("badge", "mut")
        row.addWidget(state)
        for text in ("Средняя скорость: —", "Пройдено: —", "Осталось: —"):
            label = QLabel(tr(text))
            label.setProperty("text_style", "meta")
            row.addWidget(label)
        row.addStretch(1)
        self._status_note = QLabel(tr(DEFAULT_STATUS))
        self._status_note.setProperty("text_style", "meta")
        self._status_note.setWordWrap(True)
        row.addWidget(self._status_note)
        return bar
