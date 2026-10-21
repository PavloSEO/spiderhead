"""URL card (canvas UrlHeaders, UrlLinks, UrlRedirects, UrlResources, UrlSchema, UrlHtml, UrlHistory + the tab strip of Main).

One widget in two sizes: compact under / beside the URL table and expanded over the whole screen (the table is hidden, the
same instance is reused). Every number comes from the core: ``scan-url-detail`` (headers, page fields, redirect hops),
``scan-link-inspect --view links`` (incoming / outgoing links with total). What the core does not give yet keeps its tab and
shows the neutral «Недоступно» badge with a hint; nothing is estimated here.
"""

from __future__ import annotations

import json

from PyQt5.QtCore import QAbstractTableModel, Qt, QTimer, pyqtSignal
from PyQt5.QtGui import QColor, QFont, QFontDatabase, QTextCursor
from PyQt5.QtWidgets import (
    QApplication,
    QComboBox,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QMenu,
    QPlainTextEdit,
    QPushButton,
    QScrollArea,
    QStackedWidget,
    QTabBar,
    QTableView,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import i18n, theming
from ..i18n import joined, tr, trf
from ..ui.controls import Segmented
from ..ui.icons import material_icon
from ..ui.kit import (
    BADGE_ROLE,
    BadgeDelegate,
    StatePanel,
    style_table,
    unavailable_tip,
    waiting_badge,
)
from ..ui.presentation import short_run_id
from .scan_common import Pairs, number
from .url_detail import (
    OverviewPage,
    SnippetPage,
    UrlHead,
    clean,
    first_response,
    headers_text,
    icon_button,
    key_label,
    section_label,
    source_text,
    stamp_text,
)
from .url_graph import GraphPage
from .url_history import HistoryPage
from .url_query import (
    CoreJob,
    detail_arguments,
    http_badge,
    links_arguments,
    query_arguments,
    seconds_to_ms,
    split_url,
)

TAB_ORDER = (
    "info",
    "hdr",
    "links",
    "html",
    "res",
    "redir",
    "hist",
    "schema",
    "graph",
    "snip",
    "checks",
)
TAB_LABELS = {
    "info": "Обзор",
    "hdr": "Заголовки",
    "links": "Ссылки",
    "html": "HTML",
    "res": "Ресурсы",
    "redir": "Редиректы",
    "hist": "История",
    "schema": "Schema",
    "graph": "Граф",
    "snip": "Сниппет",
    "checks": "Проверки",
}
# tab -> (title, text, core issue) of the tabs whose data the core does not give yet
UNAVAILABLE = {
    "res": (
        "Ресурсы страницы",
        "Изображения, CSS, JS и шрифты страницы со статусом и весом ядро по одному URL не отдаёт; ниже — счётчики со страницы.",
        974,
    ),
    "schema": (
        "Структурированные данные",
        "JSON-LD читается только из сохранённого тела выбранного URL. Microdata, RDFa и проверки eligibility здесь не оцениваются.",
        None,
    ),
    "checks": (
        "Проверки выбранного URL",
        "Находки по одному URL ядро пока не отдаёт: в скане они есть только списком проверок.",
        980,
    ),
}
RESOURCE_ROWS = (
    ("Ресурсов всего", "resource_count"),
    ("Изображения", "resource_images"),
    ("Скрипты", "resource_scripts"),
    ("Стили", "resource_stylesheets"),
    ("Вес ресурсов, байт", "resource_bytes"),
)
RESOURCE_COLUMNS = ("url_id", *(key for _title, key in RESOURCE_ROWS))
MAX_HOPS = 10
HEADER_NOTES = (
    ("x-robots-tag", "visibility_off", "X-Robots"),
    ("cache-control", "cached", "Кэш"),
    ("content-encoding", "compress", "Сжатие"),
    ("strict-transport-security", "lock", "HSTS"),
)
LINK_COLUMNS = ("Адрес", "Анкор", "rel", "Позиция", "HTTP цели")


def label_row(text, mono=False, na=False, wrap=True):
    label = QLabel(text)
    label.setWordWrap(wrap)
    label.setTextInteractionFlags(Qt.TextSelectableByMouse)
    if mono:
        label.setProperty("text_style", "mono")
    if na:
        label.setProperty("na", True)
    return label


def kv_block(rows, mono=True):
    """Key / value rows of header lines; the widget is rebuilt per URL (a few dozen labels, never per table row)."""
    box = QWidget()
    grid = QGridLayout(box)
    grid.setContentsMargins(0, 0, 0, 0)
    grid.setHorizontalSpacing(12)
    grid.setVerticalSpacing(0)
    grid.setColumnStretch(1, 1)
    for index, (key, value) in enumerate(rows):
        k = label_row(key, mono=mono, wrap=False)
        k.setProperty("kv", "key")
        v = label_row(value if value is not None else tr("Нет данных"), mono=mono, na=value is None)
        v.setProperty("kv", "value")
        grid.addWidget(k, index, 0, Qt.AlignTop)
        grid.addWidget(v, index, 1)
    return box


def scrolled(widget):
    scroll = QScrollArea()
    scroll.setWidgetResizable(True)
    scroll.setFrameShape(QFrame.NoFrame)
    scroll.setWidget(widget)
    return scroll


def clear(layout):
    while layout.count():
        item = layout.takeAt(0)
        widget = item.widget()
        if widget is not None:
            widget.hide()
            widget.setParent(None)
            widget.deleteLater()
        elif item.layout():
            clear(item.layout())


def note(kind, text):
    frame = QFrame()
    frame.setProperty("note", kind)
    layout = QHBoxLayout(frame)
    layout.setContentsMargins(8, 8, 8, 8)
    label = QLabel(text)
    label.setWordWrap(True)
    layout.addWidget(label)
    return frame


class Context:
    """What the card shows now: where the scan is, which URL, and the answers already read."""

    def __init__(self):
        self.host = None
        self.scan = None
        self.url = None
        self.row = {}
        self.detail = None

    @property
    def page(self):
        return (self.detail or {}).get("page") or {}


class HeadersPage(QWidget):
    """UrlHeaders: request headers, response headers, timings and conclusions drawn from the saved response."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.grid = QGridLayout(self)
        self.grid.setContentsMargins(20, 12, 20, 12)
        self.grid.setHorizontalSpacing(24)
        self.grid.setVerticalSpacing(14)
        self.columns = []
        self.detail = None
        self._narrow = None
        self._build()

    def _build(self):
        self.request_box, self.response_box, self.side_box = QWidget(), QWidget(), QWidget()
        for box in (self.request_box, self.response_box, self.side_box):
            layout = QVBoxLayout(box)
            layout.setContentsMargins(0, 0, 0, 0)
            layout.setSpacing(8)
        self._place("three")

    def _place(self, mode):
        boxes = (self.request_box, self.response_box, self.side_box)
        for box in boxes:
            self.grid.removeWidget(box)
        for column in range(3):
            self.grid.setColumnStretch(column, 0)
        if mode == "one":
            for row, box in enumerate(boxes):
                self.grid.addWidget(box, row, 0, Qt.AlignTop)
            self.grid.setColumnStretch(0, 1)
        elif mode == "two":
            self.grid.addWidget(self.request_box, 0, 0, Qt.AlignTop)
            self.grid.addWidget(self.response_box, 0, 1, Qt.AlignTop)
            self.grid.addWidget(self.side_box, 1, 0, 1, 2, Qt.AlignTop)
            self.grid.setColumnStretch(0, 4)
            self.grid.setColumnStretch(1, 5)
        else:
            for column, box in enumerate(boxes):
                self.grid.addWidget(box, 0, column, Qt.AlignTop)
                self.grid.setColumnStretch(column, (4, 5, 4)[column])
        self.grid.setRowStretch(3, 1)
        self._narrow = mode

    def resizeEvent(self, event):
        super().resizeEvent(event)
        width = event.size().width()
        mode = "three" if width >= 1000 else "two" if width >= 640 else "one"
        if mode != self._narrow:
            self._place(mode)

    def fill(self, detail):
        self.detail = detail
        response = first_response(detail)
        for box in (self.request_box, self.response_box, self.side_box):
            clear(box.layout())
        request = response.get("request_headers")
        status = response.get("status_code")
        req_pairs = (
            [(str(a), str(b)) for a, b in request if isinstance(request, list)]
            if isinstance(request, list)
            else []
        )
        self.request_box.layout().addWidget(section_label(trf("Запрос · {n}", n=len(req_pairs))))
        if response:
            start = f"{response.get('method') or 'GET'}  {split_url(response.get('request_url'))[1] or ''}"
            self.request_box.layout().addWidget(label_row(start, mono=True))
        self.request_box.layout().addWidget(
            kv_block(req_pairs) if req_pairs else label_row(tr("Нет данных"), na=True)
        )
        self.request_box.layout().addWidget(
            note(
                "info",
                tr(
                    "Заголовки запроса — как отправил краулер. Повторный запрос с вкладки не выполняется."
                ),
            )
        )
        self.request_box.layout().addStretch(1)
        headers = response.get("response_headers")
        pairs = [(str(a), str(b)) for a, b in headers] if isinstance(headers, list) else []
        head = QHBoxLayout()
        head.addWidget(section_label(trf("Ответ · {n}", n=len(pairs))))
        head.addStretch(1)
        copy = QPushButton(tr("Копировать всё"))
        copy.setProperty("role", "text")
        copy.setProperty("size", "sm")
        copy.setIcon(material_icon("content_copy"))
        copy.setEnabled(bool(pairs))
        copy.clicked.connect(
            lambda _c=False: QApplication.clipboard().setText(headers_text(self.detail) or "")
        )
        head.addWidget(copy)
        self.response_box.layout().addLayout(head)
        if type(status) is int:
            self.response_box.layout().addWidget(label_row(f"HTTP {status}", mono=True))
        self.response_box.layout().addWidget(
            kv_block(pairs)
            if pairs
            else label_row(tr("Ядро не сохранило заголовки этого ответа."), na=True)
        )
        self.response_box.layout().addStretch(1)
        ms = seconds_to_ms(response.get("response_time"))
        self.side_box.layout().addWidget(
            section_label(
                trf("Тайминги · всего {ms} мс", ms=number(ms)) if ms is not None else "Тайминги"
            )
        )
        line = QHBoxLayout()
        line.addWidget(label_row(tr("DNS, соединение, TLS, TTFB, загрузка"), wrap=True), 1)
        line.addWidget(waiting_badge(956))
        self.side_box.layout().addLayout(line)
        self.side_box.layout().addWidget(section_label("Выводы по заголовкам"))
        lowered = {k.lower(): v for k, v in pairs}
        for key, icon, label in HEADER_NOTES:
            value = lowered.get(key)
            row = QHBoxLayout()
            badge = QLabel(tr(label))
            badge.setProperty("badge", "ok" if value else "mut")
            row.addWidget(badge, 0, Qt.AlignTop)
            row.addWidget(
                label_row(value if value else tr("не задан"), mono=bool(value), na=not value), 1
            )
            self.side_box.layout().addLayout(row)
        self.side_box.layout().addStretch(1)

    def count(self):
        response = first_response(self.detail)
        headers = response.get("response_headers")
        request = response.get("request_headers")
        total = (len(headers) if isinstance(headers, list) else 0) + (
            len(request) if isinstance(request, list) else 0
        )
        return total or None


class LinkModel(QAbstractTableModel):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.rows = []
        self.direction = "out"
        self.badges = True

    def set_rows(self, rows, direction):
        self.beginResetModel()
        self.rows, self.direction = list(rows), direction
        self.endResetModel()

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return len(LINK_COLUMNS)

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if orientation != Qt.Horizontal:
            return None
        if role == Qt.DisplayRole:
            if self.direction == "in" and section in (0, 4):
                return tr("Источник" if section == 0 else "HTTP источника")
            return tr(LINK_COLUMNS[section])
        if role == Qt.ToolTipRole and section == 3:
            return unavailable_tip("Позиция ссылки на странице")
        if role == Qt.TextAlignmentRole and section == 4:
            return int(Qt.AlignRight | Qt.AlignVCenter)
        return None

    def other(self, link):
        return link.get("source_url") if self.direction == "in" else link.get("target_url")

    def status(self, link):
        return link.get("source_status") if self.direction == "in" else link.get("target_status")

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid() or index.row() >= len(self.rows):
            return None
        link, column = self.rows[index.row()], index.column()
        text = {
            0: self.other(link),
            1: link.get("anchor") or tr("[без текста]"),
            2: joined(", ", [name for name in ("nofollow", "sponsored", "ugc") if link.get(name)])
            or "—",
            3: link.get("position") or None,
            4: None,
        }[column]
        if column == 4:
            status = self.status(link)
            kind, label = http_badge(status) if type(status) is int else ("mut", tr("Не в скане"))
            if role == BADGE_ROLE and self.badges:
                return kind, label
            text = label
        if role == Qt.DisplayRole:
            return text if text else tr("Нет данных")
        if role == Qt.ToolTipRole:
            return (
                self.other(link)
                if column == 0
                else unavailable_tip("Позиция ссылки на странице")
                if column == 3 and not text
                else None
            )
        if role == Qt.TextAlignmentRole and column == 4:
            return int(Qt.AlignRight | Qt.AlignVCenter)
        if role == Qt.FontRole and (text is None or (column == 1 and not link.get("anchor"))):
            font = QFont()
            font.setItalic(True)
            return font
        if role == Qt.ForegroundRole and (text is None or (column == 1 and not link.get("anchor"))):
            return QColor(theming.roles()["text_muted"])
        return None


class LinksPage(QWidget):
    """UrlLinks: incoming / outgoing links of the URL from the core, filtered and paged there."""

    counts_changed = pyqtSignal(object, object)

    def __init__(self, ctx, parent=None):
        super().__init__(parent)
        self.ctx = ctx
        self.direction = "out"
        self.offset = 0
        self.revision = 0
        self.totals = {"in": None, "out": None}
        self.loaded_for = None
        self.job = CoreJob(ctx.host_ref, self)
        self.job.done.connect(self._done)
        self.job.failed.connect(self._failed)
        self.counter = CoreJob(ctx.host_ref, self)
        self.counter.done.connect(self._counted)
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        scope_row = QHBoxLayout()
        scope_row.setContentsMargins(12, 4, 12, 0)
        scope_row.setSpacing(8)
        self.scope_buttons = {}
        for key, label in (("int", "Внутренние"), ("ext", "Внешние")):
            button = QToolButton()
            button.setProperty("pill", "group")
            button.setCheckable(True)
            button.setChecked(key == "int")
            button.setText(tr(label))
            button.clicked.connect(lambda _c=False, k=key: self.set_scope(k))
            self.scope_buttons[key] = button
            scope_row.addWidget(button)
        scope_row.addStretch(1)
        root.addLayout(scope_row)
        self.internal = QWidget()
        layout = QVBoxLayout(self.internal)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        root.addWidget(self.internal, 1)
        external_body = QWidget()
        external_layout = QVBoxLayout(external_body)
        external_layout.setContentsMargins(24, 12, 24, 12)
        external_layout.addWidget(
            StatePanel(
                "waiting",
                "Внешние ссылки доноров",
                "База внешних ссылок проекта (Вебмастер, Bing, GSC) ещё не подключена к ядру.",
                issue=999,
            )
        )
        external_layout.addStretch(1)
        self.external = scrolled(external_body)
        self.external.hide()
        root.addWidget(self.external, 1)
        bar = QHBoxLayout()
        bar.setContentsMargins(12, 4, 12, 4)
        bar.setSpacing(8)
        self.buttons = {}
        for key in ("in", "out"):
            button = QToolButton()
            button.setProperty("pill", "group")
            button.setCheckable(True)
            button.setChecked(key == self.direction)
            button.clicked.connect(lambda _c=False, k=key: self.set_direction(k))
            self.buttons[key] = button
            bar.addWidget(button)
        self.search = QLineEdit()
        self.search.setPlaceholderText(tr("Адрес или анкор содержит…"))
        self.search.setAccessibleName(tr("Фильтр ссылок"))
        self.search.setClearButtonEnabled(True)
        self.search.setFixedHeight(32)
        self.search.setMinimumWidth(120)
        self.search.setMaximumWidth(300)
        self.timer = QTimer(self)
        self.timer.setSingleShot(True)
        self.timer.setInterval(350)
        self.timer.timeout.connect(lambda: self.reload(0))
        self.search.textChanged.connect(lambda _t: self.timer.start())
        bar.addWidget(self.search)
        self.nofollow = QToolButton()
        self.nofollow.setProperty("pill", "group")
        self.nofollow.setCheckable(True)
        self.nofollow.setText("rel: nofollow")
        self.nofollow.toggled.connect(lambda _c: self.reload(0))
        bar.addWidget(self.nofollow)
        bar.addStretch(1)
        self.caption = QLabel()
        self.caption.setProperty("text_style", "meta")
        bar.addWidget(self.caption)
        self.help = icon_button("help", "Как читать таблицу ссылок")
        bar.addWidget(self.help)
        self.prev = icon_button("chevron_left", "Предыдущая страница", False)
        self.next = icon_button("chevron_right", "Следующая страница", False)
        self.prev.clicked.connect(lambda: self.reload(max(0, self.offset - 100)))
        self.next.clicked.connect(lambda: self.reload(self.offset + 100))
        export = icon_button("download", "Экспорт ссылок", False)
        export.setToolTip(unavailable_tip("Экспорт ссылок"))
        self.export = export
        for widget in (self.prev, self.next, export):
            bar.addWidget(widget)
        layout.addLayout(bar)
        self.model = LinkModel(self)
        self.table = style_table(QTableView(), "compact")
        self.table.setModel(self.model)
        self.table.setFrameShape(QFrame.NoFrame)
        self.table.setItemDelegateForColumn(4, BadgeDelegate(self.table))
        header = self.table.horizontalHeader()
        header.setStretchLastSection(False)
        header.setSectionResizeMode(0, QHeaderView.Stretch)
        header.setSectionResizeMode(1, QHeaderView.Stretch)
        for column, width in ((2, 100), (3, 110), (4, 132)):
            header.setSectionResizeMode(column, QHeaderView.Fixed)
            self.table.setColumnWidth(column, width)
        self.stack = QStackedWidget()
        self.stack.addWidget(self.table)
        self.state = QWidget()
        self.state_layout = QVBoxLayout(self.state)
        self.stack.addWidget(self.state)
        layout.addWidget(self.stack, 1)
        self._labels()

    def _labels(self):
        for key, button in self.buttons.items():
            total = self.totals[key]
            button.setText(
                f"{tr('Входящие' if key == 'in' else 'Исходящие')} · {number(total) if total is not None else '…'}"
            )
            button.setMinimumWidth(button.sizeHint().width())

    def retranslate(self):
        self.search.setPlaceholderText(tr("Адрес или анкор содержит…"))
        for key, label in (("int", "Внутренние"), ("ext", "Внешние")):
            self.scope_buttons[key].setText(tr(label))
        self._labels()

    def set_scope(self, key):
        """«Внутренние» are the core's links; «Внешние» (donors from the project's backlink base) wait for issue 999."""
        for name, button in self.scope_buttons.items():
            button.setChecked(name == key)
        self.internal.setVisible(key == "int")
        self.external.setVisible(key == "ext")
        if key == "int":
            self.activate()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self.caption.setVisible(event.size().width() >= 1000)

    def set_direction(self, key):
        self.direction = key
        for name, button in self.buttons.items():
            button.setChecked(name == key)
        self.reload(0)

    def context_changed(self):
        """A different URL (or none): forget what was loaded; the page reloads when it is shown."""
        self.job.cancel()
        self.counter.cancel()
        self.timer.stop()
        for widget, reset in (
            (self.search, lambda: self.search.setText("")),
            (self.nofollow, lambda: self.nofollow.setChecked(False)),
        ):
            widget.blockSignals(True)
            reset()
            widget.blockSignals(False)
        for name, button in self.scope_buttons.items():
            button.setChecked(name == "int")
        self.internal.show()
        self.external.hide()
        self.totals = {"in": None, "out": None}
        self.loaded_for = None
        self.offset = 0
        self.model.set_rows([], self.direction)
        self._labels()
        self.counts_changed.emit(None, None)

    def activate(self):
        key = (self.ctx.scan, self.ctx.url)
        if self.ctx.url and self.loaded_for != key:
            self.loaded_for = key
            self.reload(0)

    def reload(self, offset):
        if not self.ctx.scan or not self.ctx.url:
            return
        self.offset = offset
        self.revision += 1
        try:
            arguments = links_arguments(
                self.ctx.scan,
                self.ctx.url,
                direction=self.direction,
                offset=offset,
                contains=self.search.text(),
                nofollow=self.nofollow.isChecked(),
            )
        except ValueError as exc:
            return self._show_state("error", str(exc))
        self._show_state("loading", "")
        self.job.start(arguments, ("links", self.revision, self.ctx.url))

    def _show_state(self, kind, text):
        clear(self.state_layout)
        if kind is None:
            self.stack.setCurrentIndex(0)
            return
        title = {
            "loading": "Читаю ссылки…",
            "error": "Не удалось прочитать ссылки",
            "empty": "Ссылок нет",
        }[kind]
        panel = StatePanel(kind, title, text)
        panel.layout().setContentsMargins(24, 4, 24, 4)
        self.state_layout.addWidget(panel)
        self.stack.setCurrentIndex(1)

    def _done(self, token, payload):
        if token != ("links", self.revision, self.ctx.url):
            return
        if payload.get("ok") is not True:
            return self._show_state(
                "error",
                payload.get("error")
                or payload.get("reason_code")
                or tr("Ядро не выполнило запрос"),
            )
        direction = payload.get("direction") or self.direction
        rows = payload.get("items") or []
        total = payload.get("total") if type(payload.get("total")) is int else None
        filtered = (
            payload.get("filtered_total")
            if payload.get("filtered_total_state") == "exact"
            else None
        )
        if not self.search.text().strip() and not self.nofollow.isChecked():
            self.totals[direction] = total
        self.model.set_rows(rows, direction)
        self._labels()
        if not rows:
            self._show_state(
                "empty",
                tr("Под условия ничего не подошло")
                if self.search.text().strip() or self.nofollow.isChecked()
                else tr("Ядро не нашло ссылок этой страницы в сохранённом скане."),
            )
        else:
            self._show_state(None, "")
        shown = (
            trf(
                "Показаны {a}–{b} из {n}",
                a=number(self.offset + 1),
                b=number(self.offset + len(rows)),
                n=number(filtered),
            )
            if rows and filtered is not None
            else ""
        )
        self.caption.setText(shown)
        coverage = (payload.get("coverage") or {}).get("not_retained_attributes") or []
        self.help.setToolTip(
            joined(
                "\n",
                [
                    tr(
                        "Позиция ссылки и список внутренних и внешних — по данным скана; «Нет данных» — ядро не сохранило значение, это не ноль."
                    ),
                    trf("Не сохранено в скане: {a}", a=", ".join(coverage)) if coverage else "",
                ],
            )
        )
        self.prev.setEnabled(self.offset > 0)
        self.next.setEnabled(bool(payload.get("has_more")))
        self.counts_changed.emit(self.totals["in"], self.totals["out"])
        other = "in" if direction == "out" else "out"
        if self.totals[other] is None and not self.counter.busy:
            self.counter.start(
                links_arguments(self.ctx.scan, self.ctx.url, direction=other, limit=1),
                ("count", self.ctx.url, other),
            )

    def _counted(self, token, payload):
        if token[1] != self.ctx.url or payload.get("ok") is not True:
            return
        total = payload.get("total")
        self.totals[token[2]] = total if type(total) is int else None
        self._labels()
        self.counts_changed.emit(self.totals["in"], self.totals["out"])

    def _failed(self, token, text):
        if token == ("links", self.revision, self.ctx.url):
            self._show_state("error", text)


class Hop:
    def __init__(self, url, status=None, ms=None, target=None, known=True):
        self.url, self.status, self.ms, self.target, self.known = url, status, ms, target, known


class RedirectsPage(QWidget):
    """UrlRedirects: the chain of hops built by following ``redirect_url`` through ``scan-url-detail`` (at most 10 hops)."""

    counted = pyqtSignal(object)

    def __init__(self, ctx, parent=None):
        super().__init__(parent)
        self.ctx = ctx
        self.hops = []
        self.verdict = None  # None while walking | "end" | "loop" | "limit" | "missing"
        self.generation = 0
        self.loaded_for = None
        self.job = CoreJob(ctx.host_ref, self)
        self.job.done.connect(self._hop)
        self.job.failed.connect(self._failed)
        self.grid = QGridLayout(self)
        self.grid.setContentsMargins(20, 12, 20, 12)
        self.grid.setHorizontalSpacing(32)
        self.grid.setColumnStretch(0, 3)
        self.grid.setColumnStretch(1, 2)
        self.chain_box = QWidget()
        self.chain = QVBoxLayout(self.chain_box)
        self.chain.setContentsMargins(0, 0, 0, 0)
        self.chain.setSpacing(4)
        self.side_box = QWidget()
        self.side = QVBoxLayout(self.side_box)
        self.side.setContentsMargins(0, 0, 0, 0)
        self.side.setSpacing(8)
        self.grid.addWidget(self.chain_box, 0, 0, Qt.AlignTop)
        self.grid.addWidget(self.side_box, 0, 1, Qt.AlignTop)
        self.grid.setRowStretch(1, 1)
        self._narrow = False

    def resizeEvent(self, event):
        super().resizeEvent(event)
        narrow = event.size().width() < 900
        if narrow != self._narrow:
            self._narrow = narrow
            self.grid.removeWidget(self.side_box)
            self.grid.addWidget(self.side_box, 1 if narrow else 0, 0 if narrow else 1, Qt.AlignTop)
            self.grid.setRowStretch(1, 0 if narrow else 1)
            self.grid.setRowStretch(2, 1 if narrow else 0)

    def context_changed(self):
        self.job.cancel()
        self.generation += 1
        self.hops, self.verdict, self.loaded_for = [], None, None
        self._render()

    def activate(self):
        key = (self.ctx.scan, self.ctx.url)
        if not self.ctx.url or self.loaded_for == key or not self.ctx.detail:
            return
        self.loaded_for = key
        self.generation += 1
        self.hops, self.verdict = [], None
        self._consume(self.ctx.url, self.ctx.detail, first=True)

    def _consume(self, url, payload, first=False):
        page = (payload or {}).get("page")
        if not isinstance(page, dict):
            self.hops.append(Hop(url, known=False))
            self.verdict = "missing"
            return self._finish()
        status = page.get("status_code")
        response = first_response(payload)
        target = clean(page.get("redirect_url"))
        self.hops.append(
            Hop(
                url,
                status if type(status) is int else None,
                seconds_to_ms(page.get("response_time")),
                target,
            )
        )
        seen = {hop.url for hop in self.hops[:-1]}
        if type(status) is int and 300 <= status < 400 and target:
            if target in seen or target == url:
                self.hops.append(Hop(target, known=True))
                self.verdict = "loop"
            elif len(self.hops) >= MAX_HOPS:
                self.verdict = "limit"
            else:
                self._render()
                self.job.start(
                    detail_arguments(self.ctx.scan, target), ("hop", self.generation, target)
                )
                return
        else:
            self.verdict = "end"
        del response
        self._finish()

    def _hop(self, token, payload):
        if token[0] != "hop" or token[1] != self.generation:
            return
        if payload.get("ok") is not True or payload.get("state") == "not_found":
            self.hops.append(Hop(token[2], known=False))
            self.verdict = "missing"
            return self._finish()
        self._consume(token[2], payload)

    def _failed(self, token, _text):
        if token[1] == self.generation:
            self.hops.append(Hop(token[2], known=False))
            self.verdict = "missing"
            self._finish()

    def _finish(self):
        self._render()
        self.counted.emit(max(0, len([h for h in self.hops if h.target])))

    def _render(self):
        clear(self.chain)
        clear(self.side)
        head = QHBoxLayout()
        head.addWidget(section_label("Цепочка этого URL"))
        head.addStretch(1)
        self.chain.addLayout(head)
        if not self.hops:
            self.chain.addWidget(label_row(tr("Читаю цепочку…"), na=True))
        for index, hop in enumerate(self.hops):
            if index:
                arrow = QLabel("↓ Location")
                arrow.setProperty("text_style", "meta")
                arrow.setContentsMargins(24, 0, 0, 0)
                self.chain.addWidget(arrow)
            self.chain.addWidget(self._hop_row(index, hop))
        loop = self.verdict == "loop"
        redirects = [h for h in self.hops if h.target]
        if loop:
            self.chain.addWidget(
                note(
                    "error",
                    tr(
                        "Цикл редиректов. Цепочка вернулась на уже пройденный URL: страница недоступна ни пользователю, ни роботу."
                    ),
                )
            )
        elif self.verdict == "limit":
            self.chain.addWidget(
                note(
                    "warn",
                    trf(
                        "Цепочка длиннее {n} переходов — показано {n}; остальные не прочитаны.",
                        n=MAX_HOPS,
                    ),
                )
            )
        elif self.verdict == "missing":
            self.chain.addWidget(
                note("warn", tr("Целевой URL не сохранён в этом скане: итог цепочки не известен."))
            )
        elif len(redirects) > 1:
            self.chain.addWidget(
                note(
                    "warn",
                    trf(
                        "{n} перехода вместо 1. Ведите исходный адрес сразу на финальный URL.",
                        n=len(redirects),
                    ),
                )
            )
        elif not redirects and self.verdict == "end":
            self.chain.addWidget(note("success", tr("Редиректов нет: адрес отвечает сам.")))
        self.chain.addStretch(1)
        self.side.addWidget(section_label("Сводка"))
        pairs = Pairs(
            (
                "Переходов",
                "Время цепочки",
                "Типы",
                "Смена протокола",
                "Ссылок на начало",
                "Лимит цепочки",
            )
        )
        pairs.set("Переходов", number(len(redirects)) if self.verdict else None)
        times = [h.ms for h in self.hops if h.ms is not None]
        pairs.set(
            "Время цепочки",
            trf("{ms} мс до итога", ms=number(sum(times))) if times and self.verdict else None,
        )
        pairs.set("Типы", " → ".join(str(h.status) for h in self.hops if h.status) or None)
        schemes = [h.url.split(":", 1)[0] for h in self.hops if h.known and h.url]
        pairs.set(
            "Смена протокола",
            f"{schemes[0]} → {schemes[-1]}"
            if len(schemes) > 1 and schemes[0] != schemes[-1]
            else tr("нет")
            if schemes and self.verdict
            else None,
        )
        pairs.set("Ссылок на начало", None)
        pairs.set("Лимит цепочки", trf("{n} переходов", n=MAX_HOPS))
        self.side.addWidget(pairs)
        line = QHBoxLayout()
        line.addWidget(label_row(tr("Цепочка одним запросом"), wrap=False), 1)
        line.addWidget(
            waiting_badge(
                930,
                "Цепочка редиректов одного URL одним ответом; сейчас она собирается переходами по сохранённым страницам скана",
            )
        )
        self.side.addLayout(line)
        self.side.addWidget(section_label("Цепочка canonical"))
        page = self.ctx.page
        canonical, final = clean(page.get("canonical")), clean(page.get("final_canonical"))
        chain = page.get("canonical_chain_json")
        self.side.addWidget(
            label_row(
                joined(
                    " → ",
                    [
                        self.ctx.url or "",
                        canonical or "",
                        final if final and final != canonical else "",
                    ],
                )
                if canonical
                else tr("Canonical не задан"),
                mono=bool(canonical),
                na=not canonical,
            )
        )
        if isinstance(chain, list) and chain:
            self.side.addWidget(note("warn", tr("У canonical есть цепочка переходов.")))
        self.side.addStretch(1)

    @staticmethod
    def _hop_row(index, hop):
        frame = QFrame()
        frame.setProperty("card", "panel")
        layout = QHBoxLayout(frame)
        layout.setContentsMargins(12, 8, 12, 8)
        layout.setSpacing(8)
        kind, text = (
            http_badge(hop.status)
            if hop.known and hop.status is not None
            else ("mut", tr("Нет в скане"))
        )
        badge = QLabel(text)
        badge.setProperty("badge", kind)
        layout.addWidget(badge, 0, Qt.AlignTop)
        box = QVBoxLayout()
        box.setSpacing(2)
        step = trf("шаг {n}", n=index + 1) + (
            f" · {number(hop.ms)} " + tr("мс") if hop.ms is not None else ""
        )
        box.addWidget(label_row(step, wrap=False))
        box.itemAt(0).widget().setProperty("text_style", "meta")
        box.addWidget(label_row(hop.url or "", mono=True))
        layout.addLayout(box, 1)
        return frame


class FactsPage(QWidget):
    """A tab the core cannot fill yet: the neutral badge with a hint, and the facts that are known from the saved page."""

    def __init__(self, tab, ctx, parent=None):
        super().__init__(parent)
        self.tab, self.ctx = tab, ctx
        self.job = CoreJob(ctx.host_ref, self)
        self.job.done.connect(self._resources)
        title, text, issue = UNAVAILABLE[tab]
        layout = QVBoxLayout(self)
        layout.setContentsMargins(20, 12, 20, 12)
        layout.setSpacing(8)
        head = QHBoxLayout()
        head.addWidget(section_label(title))
        head.addWidget(waiting_badge(issue))
        head.addStretch(1)
        layout.addLayout(head)
        description = label_row(text)
        layout.addWidget(description)
        self.facts_box = QWidget()
        self.facts = QVBoxLayout(self.facts_box)
        self.facts.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(self.facts_box)
        layout.addStretch(1)

    def activate(self):
        clear(self.facts)
        page = self.ctx.page
        rows = []
        if self.tab == "res":
            rows = [
                ("Изображений на странице", number(page.get("images_total"))),
                ("Изображений без alt", number(page.get("images_missing_alt_attr"))),
            ]
            self._read_resources()
        if rows:
            pairs = Pairs(tuple(key for key, _v in rows))
            for key, value in rows:
                pairs.set(key, value)
            self.facts.addWidget(pairs)

    def _read_resources(self):
        """Counts of the page's resource graph (``scan-url-query`` columns); the core lists no single resources."""
        if not self.ctx.scan or not self.ctx.url:
            return
        self.job.start(
            query_arguments(
                self.ctx.scan,
                filters=[{"column": "url", "op": "eq", "value": self.ctx.url}],
                limit=1,
                columns=RESOURCE_COLUMNS,
            ),
            self.ctx.url,
        )

    def _resources(self, token, payload):
        row = next(iter(payload.get("rows") or []), None)
        if token != self.ctx.url or not row:
            return
        pairs = Pairs(tuple(title for title, _key in RESOURCE_ROWS))
        for title, key in RESOURCE_ROWS:
            pairs.set(title, number(row.get(key)))
        self.facts.addWidget(pairs)


