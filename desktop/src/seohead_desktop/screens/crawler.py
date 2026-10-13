"""Screen «Краулер»: an ad-hoc crawl of an address without a project, through the core ``crawl-site`` command.

Start runs ``seohead crawl-site --url|--urls|--sitemap … --scan-out <temporary file>`` as an owned child process; the
progress line the core prints drives the status strip. Rows, counters and the export come from ``scan-url-query`` over
that file (filter, search and paging are the core's). Nothing starts when the screen opens. The result lives in a
temporary folder that is removed on «Очистить», on the next crawl and when the application quits.
"""

from __future__ import annotations

import os
import re
import shutil
import signal
import tempfile
import time
from pathlib import Path
from urllib.parse import urlsplit

from PyQt5.QtCore import QCoreApplication, QProcess, Qt, QTimer
from PyQt5.QtWidgets import (
    QButtonGroup,
    QFileDialog,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QPushButton,
    QSizePolicy,
    QSpinBox,
    QSplitter,
    QStackedWidget,
    QTabBar,
    QTableView,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import theming
from ..i18n import tr, trf
from ..project_create import normalize_target
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import BadgeDelegate, StatePanel, style_table
from .scan_common import number
from .url import COLUMNS, UrlDelegate, UrlPageModel
from .url_query import PAGE, CoreJob, index_badge, query_arguments, reason_text

COMPACT_WIDTH = 1000  # below this the overview column is hidden
REFRESH_MS = 3000  # the table follows a running crawl at this pace
DEFAULT_LIMIT = 200
MODES = (("site", "Сайт"), ("list", "Список"), ("sitemap", "Sitemap"))
PLACEHOLDERS = {
    "site": "Адрес сайта",
    "list": "Адреса через пробел или запятую",
    "sitemap": "Адрес sitemap.xml",
}
EMPTY_FILTER = "Всё"
TAB_FILTERS = (
    ("Всё", []),
    ("HTML", [{"column": "content_type", "op": "contains", "value": "html"}]),
    ("JavaScript", [{"column": "content_type", "op": "contains", "value": "javascript"}]),
    ("CSS", [{"column": "content_type", "op": "contains", "value": "css"}]),
    ("Изображения", [{"column": "content_type", "op": "contains", "value": "image/"}]),
    ("Коды ответа", [{"column": "status_class", "op": "ne", "value": "2xx"}]),
    ("Директивы", [{"column": "meta_robots", "op": "not_empty"}]),
)
# icon, label, depth, core facet (the counter), filters; a row without a facet is a heading
OVERVIEW = (
    ("folder", "Всё", 0, "all", []),
    ("folder_open", "Коды ответа", 0, None, None),
    (
        "check_circle",
        "2xx успешно",
        1,
        "status_2xx",
        [{"column": "status_class", "op": "eq", "value": "2xx"}],
    ),
    (
        "subdirectory_arrow_right",
        "3xx редирект",
        1,
        "status_3xx",
        [{"column": "status_class", "op": "eq", "value": "3xx"}],
    ),
    (
        "warning",
        "4xx ошибка клиента",
        1,
        "status_4xx",
        [{"column": "status_class", "op": "eq", "value": "4xx"}],
    ),
    (
        "error",
        "5xx ошибка сервера",
        1,
        "status_5xx",
        [{"column": "status_class", "op": "eq", "value": "5xx"}],
    ),
    ("folder_open", "Title", 0, None, None),
    ("remove", "Отсутствует", 1, "title_missing", [{"column": "title", "op": "empty"}]),
    (
        "content_copy",
        "Дубли",
        1,
        "title_duplicate",
        [{"column": "title_duplicate", "op": "eq", "value": True}],
    ),
    (
        "fit_width",
        "Длиннее 60 знаков",
        1,
        "title_long",
        [{"column": "title_length", "op": "gt", "value": 60}],
    ),
    ("folder_open", "Canonical", 0, None, None),
    ("remove", "Отсутствует", 1, "canonical_missing", [{"column": "canonical", "op": "empty"}]),
    (
        "open_in_new",
        "На другой URL",
        1,
        "canonical_other",
        [{"column": "canonical_other", "op": "eq", "value": True}],
    ),
)
HIDDEN_COLUMNS = (6, 9)  # inlinks and issue count: not part of the core page answer
PROGRESS = re.compile(
    r"(\d+) fetched, (\d+) known .*?(?:([\d.]+) req/s|rate n/a)"
)  # the core's own progress line
STATES = {
    "idle": ("mut", "Краул не запущен"),
    "running": ("info", "Идёт краул"),
    "stopping": ("warn", "Останавливаю"),
    "done": ("ok", "Краул завершён"),
    "stopped": ("warn", "Краул остановлен"),
    "failed": ("err", "Краул не удался"),
}


def crawl_arguments(mode, text, limit, scan_out):
    """``crawl-site`` arguments for the typed address(es); ValueError carries the Russian reason."""
    if mode == "list":
        urls = [normalize_target(part) for part in re.split(r"[\s,]+", text.strip()) if part]
        if not urls:
            raise ValueError("Укажите хотя бы один адрес")
        for url in urls:
            _check_url(url)
        return ["crawl-site", "--urls", ",".join(urls), "--scan-out", scan_out]
    url = normalize_target(text)
    _check_url(url)
    arguments = ["crawl-site", "--max-urls", str(limit), "--scan-out", scan_out]
    if mode == "sitemap":
        return [*arguments, "--sitemap", url, "--sitemap-only"]
    return [*arguments, "--url", url]


def _check_url(url):
    try:
        parts = urlsplit(url)
        valid = (
            parts.scheme in ("http", "https")
            and bool(parts.hostname)
            and not parts.username
            and parts.port != 0
        )
    except ValueError:
        valid = False
    if not valid:
        raise ValueError("Адрес должен начинаться с http:// или https:// и содержать домен")


class CrawlerScreen(QWidget):
    """Board «Краулер»: toolbar, check tabs, results table, overview counters, details and the status strip."""

    def __init__(self, host=None, parent=None):
        super().__init__(parent)
        self.host = host
        self.setProperty("spaciousPage", False)
        self.mode = "site"
        self.state = "idle"
        self.workdir = None
        self.scan_path = None
        self.process = None
        self.stderr = ""
        self.started_at = 0.0
        self.offset = 0
        self.total = None
        self.revision = 0
        self.base_filters = []
        self.speed_value = None
        self.counts = {}
        self.page_job = CoreJob(host, self)
        self.page_job.done.connect(self._page_done)
        self.page_job.failed.connect(self._page_failed)
        self.facet_job = CoreJob(host, self)
        self.facet_job.done.connect(self._facets_done)
        self.facet_job.failed.connect(lambda _token, _reason: None)
        self.export_job = CoreJob(host, self)
        self.export_job.done.connect(self._export_done)
        self.export_job.failed.connect(self._export_failed)
        self._refresh = QTimer(self)
        self._refresh.setInterval(REFRESH_MS)
        self._refresh.timeout.connect(self._reload_running)
        self._search = QTimer(self)
        self._search.setSingleShot(True)
        self._search.setInterval(350)
        self._search.timeout.connect(lambda: self._reload())
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
        splitter.setStretchFactor(1, 1)
        body.addWidget(splitter, 1)
        self.overview = self._overview()
        body.addWidget(self.overview)
        root.addLayout(body, 1)
        root.addWidget(self._status())
        self._set_state("idle")
        app = QCoreApplication.instance()
        if app is not None:
            app.aboutToQuit.connect(self.shutdown)

    # ---- layout ---------------------------------------------------------------------------------------------
    def resizeEvent(self, event):
        super().resizeEvent(event)
        self.overview.setVisible(self.width() >= COMPACT_WIDTH)

    def showEvent(self, event):
        super().showEvent(event)
        self._sync_start()

    def _toolbar(self):
        bar = QFrame()
        bar.setProperty("role", "toolbar")
        row = QHBoxLayout(bar)
        row.setContentsMargins(12, 8, 12, 8)
        row.setSpacing(8)
        chip = QLabel(tr("Краулер · без проекта"))
        chip.setProperty("badge", "info")
        row.addWidget(chip)
        self.address = QLineEdit()
        self.address.setPlaceholderText(tr(PLACEHOLDERS["site"]))
        self.address.setMinimumWidth(120)
        self.address.returnPressed.connect(self._start_or_stop)
        row.addWidget(self.address, 1)
        segmented = QFrame()
        segmented.setProperty("segmented", "true")
        seg_row = QHBoxLayout(segmented)
        seg_row.setContentsMargins(2, 2, 2, 2)
        seg_row.setSpacing(2)
        group = QButtonGroup(segmented)
        group.setExclusive(True)
        self.mode_buttons = {}
        for key, text in MODES:
            button = QToolButton()
            button.setProperty("segment", "true")
            button.setText(tr(text))
            button.setCheckable(True)
            button.setChecked(key == self.mode)
            button.clicked.connect(lambda _checked, k=key: self._set_mode(k))
            group.addButton(button)
            seg_row.addWidget(button)
            self.mode_buttons[key] = button
        row.addWidget(segmented)
        self.limit = QSpinBox()
        self.limit.setRange(1, 100000)
        self.limit.setValue(DEFAULT_LIMIT)
        self.limit.setPrefix(tr("Лимит URL: "))
        self.limit.setMinimumWidth(150)
        self.limit.setToolTip(tr("Сколько адресов обойти самое большее"))
        row.addWidget(self.limit)
        self.start = QPushButton(tr("Старт"))
        self.start.setProperty("role", "primary")
        self.start.setIcon(material_icon("play_arrow"))
        self.start.clicked.connect(self._start_or_stop)
        row.addWidget(self.start)
        self.clear = QPushButton(tr("Очистить"))
        self.clear.clicked.connect(self.clear_results)
        row.addWidget(self.clear)
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
                text="без проекта нет истории, задач и сравнения сканов. Создайте проект, чтобы перепроверять исправления.",
            )
        )
        text.setTextFormat(Qt.RichText)
        text.setWordWrap(True)
        row.addWidget(text, 1)
        self._note.setVisible(False)
        return self._note

    def _check_tabs(self):
        self.tabs = QTabBar()
        self.tabs.setProperty("tabs", "underline")
        self.tabs.setExpanding(False)
        self.tabs.setUsesScrollButtons(True)
        self.tabs.setElideMode(Qt.ElideNone)
        for text, _filters in TAB_FILTERS:
            self.tabs.addTab(tr(text))
        self.tabs.currentChanged.connect(self._tab_changed)
        return self.tabs

    def _main(self):
        column = QVBoxLayout()
        column.setContentsMargins(0, 0, 0, 0)
        column.setSpacing(0)
        filters = QHBoxLayout()
        filters.setContentsMargins(10, 8, 10, 8)
        filters.setSpacing(8)
        self.filter_label = QLabel()
        self.filter_label.setProperty("badge", "info")
        self.filter_label.setSizePolicy(QSizePolicy.Maximum, QSizePolicy.Fixed)
        filters.addWidget(self.filter_label)
        self.search = QLineEdit()
        self.search.setPlaceholderText(tr("Поиск в адресах…"))
        self.search.setClearButtonEnabled(True)
        self.search.setMinimumWidth(120)
        self.search.setMaximumWidth(260)
        self.search.textChanged.connect(lambda _text: self._search.start())
        filters.addWidget(self.search, 1)
        self.export = QPushButton(tr("Экспорт"))
        self.export.clicked.connect(self._export)
        filters.addWidget(self.export)
        column.addLayout(filters)
        self.model = UrlPageModel(self)
        self.table = style_table(QTableView(), "compact")
        self.table.setModel(self.model)
        self.table.setFrameShape(QFrame.NoFrame)
        self.table.setTextElideMode(Qt.ElideRight)
        self.table.setItemDelegateForColumn(0, UrlDelegate(self.table))
        badges = BadgeDelegate(self.table)
        for index in (1, 3):
            self.table.setItemDelegateForColumn(index, badges)
        header = self.table.horizontalHeader()
        header.setStretchLastSection(False)
        header.setSectionResizeMode(0, QHeaderView.Stretch)
        header.setSectionResizeMode(4, QHeaderView.Interactive)
        self.table.setColumnWidth(4, 180)
        for index in HIDDEN_COLUMNS:
            self.table.setColumnHidden(index, True)
        self.table.selectionModel().currentRowChanged.connect(self._row_changed)
        self.stack = QStackedWidget()
        self.stack.addWidget(self.table)
        self.panel_host = QWidget()
        QVBoxLayout(self.panel_host).setContentsMargins(0, 0, 0, 0)
        self.stack.addWidget(self.panel_host)
        column.addWidget(self.stack, 1)
        foot = QHBoxLayout()
        foot.setContentsMargins(12, 4, 8, 4)
        self.foot_text = QLabel()
        self.foot_text.setProperty("text_style", "meta")
        foot.addWidget(self.foot_text, 1)
        self.prev = QToolButton()
        self.prev.setIcon(material_icon("chevron_left"))
        self.prev.setToolTip(tr("Предыдущая страница"))
        self.prev.clicked.connect(lambda: self._go_page(-1))
        self.next = QToolButton()
        self.next.setIcon(material_icon("chevron_right"))
        self.next.setToolTip(tr("Следующая страница"))
        self.next.clicked.connect(lambda: self._go_page(1))
        foot.addWidget(self.prev)
        foot.addWidget(self.next)
        column.addLayout(foot)
        self._show_panel(
            "empty",
            "Введите адрес и нажмите «Старт»",
            "Краул без проекта сохраняет результат во временную папку до закрытия окна.",
        )
        self._set_filter(0)
        return column

    def _detail_pane(self):
        pane = QWidget()
        column = QVBoxLayout(pane)
        column.setContentsMargins(0, 0, 0, 0)
        column.setSpacing(0)
        self._details = QLabel(tr("Выберите адрес в таблице, чтобы увидеть сведения"))
        self._details.setProperty("text_style", "meta")
        self._details.setContentsMargins(16, 10, 16, 10)
        column.addWidget(self._details)
        grid_host = QWidget()
        grid = QGridLayout(grid_host)
        grid.setContentsMargins(16, 4, 16, 12)
        grid.setHorizontalSpacing(24)
        grid.setVerticalSpacing(8)
        self._fields = {}
        for column_index, keys in enumerate(
            (("url", "status_code", "indexable"), ("title", "crawl_depth", "response_time"))
        ):
            for row_index, key in enumerate(keys):
                title = next(c[1] for c in COLUMNS if c[0] == key)
                caption = QLabel(tr(title))
                caption.setProperty("text_style", "meta")
                value = QLabel()
                value.setTextInteractionFlags(Qt.TextSelectableByMouse)
                value.setWordWrap(True)
                grid.addWidget(caption, row_index, column_index * 2)
                grid.addWidget(value, row_index, column_index * 2 + 1)
                self._fields[key] = value
        grid.setColumnStretch(1, 1)
        grid.setColumnStretch(3, 1)
        grid.setRowStretch(3, 1)
        column.addWidget(grid_host, 1)
        return pane

    def _overview(self):
        panel = QFrame()
        panel.setFixedWidth(300)
        panel.setProperty("role", "overview")
        column = QVBoxLayout(panel)
        column.setContentsMargins(0, 0, 0, 0)
        column.setSpacing(0)
        title = QLabel(tr("Обзор"))
        title.setProperty("text_style", "overline")
        title.setContentsMargins(12, 10, 12, 6)
        column.addWidget(title)
        self.counters = {}
        for index, (icon_name, text, depth, facet, _filters) in enumerate(OVERVIEW):
            button = QPushButton()
            button.setFlat(True)
            button.setStyleSheet("QPushButton { border: none; background: transparent; }")
            button.setEnabled(facet is not None)
            button.setCursor(Qt.PointingHandCursor if facet else Qt.ArrowCursor)
            row = QHBoxLayout(button)
            row.setContentsMargins(6 + depth * 16, 2, 10, 2)
            row.setSpacing(6)
            row.addWidget(MaterialIconLabel(icon_name, 16, color="role:text_muted"))
            label = QLabel(tr(text))
            label.setEnabled(True)
            row.addWidget(label)
            row.addStretch(1)
            count = QLabel("—")
            count.setProperty("text_style", "meta")
            row.addWidget(count)
            if facet:
                button.clicked.connect(lambda _c, i=index: self._select_overview(i))
                self.counters[facet] = count
            column.addWidget(button)
        column.addStretch(1)
        return panel

    def _status(self):
        bar = QFrame()
        bar.setProperty("role", "status")
        row = QHBoxLayout(bar)
        row.setContentsMargins(12, 4, 16, 4)
        row.setSpacing(20)
        self.state_badge = QLabel()
        row.addWidget(self.state_badge)
        self.speed = QLabel()
        self.crawled = QLabel()
        self.remaining = QLabel()
        for label in (self.speed, self.crawled, self.remaining):
            label.setProperty("text_style", "meta")
            row.addWidget(label)
        row.addStretch(1)
        self.status_note = QLabel()
        self.status_note.setProperty("text_style", "meta")
        self.status_note.setWordWrap(True)
        row.addWidget(self.status_note)
        self._counters(None, None, None)
        return bar

    # ---- state ----------------------------------------------------------------------------------------------
    def _set_state(self, state, note=""):
        self.state = state
        kind, text = STATES[state]
        self.state_badge.setProperty("badge", kind)
        self.state_badge.setText(tr(text))
        self.state_badge.style().unpolish(self.state_badge)
        self.state_badge.style().polish(self.state_badge)
        self.status_note.setText(note or tr("Без проекта · данные краула во временной папке"))
        running = state in ("running", "stopping")
        self.start.setText(tr("Стоп" if running else "Старт"))
        self.start.setIcon(material_icon("stop" if running else "play_arrow"))
        for widget in (self.address, self.limit, self.clear, *self.mode_buttons.values()):
            widget.setEnabled(not running)
        self.start.setEnabled(state != "stopping")
        self._sync_start()
        self._note.setVisible(self.scan_path is not None)
        self.export.setEnabled(self.scan_path is not None and bool(self.total))
        self.search.setEnabled(self.scan_path is not None)

    def _sync_start(self):
        """Start is unavailable (with the reason) while the core executable is unknown."""
        if self.state == "stopping":
            return
        if self.state in ("running",):
            self.start.setToolTip(tr("Остановить краул"))
            return
        found = bool(getattr(self.host, "core_executable", None))
        self.start.setEnabled(found)
        self.start.setToolTip("" if found else tr("Локальное ядро seohead не найдено"))

    def _counters(self, speed, fetched, remaining):
        dash = "—"
        self.speed.setText(
            trf("Средняя скорость: {value}", value=f"{speed} {tr('стр./с')}" if speed else dash)
        )
        self.crawled.setText(
            trf("Пройдено: {value}", value=number(fetched) if fetched is not None else dash)
        )
        self.remaining.setText(
            trf("Осталось: {value}", value=number(remaining) if remaining is not None else dash)
        )

    def _set_mode(self, mode):
        self.mode = mode
        self.address.setPlaceholderText(tr(PLACEHOLDERS[mode]))
        self.limit.setEnabled(mode != "list" and self.state not in ("running", "stopping"))

    # ---- crawl ----------------------------------------------------------------------------------------------
    def _start_or_stop(self):
        if self.state == "running":
            return self._stop()
        if self.state == "stopping":
            return
        executable = getattr(self.host, "core_executable", None)
        if not executable:
            return self._set_state(self.state, tr("Локальное ядро seohead не найдено"))
        self._discard_results()
        workdir = tempfile.mkdtemp(prefix="spiderhead-crawl-")
        scan_out = str(Path(workdir) / "crawl.sqlite")
        try:
            arguments = crawl_arguments(
                self.mode, self.address.text(), self.limit.value(), scan_out
            )
        except ValueError as exc:
            shutil.rmtree(workdir, ignore_errors=True)
            return self._set_state("failed", tr(str(exc)))
        self.workdir, self.stderr, self.started_at = workdir, "", time.monotonic()
        process = QProcess(self)
        process.setStandardOutputFile(
            QProcess.nullDevice()
        )  # the summary is large; the scan file has it all
        process.readyReadStandardError.connect(self._read_progress)
        process.errorOccurred.connect(self._process_error)
        process.finished.connect(lambda code, status: self._crawl_finished(process, code, status))
        self.process = process
        self.scan_path = scan_out
        self._counters(None, 0, None)
        self._show_panel(
            "loading", "Краул идёт", "Первые строки появятся, как только ядро их запишет."
        )
        self._set_state("running")
        process.start(executable, arguments)
        self._refresh.start()

    def _stop(self):
        process = self.process
        if process is None or process.state() == QProcess.NotRunning:
            return
        self._set_state("stopping")
        pid = process.processId()
        if os.name != "nt" and pid:
            try:
                os.kill(pid, signal.SIGINT)  # own child only
                return
            except OSError:
                pass
        process.terminate()

    def _read_progress(self):
        process = self.process
        if process is None:
            return
        self.stderr = (
            self.stderr + bytes(process.readAllStandardError()).decode("utf-8", "replace")
        )[-8000:]
        found = PROGRESS.findall(self.stderr)
        if found:
            fetched, known, rate = found[-1]
            self.speed_value = rate or None
            self._counters(rate or None, int(fetched), max(int(known) - int(fetched), 0))

    def _process_error(self, error):
        if error == QProcess.FailedToStart:
            self._set_state("failed", tr("Не удалось запустить локальное ядро"))
            self._discard_results()

    def _crawl_finished(self, process, code, status):
        if process is not self.process:
            return
        self._read_progress()
        self._refresh.stop()
        stopped = self.state == "stopping"
        self.process = None
        process.deleteLater()
        if code != 0 and not stopped:
            reason = next(
                (
                    line.split("error:", 1)[1].strip()
                    for line in reversed(self.stderr.splitlines())
                    if "error:" in line
                ),
                tr("Ядро завершило краул с ошибкой"),
            )
            self._discard_results()
            self._show_panel("partial", "Краул не удался", reason)
            return self._set_state("failed", reason)
        if not Path(self.scan_path or "").is_file():
            self._discard_results()
            self._show_panel("empty", "Краул остановлен до первого адреса")
            return self._set_state("stopped")
        self._set_state("stopped" if stopped else "done")
        self._reload()
        self._read_facets()

    def _discard_results(self):
        self._refresh.stop()
        self.page_job.cancel()
        self.facet_job.cancel()
        self.export_job.cancel()
        if self.process is not None and self.process.state() != QProcess.NotRunning:
            self.process.kill()
            self.process.waitForFinished(500)
        self.process = None
        if self.workdir:
            shutil.rmtree(self.workdir, ignore_errors=True)
        self.workdir = self.scan_path = None
        self.total = None
        self.offset = 0
        self.model.set_rows([])
        self.counts = {}
        for facet, label in self.counters.items():
            label.setText("—")
        self._clear_details()
        self.foot_text.setText("")
        self.prev.setEnabled(False)
        self.next.setEnabled(False)

    def clear_results(self):
        if self.state in ("running", "stopping"):
            return
        self._discard_results()
        self._counters(None, None, None)
        self._show_panel(
            "empty",
            "Введите адрес и нажмите «Старт»",
            "Краул без проекта сохраняет результат во временную папку до закрытия окна.",
        )
        self._set_state("idle")

    def shutdown(self):
        try:
            self._discard_results()
        except RuntimeError:  # Qt objects are already deleted at exit
            pass

    # ---- reading --------------------------------------------------------------------------------------------
    def filters(self):
        found = list(self.base_filters)
        text = self.search.text().strip()
        if text:
            found.append({"column": "url", "op": "contains", "value": text})
        return found

    def _reload(self, keep_page=False):
        if not self.scan_path or not Path(self.scan_path).is_file():
            return
        if not keep_page:
            self.offset = 0
        self.revision += 1
        try:
            arguments = query_arguments(self.scan_path, filters=self.filters(), offset=self.offset)
        except ValueError as exc:
            return self._show_panel("partial", "Ошибка запроса", str(exc))
        self.page_job.start(arguments, self.revision)

    def _reload_running(self):
        if self.state == "running" and not self.page_job.busy:
            self._reload(keep_page=True)

    def _page_done(self, token, payload):
        if token != self.revision:
            return
        if payload.get("ok") is not True or payload.get("state") == "unavailable":
            if self.state == "running":  # the file is still being written: try on the next tick
                return
            return self._show_panel("partial", "Страницы недоступны", reason_text(payload))
        rows = payload.get("rows") or []
        self.total = (
            payload.get("filtered_total") if type(payload.get("filtered_total")) is int else None
        )
        self.model.project_host = None
        self.model.set_rows(rows)
        if rows:
            self.stack.setCurrentWidget(self.table)
        elif self.state == "running":
            self._show_panel(
                "loading", "Краул идёт", "Первые строки появятся, как только ядро их запишет."
            )
        else:
            self._show_panel(
                "empty",
                "Нет адресов по этому условию",
                "Смените вкладку, сбросьте поиск или повторите краул.",
            )
        first = self.offset + 1 if rows else 0
        self.foot_text.setText(
            trf(
                "{first}–{last} из {total}",
                first=number(first),
                last=number(self.offset + len(rows)),
                total=number(self.total) if self.total is not None else "—",
            )
        )
        self.prev.setEnabled(self.offset > 0)
        self.next.setEnabled(bool(payload.get("has_more")))
        self.export.setEnabled(bool(self.total))
        if self.state not in ("running", "stopping") and type(payload.get("total")) is int:
            self._counters(self.speed_value, payload["total"], 0)
        self._clear_details()

    def _page_failed(self, token, reason):
        if token == self.revision and self.state != "running":
            self._show_panel("partial", "Страницы недоступны", reason)

    def _go_page(self, step):
        self.offset = max(0, self.offset + step * PAGE)
        self._reload(keep_page=True)

    def _read_facets(self):
        if not self.scan_path:
            return
        self.facet_job.start(
            [
                "scan-url-query",
                "--scan",
                self.scan_path,
                "--limit",
                "1",
                "--columns",
                "url_id",
                "--facets",
                "all",
            ],
            self.scan_path,
        )

    def _facets_done(self, token, payload):
        if token != self.scan_path:
            return
        facets = payload.get("facets") or {}
        for facet, label in self.counters.items():
            value = facets.get(facet)
            label.setText(number(value) if type(value) is int else "—")

    # ---- selection ------------------------------------------------------------------------------------------
    def _tab_changed(self, index):
        if index >= 0:
            self._set_filter(index)
            self._reload()

    def _set_filter(self, index):
        label, filters = TAB_FILTERS[index]
        self.base_filters = list(filters)
        self.filter_label.setText(trf("Фильтр: {name}", name=tr(label)))

    def _select_overview(self, index):
        _icon, label, _depth, _facet, filters = OVERVIEW[index]
        self.base_filters = list(filters or [])
        self.filter_label.setText(trf("Фильтр: {name}", name=tr(label)))
        self.tabs.blockSignals(True)
        self.tabs.setCurrentIndex(0 if not filters else -1)
        self.tabs.blockSignals(False)
        self._reload()

    def _row_changed(self, current, _previous):
        row = (
            self.model.rows[current.row()]
            if current.isValid() and current.row() < len(self.model.rows)
            else None
        )
        if row is None:
            return self._clear_details()
        self._details.setText(trf("Выбрано: {url}", url=row.get("url") or ""))
        for key, label in self._fields.items():
            value = row.get(key)
            if key == "indexable":
                text = index_badge(row.get("status_code"), value)[1]
            else:
                text = self.model.cell_text(row, key)
            label.setText(text if text is not None else tr("Нет данных"))

    def _clear_details(self):
        self._details.setText(tr("Выберите адрес в таблице, чтобы увидеть сведения"))
        for label in self._fields.values():
            label.setText("")

    # ---- export ---------------------------------------------------------------------------------------------
    def _export(self):
        if not self.scan_path or not self.total:
            return
        path, chosen = QFileDialog.getSaveFileName(
            self, tr("Экспорт адресов"), "crawl.csv", "CSV (*.csv);;Excel (*.xlsx)"
        )
        if not path:
            return
        fmt = "xlsx" if path.lower().endswith(".xlsx") or "xlsx" in chosen else "csv"
        if not path.lower().endswith("." + fmt):
            path += "." + fmt
        if Path(
            path
        ).exists():  # the core writes only new files; the dialog already confirmed the overwrite
            try:
                Path(path).unlink()
            except OSError:
                return self._set_state(self.state, tr("Не удалось заменить существующий файл"))
        arguments = [
            "scan-url-query",
            "--scan",
            self.scan_path,
            "--export",
            path,
            "--export-format",
            fmt,
        ]
        filters = self.filters()
        if filters:
            import json

            arguments += ["--filters", json.dumps(filters, ensure_ascii=False)]
        self.export.setEnabled(False)
        self.export_job.start(arguments, path)

    def _export_done(self, token, payload):
        self.export.setEnabled(bool(self.total))
        if payload.get("ok") is not True:
            return self._set_state(self.state, reason_text(payload))
        self._set_state(self.state, trf("Экспорт записан: {path}", path=Path(token).name))

    def _export_failed(self, _token, reason):
        self.export.setEnabled(bool(self.total))
        self._set_state(self.state, reason)

    # ---- panels ---------------------------------------------------------------------------------------------
    def _show_panel(self, kind, title, text=""):
        layout = self.panel_host.layout()
        while layout.count():
            widget = layout.takeAt(0).widget()
            if widget is not None:
                widget.setParent(None)
                widget.deleteLater()
        layout.addWidget(StatePanel(kind, title, text))
        self.stack.setCurrentWidget(self.panel_host)
