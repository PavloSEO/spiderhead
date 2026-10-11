"""«Картинки» (design v2 ToolImages): compress a folder of images, then build the 301 rules for the changed addresses.

Real data only. The file list is read locally from the folder the user picks (name, size, format). Compression runs
``seohead images-optimize`` in a background job on the user's click and writes only into the output folder; the 301
rules come from ``seohead redirects-generate``. Options the core does not declare are shown as unavailable and are never
sent. The task step waits for the core's tool-to-task link. Nothing is sent over the network by this window.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

from PyQt5.QtCore import QAbstractTableModel, QSortFilterProxyModel, Qt
from PyQt5.QtGui import QGuiApplication
from PyQt5.QtWidgets import (
    QAbstractItemView,
    QButtonGroup,
    QCheckBox,
    QFileDialog,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QPlainTextEdit,
    QPushButton,
    QSpinBox,
    QStackedWidget,
    QTableView,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from ..i18n import tr, trf
from ..integration_worker import run_background
from ..ui.controls import Segmented
from ..ui.icons import MaterialIconLabel
from ..ui.icons import material_icon as _icon
from ..ui.kit import StatePanel, waiting_badge

IMAGE_SUFFIXES = {".jpg", ".jpeg", ".png", ".webp", ".avif", ".tiff", ".gif"}
OUTPUT_FOLDER = "optimized"
MAX_FILES = 5000  # rows shown and read from one folder
MAX_BATCH = 1000  # files per compression run (one command line)
HEAVY_BYTES = 200 * 1024
STEPS = ("Найти", "Сжать", "Формат", "Редиректы", "Задачи")
STEP_HINTS = (
    "Выберите папку и отметьте файлы, которые нужно сжать",
    "Подберите качество и сожмите отмеченные файлы",
    "Выберите формат, ширину и папку для результата",
    "Выгрузите правила 301 для старых адресов",
    "Передайте работу программисту",
)
COLUMNS = ("", "Файл", "Размер", "Формат", "После", "−%", "Статус")
QUALITY_OPTIONS = ((70, "70"), (80, "80"), (90, "90"))
FORMAT_OPTIONS = (("webp", "WebP"), ("avif", "AVIF"), ("keep", "оставить"))
REDIRECT_FORMATS = (
    ("nginx", "nginx"),
    ("apache-rewrite-rule", "Apache RewriteRule"),
    ("apache-redirect", "Apache Redirect"),
)
# Options the core does not declare yet are shown disabled with the neutral tooltip, never sent.
UNAVAILABLE_TIP = "Недоступно в этой версии ядра"
TASK_HINT = "Инструменты создают задачу проекта с вложениями"
TASK_HINT_EXIF = "Ядро не объявляет сохранение EXIF: сейчас метаданные не переносятся"
ISSUE_TASKS = 998  # core issue for the tool-to-task link and the CSV/WordPress formats; code only, never shown


def kb(size):
    """Russian sizes: one decimal below 10 KB, whole kilobytes below 1000 KB, megabytes with one decimal above."""
    if size >= 1000 * 1024:
        return f"{size / (1024 * 1024):.1f}".replace(".", ",") + " МБ"
    if size < 10 * 1024:
        return f"{size / 1024:.1f}".replace(".", ",") + " КБ"
    return f"{round(size / 1024)} КБ"


def scan_folder(folder):
    """Image files under ``folder`` (not hidden, not the output folder), bounded by MAX_FILES. Local reads only."""
    rows, truncated = [], False
    root = Path(folder)
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(
            name for name in dirnames if not name.startswith(".") and name != OUTPUT_FOLDER
        )
        for name in sorted(filenames):
            path = Path(dirpath, name)
            suffix = path.suffix.lower()
            if suffix not in IMAGE_SUFFIXES:
                continue
            if len(rows) >= MAX_FILES:
                return rows, True
            try:
                size = path.stat().st_size
            except OSError:
                continue
            rows.append(
                {
                    "path": str(path),
                    "rel": path.relative_to(root).as_posix(),
                    "size": size,
                    "ext": suffix,
                    "selected": True,
                    "after": None,
                    "pct": None,
                    "status": "Не сжат",
                }
            )
    return rows, truncated


class FilesModel(QAbstractTableModel):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.rows = []

    def load(self, rows):
        self.beginResetModel()
        self.rows = rows
        self.endResetModel()

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(COLUMNS)

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if orientation == Qt.Horizontal and role == Qt.DisplayRole:
            return tr(COLUMNS[section]) if COLUMNS[section] else ""
        return None

    def data(self, index, role=Qt.DisplayRole):
        row, column = self.rows[index.row()], index.column()
        if column == 0 and role == Qt.CheckStateRole:
            return Qt.Checked if row["selected"] else Qt.Unchecked
        if role == Qt.TextAlignmentRole and column in (2, 4, 5):
            return int(Qt.AlignRight | Qt.AlignVCenter)
        if role == Qt.ToolTipRole and column == 1:
            return row["path"]
        if role != Qt.DisplayRole:
            return None
        if column == 1:
            return row["rel"]
        if column == 2:
            return kb(row["size"])
        if column == 3:
            return row["ext"][1:].upper()
        if column == 4:
            return "—" if row["after"] is None else kb(row["after"])
        if column == 5:
            return "—" if row["pct"] is None else f"−{row['pct']} %"
        if column == 6:
            return tr(row["status"])
        return None

    def flags(self, index):
        flags = Qt.ItemIsEnabled | Qt.ItemIsSelectable
        return flags | Qt.ItemIsUserCheckable if index.column() == 0 else flags

    def setData(self, index, value, role=Qt.EditRole):
        if index.column() == 0 and role == Qt.CheckStateRole:
            self.rows[index.row()]["selected"] = int(value) == int(Qt.Checked)
            self.dataChanged.emit(index, index, [Qt.CheckStateRole])
            return True
        return False


class FilesProxy(QSortFilterProxyModel):
    """Filters from the left pane; the source model keeps every row."""

    def __init__(self, source, parent=None):
        super().__init__(parent)
        self.setSourceModel(source)
        self.heavy = False
        self.not_webp = False

    def filterAcceptsRow(self, source_row, source_parent):
        row = self.sourceModel().rows[source_row]
        if self.heavy and row["size"] <= HEAVY_BYTES:
            return False
        return not (self.not_webp and row["ext"] == ".webp")


def optimize_command(core, files, out_dir, fmt, quality, max_width):
    return [
        core,
        "images-optimize",
        "--files",
        ",".join(files),
        "--output-dir",
        out_dir,
        "--format",
        fmt,
        "--quality",
        str(quality),
        "--max-width",
        str(max_width),
    ]


def parse_core_json(text):
    """The core prints a one-line header («seohead: <command>») before its JSON; return the dict or None."""
    start = text.find("\n{")
    payload = text[start + 1 :] if start >= 0 else text
    try:
        value = json.loads(payload)
    except ValueError:
        return None
    return value if isinstance(value, dict) else None


def run_optimize(command):
    """Background job: the core's result dict, or None when the core refused or printed nothing usable."""
    try:
        completed = subprocess.run(
            command, capture_output=True, text=True, timeout=1800, check=False
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return parse_core_json(completed.stdout)


def run_redirects(core, redirects, fmt):
    """Background job: the rules are passed on stdin (the core reads --input as raw JSON text), None on failure."""
    try:
        completed = subprocess.run(
            [core, "redirects-generate", "--format", fmt],
            input=json.dumps({"redirects": redirects}, ensure_ascii=False),
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    value = parse_core_json(completed.stdout)
    rules = value.get("rules") if value else None
    return rules if isinstance(rules, list) else None


class ImagesWindow(QWidget):
    def __init__(self, host):
        super().__init__(None, Qt.Window)
        self.host = host
        self.setWindowTitle(tr("Картинки"))
        self.resize(1280, 800)
        self.setMinimumSize(960, 600)
        self.folder = None
        self.out_dir = None
        self.step = 1
        self.compressed = False
        self.busy = False
        self.redirect_fmt = "nginx"
        self.model = FilesModel(self)
        self.proxy = FilesProxy(self.model, self)
        self._build()
        self._sync()

    # layout
    def _build(self):
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        root.addLayout(self._header())
        root.addLayout(self._steps())
        self.pages = QStackedWidget()
        self.pages.addWidget(self._work_page())
        self.pages.addWidget(self._redirects_page())
        self.pages.addWidget(self._task_page())
        root.addWidget(self.pages, 1)
        self.footer = QLabel()
        self.footer.setProperty("text_style", "meta")
        self.footer.setContentsMargins(20, 6, 20, 6)
        root.addWidget(self.footer)

    def _header(self):
        bar = QHBoxLayout()
        bar.setContentsMargins(20, 12, 20, 8)
        bar.setSpacing(10)
        bar.addWidget(MaterialIconLabel("compress", 24, color="role:primary"))
        title = QLabel(tr("Картинки"))
        title.setProperty("text_style", "title")
        bar.addWidget(title)
        self.meta = QLabel()
        self.meta.setProperty("text_style", "meta")
        bar.addWidget(self.meta)
        bar.addStretch(1)
        self.summary = QLabel()
        self.summary.setProperty("text_style", "meta")
        bar.addWidget(self.summary)
        self.primary = QPushButton()
        self.primary.setProperty("role", "primary")
        self.primary.clicked.connect(self._primary_clicked)
        bar.addWidget(self.primary)
        return bar

    def _steps(self):
        bar = QHBoxLayout()
        bar.setContentsMargins(20, 0, 20, 0)
        bar.setSpacing(4)
        self._step_group = QButtonGroup(self)
        self._step_group.setExclusive(True)
        self._step_buttons = []
        for number, name in enumerate(STEPS, start=1):
            button = QToolButton()
            button.setCheckable(True)
            button.setText(f"{number}  {tr(name)}")
            button.clicked.connect(lambda _checked, n=number: self.go_step(n))
            self._step_group.addButton(button)
            self._step_buttons.append(button)
            bar.addWidget(button)
        bar.addStretch(1)
        self.step_hint = QLabel()
        self.step_hint.setProperty("text_style", "meta")
        bar.addWidget(self.step_hint)
        return bar

    def _work_page(self):
        page = QWidget()
        row = QHBoxLayout(page)
        row.setContentsMargins(0, 0, 0, 0)
        row.setSpacing(0)
        row.addWidget(self._source_pane())
        center = QVBoxLayout()
        center.setContentsMargins(16, 10, 16, 12)
        self.center = QStackedWidget()
        self.table = QTableView()
        self.table.setModel(self.proxy)
        self.table.setSelectionBehavior(QAbstractItemView.SelectRows)
        self.table.setSelectionMode(QAbstractItemView.SingleSelection)
        self.table.verticalHeader().setVisible(False)
        self.table.setEditTriggers(QAbstractItemView.NoEditTriggers)
        self.table.horizontalHeader().setSectionResizeMode(1, QHeaderView.Stretch)
        self.table.setColumnWidth(0, 40)
        self.table.setColumnWidth(2, 110)
        self.table.setColumnWidth(3, 70)
        self.table.setColumnWidth(4, 110)
        self.table.setColumnWidth(5, 70)
        self.table.setColumnWidth(6, 120)
        self.table.clicked.connect(self._row_clicked)
        self.empty_folder = StatePanel(
            "empty",
            "Выберите папку с картинками",
            "Файлы читаются локально: имя, размер и формат. Ничего не сжимается без «Сжать».",
            action=("Выбрать папку…", self.choose_folder),
        )
        self.empty_images = StatePanel(
            "empty",
            "В папке нет картинок",
            "Поддерживаются JPEG, PNG, WebP, AVIF, TIFF и GIF.",
            action=("Выбрать другую папку…", self.choose_folder),
        )
        self.center.addWidget(self.empty_folder)
        self.center.addWidget(self.empty_images)
        self.center.addWidget(self.table)
        center.addWidget(self.center, 1)
        self.limit_note = QLabel()
        self.limit_note.setProperty("text_style", "meta")
        self.limit_note.setWordWrap(True)
        center.addWidget(self.limit_note)
        self.note = QLabel()
        self.note.setWordWrap(True)
        center.addWidget(self.note)
        row.addLayout(center, 1)
        row.addWidget(self._options_pane())
        return page

    def _source_pane(self):
        pane = QWidget()
        pane.setFixedWidth(260)
        layout = QVBoxLayout(pane)
        layout.setContentsMargins(14, 12, 14, 12)
        layout.setSpacing(10)
        source = QPushButton(tr("Из папки…"))
        source.setIcon(_icon("folder_open"))
        source.clicked.connect(self.choose_folder)
        layout.addWidget(source)
        self.source_label = QLabel(tr("Папка не выбрана"))
        self.source_label.setProperty("text_style", "meta")
        self.source_label.setWordWrap(True)
        layout.addWidget(self.source_label)
        layout.addWidget(self._section("Фильтры"))
        self.heavy = QCheckBox()
        self.heavy.toggled.connect(self._filter_changed)
        self.not_webp = QCheckBox()
        self.not_webp.toggled.connect(self._filter_changed)
        layout.addWidget(self.heavy)
        layout.addWidget(self.not_webp)
        self.filter_note = QLabel()
        self.filter_note.setProperty("text_style", "meta")
        self.filter_note.setWordWrap(True)
        layout.addWidget(self.filter_note)
        layout.addStretch(1)
        return pane

    def _options_pane(self):
        pane = QWidget()
        pane.setFixedWidth(330)
        layout = QVBoxLayout(pane)
        layout.setContentsMargins(14, 12, 14, 12)
        layout.setSpacing(10)
        layout.addWidget(self._section("Качество"))
        self.quality = Segmented(
            [(value, label) for value, label in QUALITY_OPTIONS] + [("auto", tr("авто по SSIM"))],
            80,
            tr("Качество"),
        )
        self.quality._buttons["auto"].setEnabled(False)
        self.quality._buttons["auto"].setToolTip(tr(UNAVAILABLE_TIP))
        layout.addWidget(self.quality)
        layout.addWidget(self._section("Формат"))
        self.fmt = Segmented(
            [(value, tr(label)) for value, label in FORMAT_OPTIONS], "webp", tr("Формат")
        )
        self.fmt.changed.connect(lambda _v: self._sync())
        layout.addWidget(self.fmt)
        self.quality.changed.connect(lambda _v: self._sync())
        width_row = QHBoxLayout()
        self.width = QSpinBox()
        self.width.setRange(1, 10000)
        self.width.setValue(1600)
        self.width.setSuffix(" px")
        self.width.valueChanged.connect(lambda _v: self._sync())
        width_row.addWidget(QLabel(tr("Максимум по ширине")))
        width_row.addWidget(self.width)
        layout.addLayout(width_row)
        exif_row = QHBoxLayout()
        exif_row.addWidget(QLabel(tr("Сохранять EXIF")))
        exif_row.addStretch(1)
        exif_row.addWidget(waiting_badge(ISSUE_TASKS, TASK_HINT_EXIF))
        layout.addLayout(exif_row)
        layout.addWidget(QLabel(tr("Куда сохранять")))
        out_row = QHBoxLayout()
        self.out_edit = QLineEdit()
        self.out_edit.setReadOnly(True)
        self.out_edit.setPlaceholderText(tr("Сначала выберите папку"))
        out_row.addWidget(self.out_edit, 1)
        browse = QToolButton()
        browse.setIcon(_icon("more_horiz"))
        browse.setToolTip(tr("Выбрать папку"))
        browse.clicked.connect(self.choose_out_dir)
        out_row.addWidget(browse)
        layout.addLayout(out_row)
        layout.addWidget(self._section("Команда"))
        self.command = QLabel()
        self.command.setTextInteractionFlags(Qt.TextSelectableByMouse)
        self.command.setWordWrap(True)
        self.command.setProperty("text_style", "meta")
        layout.addWidget(self.command)
        layout.addStretch(1)
        self.savings = QLabel()
        self.savings.setWordWrap(True)
        layout.addWidget(self.savings)
        return pane

    def _redirects_page(self):
        page = QWidget()
        layout = QVBoxLayout(page)
        layout.setContentsMargins(16, 12, 16, 12)
        layout.setSpacing(10)
        self.redirect_note = QLabel()
        self.redirect_note.setWordWrap(True)
        layout.addWidget(self.redirect_note)
        row = QHBoxLayout()
        self.redirect_formats = Segmented(
            [(value, label) for value, label in REDIRECT_FORMATS], "nginx", tr("Формат выгрузки")
        )
        self.redirect_formats.changed.connect(self._redirect_format_changed)
        row.addWidget(self.redirect_formats)
        for value, label in (("csv", "CSV"), ("wordpress-redirection", "WordPress Redirection")):
            button = QToolButton()
            button.setText(label)
            button.setEnabled(False)
            button.setToolTip(tr(UNAVAILABLE_TIP))
            row.addWidget(button)
        row.addStretch(1)
        layout.addLayout(row)
        self.rules = QPlainTextEdit()
        self.rules.setReadOnly(True)
        layout.addWidget(self.rules, 1)
        buttons = QHBoxLayout()
        self.copy_button = QPushButton(tr("Скопировать"))
        self.copy_button.setIcon(_icon("content_copy"))
        self.copy_button.clicked.connect(self.copy_rules)
        self.save_button = QPushButton(tr("Сохранить файл…"))
        self.save_button.setIcon(_icon("save"))
        self.save_button.clicked.connect(self.save_rules)
        buttons.addWidget(self.copy_button)
        buttons.addWidget(self.save_button)
        buttons.addStretch(1)
        layout.addLayout(buttons)
        return page

    def _task_page(self):
        page = QWidget()
        layout = QVBoxLayout(page)
        layout.setContentsMargins(20, 20, 20, 20)
        layout.addWidget(
            StatePanel(
                "waiting",
                "Задача из инструмента пока не создаётся",
                "Сжатые файлы уже лежат в выбранной папке. Вложить их в задачу «Работы» можно будет после появления этой связи в ядре.",
                issue=ISSUE_TASKS,
                hint=TASK_HINT,
            )
        )
        return page

    @staticmethod
    def _section(text):
        label = QLabel(tr(text))
        label.setProperty("text_style", "overline")
        return label

    # actions
    def choose_folder(self):
        chosen = QFileDialog.getExistingDirectory(self, tr("Папка с картинками"), self.folder or "")
        if not chosen:
            return
        rows, truncated = scan_folder(chosen)
        self.folder = chosen
        self.out_dir = str(Path(chosen, OUTPUT_FOLDER))
        self.compressed = False
        self.note.setText("")
        self.model.load(rows)
        self.limit_note.setText(
            trf("Показаны первые {n} файлов: дальше папка не читается.", n=MAX_FILES)
            if truncated
            else ""
        )
        self.center.setCurrentWidget(self.table if rows else self.empty_images)
        self._sync()

    def choose_out_dir(self):
        chosen = QFileDialog.getExistingDirectory(
            self, tr("Папка для результата"), self.out_dir or self.folder or ""
        )
        if chosen:
            self.out_dir = chosen
            self._sync()

    def go_step(self, number):
        self.step = number
        self._sync()
        if number == 4:
            self._load_rules()

    def _primary_clicked(self):
        if self.step == 2 and not self.compressed:
            return self.compress()
        if self.step < 5:
            self.go_step(self.step + 1)

    def _row_clicked(self, index):
        if index.column() == 0:
            source = self.proxy.mapToSource(index)
            self.model.setData(
                source.sibling(source.row(), 0),
                Qt.Unchecked if self.model.rows[source.row()]["selected"] else Qt.Checked,
                Qt.CheckStateRole,
            )
            self._sync()

    def _filter_changed(self, _checked=False):
        self.proxy.heavy = self.heavy.isChecked()
        self.proxy.not_webp = self.not_webp.isChecked()
        self.proxy.invalidateFilter()
        self._sync()

    def selected_files(self):
        return [row for row in self.model.rows if row["selected"]]

    def compress(self):
        if self.busy or not self.folder:
            return
        if not self.host.core_executable:
            self.note.setText(tr("Ядро seohead не найдено"))
            return
        chosen = self.selected_files()
        safe = [row["path"] for row in chosen if "," not in row["path"]]
        if not safe:
            self.note.setText(tr("Отметьте файлы для сжатия"))
            return
        if len(safe) > MAX_BATCH:
            self.note.setText(
                trf("За один запуск — до {n} файлов: снимите часть отметок", n=MAX_BATCH)
            )
            return
        command = optimize_command(
            self.host.core_executable,
            safe,
            self.out_dir,
            self.format_value(),
            self.quality.value(),
            self.width.value(),
        )
        self.busy = True
        self.primary.setEnabled(False)
        self.note.setText(trf("Сжимаю {n} файлов…", n=len(safe)))
        run_background(self.host.pool, lambda: run_optimize(command), self._compressed, self)

    def _compressed(self, value):
        self.busy = False
        if not isinstance(value, dict) or not isinstance(value.get("results"), list):
            self.note.setText(tr("Ядро не вернуло результат"))
            self._sync()
            return
        by_file = {
            record.get("file"): record for record in value["results"] if isinstance(record, dict)
        }
        for row in self.model.rows:
            record = by_file.get(row["path"])
            if record is None:
                continue
            if record.get("ok"):
                row["after"] = int(record.get("after_bytes") or 0)
                row["pct"] = round(float(record.get("saved_pct") or 0.0))
                row["status"] = "Сжат"
                row["out"] = record.get("out")
            else:
                row["status"] = "Ошибка"
        self.model.dataChanged.emit(
            self.model.index(0, 0), self.model.index(len(self.model.rows) - 1, len(COLUMNS) - 1)
        )
        self.compressed = True
        self._sync()

    def format_value(self):
        return self.fmt.value()

    def redirect_pairs(self):
        """Old and new addresses for every converted file whose extension changed (relative to the chosen folder)."""
        pairs = []
        for row in self.model.rows:
            out = row.get("out")
            if not out or row["status"] != "Сжат" or not self.out_dir:
                continue
            new_rel = (
                Path(out).relative_to(self.out_dir).as_posix()
                if Path(out).is_relative_to(self.out_dir)
                else Path(out).name
            )
            if Path(new_rel).suffix.lower() == row["ext"]:
                continue
            pairs.append({"old_url": "/" + row["rel"], "new_url": "/" + new_rel})
        return pairs

    def _redirect_format_changed(self, value):
        self.redirect_fmt = value
        self._load_rules()

    def _load_rules(self):
        """Runs the core once per entry to step 4 or per format change; a stale answer is dropped by its token."""
        pairs = self.redirect_pairs()
        if not pairs:
            self.rules.setPlainText(
                tr("Сначала сожмите картинки: правила строятся по сжатым файлам")
                if not self.compressed
                else ""
            )
            return
        if not self.host.core_executable:
            self.rules.setPlainText(tr("Ядро seohead не найдено"))
            return
        self._rules_token = getattr(self, "_rules_token", 0) + 1
        token = self._rules_token
        run_background(
            self.host.pool,
            lambda: run_redirects(self.host.core_executable, pairs, self.redirect_fmt),
            lambda rules: self._rules_ready(token, rules),
            self,
        )

    def _rules_ready(self, token, rules):
        if token != getattr(self, "_rules_token", 0):
            return
        if rules is None:
            self.rules.setPlainText(tr("Ядро не вернуло правила"))
            return
        self.rules.setPlainText("\n".join(str(line) for line in rules))

    def copy_rules(self):
        QGuiApplication.clipboard().setText(self.rules.toPlainText())

    def save_rules(self):
        name = "redirects-images.conf"
        target, _ = QFileDialog.getSaveFileName(self, tr("Сохранить правила"), name)
        if target:
            Path(target).write_text(self.rules.toPlainText() + "\n", encoding="utf-8")

    # state
    def _sync(self):
        rows = self.model.rows
        total = sum(row["size"] for row in rows)
        selected = self.selected_files()
        self.meta.setText(trf("Папка: {name}", name=Path(self.folder).name) if self.folder else "")
        self.source_label.setText(self.folder or tr("Папка не выбрана"))
        self.summary.setText(
            trf("{n} файлов · {size}", n=len(rows), size=kb(total)) if rows else ""
        )
        self.heavy.setText(
            trf("Тяжелее 200 КБ · {n}", n=sum(1 for r in rows if r["size"] > HEAVY_BYTES))
        )
        self.not_webp.setText(trf("Не WebP · {n}", n=sum(1 for r in rows if r["ext"] != ".webp")))
        shown = self.proxy.rowCount()
        shown_bytes = sum(
            self.model.rows[self.proxy.mapToSource(self.proxy.index(i, 0)).row()]["size"]
            for i in range(shown)
        )
        self.filter_note.setText(
            trf(
                "Под фильтры попадает {shown} из {total} файлов · {size}",
                shown=shown,
                total=len(rows),
                size=kb(shown_bytes),
            )
            if rows
            else ""
        )
        pairs = self.redirect_pairs()
        self.redirect_note.setText(
            trf(
                "Меняется расширение у {n} файлов. Старые адреса нужно перенаправить 301, иначе картинки выпадут из поиска по картинкам. Адреса — пути от выбранной папки: подставьте домен сайта.",
                n=len(pairs),
            )
            if pairs
            else tr(
                "Сначала сожмите картинки: правила строятся по сжатым файлам с новым расширением"
            )
        )
        self.out_edit.setText(self.out_dir or "")
        self.command.setText(self._command_text(len(selected)))
        self.step_hint.setText(tr(STEP_HINTS[self.step - 1]))
        for number, button in enumerate(self._step_buttons, start=1):
            button.setChecked(number == self.step)
        self.pages.setCurrentIndex({1: 0, 2: 0, 3: 0, 4: 1, 5: 2}[self.step])
        self._update_primary(len(selected))
        self._update_savings()
        self.footer.setText(
            trf(
                "Шаг {n} из 5 · {name} · локально, без сети",
                n=self.step,
                name=tr(STEPS[self.step - 1]),
            )
        )

    def _command_text(self, count):
        out = self.out_dir or "<папка>"
        return (
            f"seohead images-optimize --files <{trf('{n} файлов', n=count)}> --output-dir {out} "
            f"--format {self.format_value()} --quality {self.quality.value()} --max-width {self.width.value()}"
        )

    def _update_primary(self, selected_count):
        if self.step == 2 and not self.compressed:
            self.primary.setText(trf("Сжать {n} файлов", n=selected_count))
            self.primary.setIcon(_icon("compress"))
            self.primary.setEnabled(bool(self.folder) and selected_count > 0 and not self.busy)
        elif self.step == 5:
            self.primary.setText(tr("Создать задачу"))
            self.primary.setIcon(_icon("add_task"))
            self.primary.setEnabled(False)
            self.primary.setToolTip(tr(UNAVAILABLE_TIP))
        else:
            self.primary.setText(tr("Далее"))
            self.primary.setIcon(_icon("arrow_forward"))
            self.primary.setEnabled(True)

    def _update_savings(self):
        done = [row for row in self.model.rows if row["after"] is not None]
        if not done:
            self.savings.setText(
                tr("Экономия появится после сжатия: размер считает ядро по каждому файлу.")
            )
            return
        before = sum(row["size"] for row in done)
        after = sum(row["after"] for row in done)
        saved = round((before - after) / before * 100) if before else 0
        self.savings.setText(
            trf(
                "{n} файлов · {was} → {now} · −{pct} %",
                n=len(done),
                was=kb(before),
                now=kb(after),
                pct=saved,
            )
        )


def open_images(host):
    """Entry for the tool: opens the window, starts nothing; compression runs only on the user's «Сжать»."""
    previous = getattr(host, "_images_tool", None)
    if previous is not None:
        try:
            previous.close()
        except RuntimeError:
            pass
    window = ImagesWindow(host)
    window.setAttribute(Qt.WA_DeleteOnClose)
    window.show()
    host._images_tool = window
    return window