class HtmlPage(QWidget):
    """Source tab: the retained body of the URL as ``scan-body-read`` returns it (static or rendered), searchable, plus the response facts."""

    LENGTH = 65_536  # characters per read; the core marks a longer body as truncated

    def __init__(self, ctx, parent=None):
        super().__init__(parent)
        self.ctx = ctx
        self.loaded_for = None
        self.job = CoreJob(ctx.host_ref, self)
        self.job.done.connect(self._done)
        self.job.failed.connect(lambda _token, reason: self._message(reason))
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)
        bar = QHBoxLayout()
        bar.setContentsMargins(20, 8, 12, 8)
        bar.setSpacing(8)
        self.mode = Segmented(
            (("static", tr("Исходный")), ("rendered", tr("После рендеринга JS"))),
            value="static",
            accessible_name=tr("Представление HTML"),
        )
        self.mode.changed.connect(lambda _value: self._read())
        self.search = QLineEdit()
        self.search.setPlaceholderText(tr("Поиск в HTML"))
        self.search.setFixedWidth(240)
        self.search.returnPressed.connect(self._find)
        self.status = QLabel()
        self.status.setProperty("text_style", "meta")
        bar.addWidget(self.mode)
        bar.addWidget(self.status, 1)
        bar.addWidget(self.search)
        outer.addLayout(bar)
        body = QHBoxLayout()
        body.setContentsMargins(0, 0, 0, 0)
        body.setSpacing(0)
        self.source = QPlainTextEdit()
        self.source.setReadOnly(True)
        self.source.setLineWrapMode(QPlainTextEdit.NoWrap)
        self.source.setFont(QFontDatabase.systemFont(QFontDatabase.FixedFont))
        self.source.setFrameShape(QFrame.NoFrame)
        body.addWidget(self.source, 1)
        side = QFrame()
        side.setFixedWidth(300)
        side_layout = QVBoxLayout(side)
        side_layout.setContentsMargins(16, 12, 16, 12)
        side_layout.setSpacing(12)
        side_layout.addWidget(section_label("Сохранённое тело"))
        self.facts = Pairs(
            ("Тело сохранено", "Размер, байт", "SHA-256", "Представление"), mono=("SHA-256",)
        )
        side_layout.addWidget(self.facts)
        side_layout.addStretch(1)
        body.addWidget(side)
        outer.addLayout(body, 1)

    def _message(self, text):
        self.source.setPlainText("")
        self.status.setText(text)

    def _find(self):
        text = self.search.text()
        if text and not self.source.find(text):
            self.source.moveCursor(QTextCursor.Start)  # wrap around
            self.source.find(text)

    def _read(self):
        key = (self.ctx.scan, self.ctx.url, self.mode.value())
        if not self.ctx.scan or not self.ctx.url or self.loaded_for == key:
            return
        self.loaded_for = key
        self._message(tr("Чтение сохранённого тела…"))
        self.job.start(
            [
                "scan-body-read",
                "--scan",
                self.ctx.scan,
                "--url",
                self.ctx.url,
                "--representation",
                key[2],
                "--length",
                str(self.LENGTH),
            ],
            key,
        )

    def _done(self, token, payload):
        if token != self.loaded_for:
            return
        if payload.get("ok") is not True or payload.get("state") != "available":
            return self._message(
                payload.get("reason") or tr("Тело этой страницы не сохранено в скане")
            )
        self.source.setPlainText(payload.get("text") or "")
        self.status.setText(
            tr("Показано начало: {n} знаков").replace("{n}", number(self.LENGTH))
            if payload.get("truncated")
            else ""
        )

    def activate(self):
        page, detail = self.ctx.page, self.ctx.detail
        response = first_response(detail)
        sha = response.get("body_sha256")
        state = response.get("body_state")
        self.facts.set(
            "Тело сохранено",
            {"complete": tr("Полностью"), "partial": tr("Частично")}.get(state, clean(state)),
        )
        self.facts.set("Размер, байт", number(response.get("reported_size_bytes")))
        self.facts.set(
            "SHA-256", f"{sha[:8]}…{sha[-4:]}" if isinstance(sha, str) and len(sha) > 12 else None
        )
        self.facts.set(
            "Представление",
            source_text({"source_kind": (detail or {}).get("source", {}).get("source_kind")}, page),
        )
        self._read()

    def context_changed(self):
        self.job.cancel()
        self.loaded_for = None
        self.source.setPlainText("")
        self.status.setText("")


class SchemaPage(QWidget):
    """Show exact-URL retained JSON-LD; keep other syntax and validation explicitly unavailable."""

    def __init__(self, ctx, parent=None):
        super().__init__(parent)
        self.ctx = ctx
        self.loaded_for = None
        self._pending = None
        self.job = CoreJob(ctx.host_ref, self)
        self.job.done.connect(self._done)
        self.job.failed.connect(self._failed)
        title, text, issue = UNAVAILABLE["schema"]
        layout = QVBoxLayout(self)
        layout.setContentsMargins(20, 12, 20, 12)
        layout.setSpacing(8)
        head = QHBoxLayout()
        head.addWidget(section_label(title))
        if issue is not None:
            head.addWidget(waiting_badge(issue))
        head.addStretch(1)
        layout.addLayout(head)
        layout.addWidget(label_row(text))
        source_controls = QHBoxLayout()
        self.representation = QComboBox()
        self.representation.addItems(("static", "rendered", "legacy_fragment"))
        self.variant_key = QLineEdit()
        self.variant_key.setPlaceholderText("Optional retained variant key")
        self.read_button = QPushButton("Read retained evidence")
        self.read_button.clicked.connect(self.activate)
        source_controls.addWidget(QLabel("Representation"))
        source_controls.addWidget(self.representation)
        source_controls.addWidget(self.variant_key, 1)
        source_controls.addWidget(self.read_button)
        layout.addLayout(source_controls)
        self.count = label_row("")
        self.count.setProperty("text_style", "meta")
        layout.addWidget(self.count)
        columns = QHBoxLayout()
        columns.setSpacing(12)
        self.blocks = StatePanel(
            "waiting",
            "Блоки не перечислены",
            "Названия, позиции и статус каждого блока ядро по URL не отдаёт.",
        )
        self.tree = StatePanel(
            "waiting",
            "Содержимое блока не получено",
            "Дерево свойств появится, когда ядро отдаст разобранный блок.",
        )
        self.checks = StatePanel(
            "waiting",
            "Проверка разметки ещё не получена",
            "Локальная validation, ошибки и eligibility появятся из сохранённого документа; это не прогноз показа Google.",
        )
        for panel, stretch in ((self.blocks, 3), (self.tree, 5), (self.checks, 3)):
            columns.addWidget(panel, stretch)
        layout.addLayout(columns)
        self.facts_box = QWidget()
        self.facts = QVBoxLayout(self.facts_box)
        self.facts.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(self.facts_box)

    def activate(self):
        clear(self.facts)
        page = self.ctx.page
        found, parsed = (
            number(page.get("jsonld_blocks_found")),
            number(page.get("jsonld_blocks_parsed")),
        )
        self.count.setText(
            joined(
                " · ",
                [
                    tr("Найдено блоков JSON-LD"),
                    found or tr("Нет данных"),
                    tr("разобрано") + " " + (parsed or tr("Нет данных")),
                ],
            )
        )
        hreflang = page.get("hreflang_json")
        rows = [("Записей hreflang", number(len(hreflang)) if isinstance(hreflang, list) else None)]
        pairs = Pairs(tuple(key for key, _v in rows))
        for key, value in rows:
            pairs.set(key, value)
        self.facts.addWidget(pairs)
        representation = self.representation.currentText()
        variant_key = self.variant_key.text().strip()
        key = (self.ctx.scan, self.ctx.url, representation, variant_key)
        if not self.ctx.scan or not self.ctx.url:
            self.blocks.text.setText("Select a retained scan URL to read its saved JSON-LD blocks.")
            return
        if self.loaded_for == key or (self._pending and self._pending[0] == key):
            return
        token = object()
        self._pending = (key, token)
        self.blocks.text.setText("Reading retained structured data…")
        self.tree.text.setText(
            f"The exact URL and {representation} retained document are being read locally."
        )
        arguments = [
            "scan-structured-blocks",
            "--scan",
            self.ctx.scan,
            "--url",
            self.ctx.url,
            "--representation",
            representation,
        ]
        if variant_key:
            arguments.extend(("--variant-key", variant_key))
        self.job.start(arguments, token)

    def _done(self, token, payload):
        if self._pending is None or token is not self._pending[1]:
            return
        self.loaded_for, self._pending = self._pending[0], None
        state = payload.get("state", "unknown")
        reason = payload.get("reason")
        detail = payload.get("schema_detail")
        detail = detail if isinstance(detail, dict) else {}
        formats = detail.get("formats")
        formats = formats if isinstance(formats, dict) else {}
        format_lines = []
        for name in ("jsonld", "microdata", "rdfa"):
            evidence = formats.get(name)
            evidence = evidence if isinstance(evidence, dict) else {}
            count = next(
                (
                    evidence.get(field)
                    for field in ("count", "block_count", "entity_count")
                    if type(evidence.get(field)) is int
                ),
                None,
            )
            count_name = "blocks" if name == "jsonld" else "entities"
            format_lines.append(
                f"{name}: {evidence.get('state') or 'not reported'} · "
                f"{count if count is not None else 'count unavailable'} {count_name}"
                + (f" · {evidence['reason']}" if evidence.get("reason") else "")
            )
        source_state = payload.get("source_state")
        analysis_state = detail.get("analysis_state", "unavailable")
        root_count = payload.get("block_count")
        root_suffix = (f" · source {source_state}" if source_state else "") + (
            f" · {root_count} JSON-LD blocks" if type(root_count) is int else ""
        )
        if payload.get("blocks_truncated"):
            root_suffix += " · block output truncated"
        self.blocks.title.setText(
            f"Schema analysis: {analysis_state} · JSON-LD: {state}{root_suffix}"
        )
        notes = format_lines[:]
        partial_reasons = detail.get("partial_reasons")
        if isinstance(partial_reasons, list):
            notes.extend(str(note) for note in partial_reasons)
        if reason:
            notes.append(reason)
        detail_reason = detail.get("reason")
        if detail_reason and detail_reason != reason:
            notes.append(str(detail_reason))
        self.blocks.text.setText("\n".join(str(note) for note in notes))

        jsonld = formats.get("jsonld")
        blocks = payload.get("blocks")
        if blocks is None and isinstance(jsonld, dict):
            blocks = jsonld.get("blocks")
        structured = payload.get("structured_evidence")
        if blocks is None and isinstance(structured, dict):
            blocks = structured.get("blocks")
        self.tree.title.setText("JSON-LD blocks")
        self.tree.text.setText(
            json.dumps(blocks, ensure_ascii=False, indent=2)[:30_000]
            if isinstance(blocks, list)
            else "No retained JSON-LD block detail was returned."
        )

        validation = detail.get("validation")
        validation = validation if isinstance(validation, dict) else {}
        checks = {
            "rules_version": detail.get("rules_version") or validation.get("rules_version"),
            "syntax_state": validation.get("syntax_state"),
            "structural_state": validation.get("structural_state"),
            "feature_eligibility_state": validation.get("feature_eligibility_state"),
            "feature_checks": validation.get("feature_checks"),
            "unsupported_context_normalization": validation.get(
                "unsupported_context_normalization"
            ),
            "entity_errors": validation.get("entity_errors"),
            "entity_warnings": validation.get("entity_warnings"),
            "markup_coverage": detail.get("markup_coverage"),
            "parse_errors": detail.get("parse_errors"),
            "partial_reasons": detail.get("partial_reasons"),
            "graph": detail.get("graph"),
            "entities": detail.get("entities"),
            "local_eligibility": detail.get("local_eligibility") or detail.get("rich_results"),
            "google_display": detail.get("google_display", "unknown"),
        }
        self.checks.title.setText("Retained local validation")
        self.checks.text.setText(
            json.dumps(checks, ensure_ascii=False, indent=2)[:30_000]
            if validation or detail.get("local_eligibility") or detail.get("rich_results")
            else "The retained URL reader did not return validation fields."
        )

        source = payload.get("source")
        source = source if isinstance(source, dict) else payload
        identity = Pairs(
            (
                "Scan UUID",
                "Evidence revision",
                "Document ID",
                "Representation",
                "Variant",
                "Content type",
                "Body SHA-256",
                "Decoded SHA-256",
                "Source state",
                "Analysis state",
            ),
            mono=("Scan UUID", "Body SHA-256", "Decoded SHA-256"),
        )
        for label, field in (
            ("Scan UUID", "scan_uuid"),
            ("Evidence revision", "evidence_revision"),
            ("Document ID", "document_id"),
            ("Representation", "representation"),
            ("Variant", "variant_key"),
            ("Content type", "content_type"),
            ("Body SHA-256", "raw_body_sha256"),
            ("Decoded SHA-256", "sha256"),
            ("Source state", "source_state"),
        ):
            identity.set(label, source.get(field))
        identity.set("Analysis state", detail.get("analysis_state"))
        self.facts.addWidget(identity)
        language = payload.get("language_evidence")
        if isinstance(language, dict):
            self.facts.addWidget(
                label_row("Language evidence: " + json.dumps(language, ensure_ascii=False)[:2000])
            )

    def _failed(self, token, reason):
        if self._pending is None or token is not self._pending[1]:
            return
        self._pending = None
        self.blocks.text.setText(f"Saved JSON-LD reader unavailable: {str(reason)[:300]}")


class UrlCard(QFrame):
    """The URL card: header (HTTP, address, scan) + tabs of the Url* sheets; ``set_expanded`` only changes the owner's layout."""

    back_requested = pyqtSignal()
    hide_requested = pyqtSignal()
    note_requested = pyqtSignal()
    expand_toggled = pyqtSignal(bool)

    def __init__(self, host, parent=None):
        super().__init__(parent)
        self.host = host
        self.ctx = Context()
        self.ctx.host_ref = host
        self.expanded = False
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        bar = QHBoxLayout()
        bar.setContentsMargins(4, 0, 8, 0)
        self.tabs = QTabBar()
        self.tabs.setUsesScrollButtons(True)
        self.tabs.setElideMode(Qt.ElideNone)
        self.tabs.setObjectName("cardTabs")
        self.tabs.setProperty("tabs", "underline")
        self.tabs.setDrawBase(False)
        self.tabs.setExpanding(False)
        self.tabs.setUsesScrollButtons(True)
        for tab in TAB_ORDER:
            self.tabs.addTab(tr(TAB_LABELS[tab]))
        self.tabs.currentChanged.connect(self._tab_changed)
        bar.addWidget(self.tabs, 1)
        self.tab_more = QToolButton()
        self.tab_more.setPopupMode(QToolButton.InstantPopup)
        self.tab_more.setToolTip(tr("Все вкладки URL"))
        self.tab_more.setAccessibleName(tr("Все вкладки URL"))
        self.tab_more.setMenu(QMenu(self.tab_more))
        for index, tab in enumerate(TAB_ORDER):
            action = self.tab_more.menu().addAction(tr(TAB_LABELS[tab]))
            action.triggered.connect(lambda _c=False, i=index: self.tabs.setCurrentIndex(i))
        self.tab_more.hide()
        bar.addWidget(self.tab_more)
        self.back = icon_button("arrow_back", "Предыдущий URL в истории", False)
        self.back.clicked.connect(self.back_requested.emit)
        self.expand_button = icon_button("open_in_full", "Развернуть карточку")
        self.expand_button.clicked.connect(lambda: self.set_expanded(not self.expanded, emit=True))
        self.hide_button = icon_button("bottom_panel_close", "Скрыть детали")
        self.hide_button.clicked.connect(self.hide_requested.emit)
        for widget in (self.back, self.expand_button, self.hide_button):
            bar.addWidget(widget)
        layout.addLayout(bar)
        head = QFrame()
        head.setProperty("table_head", True)
        head_layout = QHBoxLayout(head)
        head_layout.setContentsMargins(20, 8, 12, 8)
        head_layout.setSpacing(8)
        self.head = UrlHead()
        self.head.note_requested.connect(self.note_requested.emit)
        head_layout.addWidget(self.head, 1)
        self.scan_caption = QLabel()
        self.scan_caption.setProperty("text_style", "meta")
        head_layout.addWidget(self.scan_caption)
        self.head_layout = head_layout
        layout.addWidget(head)
        self.pages = QStackedWidget()
        layout.addWidget(self.pages, 1)
        self.overview = OverviewPage()
        self.headers = HeadersPage()
        self.links = LinksPage(self.ctx)
        self.graph = GraphPage(self.ctx)
        self.graph.table_requested.connect(lambda: self.select_tab("links"))
        self.redirects = RedirectsPage(self.ctx)
        self.history = HistoryPage(self.ctx)
        self.history.counted.connect(lambda n: self._counter("hist", number(n) if n else ""))
        self.snippet = SnippetPage(host)
        self.facts = {
            tab: HtmlPage(self.ctx)
            if tab == "html"
            else SchemaPage(self.ctx)
            if tab == "schema"
            else FactsPage(tab, self.ctx)
            for tab in ("html", *UNAVAILABLE)
        }
        self.index = {}
        widgets = {
            "info": scrolled(self.overview),
            "hdr": scrolled(self.headers),
            "links": self.links,
            "graph": self.graph,
            "redir": scrolled(self.redirects),
            "hist": scrolled(self.history),
            "snip": self.snippet,
            **{tab: scrolled(page) for tab, page in self.facts.items()},
        }
        for tab in TAB_ORDER:
            self.index[tab] = self.pages.addWidget(widgets[tab])
        self.placeholder = StatePanel(
            "empty", "Выберите URL в таблице", "Детали сохранённого ответа появятся здесь"
        )
        self.index["none"] = self.pages.addWidget(self.placeholder)
        from .url_detail import RunSummary

        self.summary = RunSummary()
        self.summary.setProperty("aside", False)
        self.summary.hide_button.hide()
        self.index["summary"] = self.pages.addWidget(self.summary)
        self.pages.setCurrentIndex(self.index["none"])
        self.tab = "info"
        self.has_row = False
        self.links.counts_changed.connect(self._links_counted)
        self.redirects.counted.connect(lambda n: self._counter("redir", number(n)))
        self.overview.related_hint = None

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self.scan_caption.setVisible(event.size().width() >= 1000)
        # The strip falls back to an overflow menu when its tabs cannot fit beside the header buttons.
        fits = self.tabs.sizeHint().width() <= self.tabs.parentWidget().width() - 140
        self.tabs.setVisible(fits)
        self.tab_more.setVisible(not fits)

    # ---- modes -----------------------------------------------------------------------------------------------
    def set_expanded(self, expanded, emit=False):
        self.expanded = expanded
        self.expand_button.setIcon(material_icon("fullscreen_exit" if expanded else "open_in_full"))
        tip = tr("Свернуть карточку" if expanded else "Развернуть карточку")
        self.expand_button.setToolTip(tip)
        self.expand_button.setAccessibleName(tip)
        if emit:
            self.expand_toggled.emit(expanded)

    def set_agent_visible(self, visible):
        self.head.note.setVisible(visible)

    # ---- tabs ----------------------------------------------------------------------------------------------------
    def _tab_changed(self, index):
        self.tab = TAB_ORDER[index] if index < len(TAB_ORDER) else "summary"
        if 0 <= index < len(TAB_ORDER):
            self.tab_more.setText(tr(TAB_LABELS[TAB_ORDER[index]]))
        self.head.note.setVisible(self.tab == "info" and self.head.note_allowed)
        self._show()

    def _show(self):
        if self.tab == "summary":
            self.pages.setCurrentIndex(self.index["summary"])
            return
        self.pages.setCurrentIndex(self.index[self.tab] if self.has_row else self.index["none"])
        if self.has_row:
            self._activate(self.tab)

    def _activate(self, tab):
        if tab == "links":
            self.links.activate()
        elif tab == "graph":
            self.graph.activate()
        elif tab == "redir":
            self.redirects.activate()
        elif tab == "hist":
            self.history.activate()
        elif tab in self.facts:
            self.facts[tab].activate()

    def select_tab(self, tab):
        self.tabs.setCurrentIndex(len(TAB_ORDER) if tab == "summary" else TAB_ORDER.index(tab))

    def set_summary_tab(self, visible):
        """The run summary lives in the card as a tab when the window is too narrow for the right-hand aside."""
        present = self.tabs.count() > len(TAB_ORDER)
        if visible and not present:
            self.tabs.addTab(tr("Сводка"))
        elif not visible and present:
            if self.tab == "summary":
                self.select_tab("info")
            self.tabs.removeTab(len(TAB_ORDER))

    def _counter(self, tab, text):
        index = TAB_ORDER.index(tab)
        self.tabs.setTabText(index, tr(TAB_LABELS[tab]) + (f"  {text}" if text else ""))

    def _links_counted(self, incoming, outgoing):
        if incoming is None and outgoing is None:
            self._counter("links", "")
        else:
            self._counter(
                "links",
                f"{number(incoming) if incoming is not None else '…'} / {number(outgoing) if outgoing is not None else '…'}",
            )

    # ---- data ----------------------------------------------------------------------------------------------------
    def render(self, row, page, detail, state, text=""):
        """state: none | loading | ready | error. ``row`` is the table row, ``page`` the detail page of the core."""
        had = self.ctx.url
        scan = self.host.selected_scan_path if hasattr(self.host, "selected_scan_path") else None
        self.has_row = state != "none"
        self.ctx.scan = scan
        self.ctx.row = row or {}
        self.ctx.detail = detail
        self.ctx.url = (row or {}).get("url") if self.has_row else None
        if self.ctx.url != had:
            self.links.context_changed()
            self.graph.context_changed()
            self.redirects.context_changed()
            self.history.context_changed()
            self.facts["html"].context_changed()
            self._counter("redir", "")
        if self.has_row:
            self.head.set_row(row)
            self.overview.fill(row, page, detail, state, text)
            self.headers.fill(detail)
            self.snippet.fill(page, self.ctx.url)
            count = self.headers.count()
            self._counter("hdr", number(count) if count else "")
            source = (detail or {}).get("source") or {}
            uuid = source.get("scan_uuid")
            self.scan_caption.setText(
                joined(
                    " · ",
                    [
                        short_run_id(uuid),
                        stamp_text(first_response(detail).get("received_at")),
                        source_text(source, page),
                    ],
                )
            )
            self.scan_caption.setToolTip(uuid or "")
        else:
            self.head.set_row(None)
            self.scan_caption.setText("")
        self._show()

    def retranslate(self):
        for index, tab in enumerate(TAB_ORDER):
            self.tabs.setTabText(index, tr(TAB_LABELS[tab]))
        if self.tabs.count() > len(TAB_ORDER):
            self.tabs.setTabText(len(TAB_ORDER), tr("Сводка"))
        self.links.retranslate()
        self.graph.retranslate()
        self.history.retranslate()

    def shutdown(self):
        for job in (
            self.links.job,
            self.links.counter,
            self.graph.job,
            self.redirects.job,
            self.history.job,
            self.facts["html"].job,
            self.facts["res"].job,
        ):
            job.shutdown()


__all__ = ["QToolButton", "UrlCard", "i18n", "key_label"]
