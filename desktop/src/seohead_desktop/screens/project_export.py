"""«Экспорт и отчёты» (canvas ProjExport.dc.html): what the selected scan can be exported as.

The sheet's three columns are kept: dataset and format, columns, folder and run. The dataset list comes from the
selected scan; the page reads nothing itself. The core's ``scan_export.v1`` covers pages (URL table) and findings in
CSV, XLSX, JSON and XML. Not connected in this build, shown as the neutral waiting state instead of a sample: the
PDF report, compare and task exports, the column catalogue, the run button and the list of earlier exports.
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
    QPushButton,
    QSizePolicy,
    QStackedWidget,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from ..i18n import tr, trf
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import StatePanel, no_project_panel
from ..ui.presentation import short_run_id
from .base import Screen
from .scan_common import number

UNAVAILABLE = "Недоступно в этой версии ядра"
SHORT_UNAVAILABLE = "Недоступно"
# (id, icon, label, record type the core exports or None when the core has no such export)
DATASETS = (
    ("url", "table_view", "Таблица URL", "pages"),
    ("issues", "rule", "Проблемы", "findings"),
    ("compare", "compare_arrows", "Сравнение", None),
    ("tasks", "checklist", "Задачи", None),
)
FORMATS = (("csv", "CSV", "CSV: отдельный файл на каждый тип записи"),
           ("xlsx", "XLSX", "XLSX: книга Excel"),
           ("json", "JSON", "JSON: построчно, для скриптов и агентов"),
           ("xml", "XML", "XML: схема scan_export.v1"))
PDF_NOTE = "PDF-отчёт: недоступно в этой версии ядра"


def dataset_count(record, scan):
    """Row count shown under a dataset name: the URL count of the scan; findings and the rest have none in the core."""
    if record is None:
        return tr(SHORT_UNAVAILABLE)
    if record == "pages" and scan is not None and scan_urls(scan) is not None:
        return trf("{n} URL", n=number(scan_urls(scan)))
    return tr("Нет данных")


def selected_scan(host):
    """The scan the page exports: the one the window selected, never a guess."""
    return next((row for row in host.scan_model.rows if row.get("path") == host.selected_scan_path), None)


def scan_urls(scan):
    done = (((scan.get("evidence") or {}).get("frontier") or {}).get("counts") or {}).get("done")
    return done if type(done) is int else None


def card(title, icon, width=None):
    frame = QFrame()
    frame.setProperty("card", "panel")
    if width is not None:
        frame.setFixedWidth(width)
    layout = QVBoxLayout(frame)
    layout.setContentsMargins(16, 14, 16, 14)
    layout.setSpacing(10)
    heading = QHBoxLayout()
    heading.setSpacing(8)
    heading.addWidget(MaterialIconLabel(icon, 18, color="role:text_2"))
    text = QLabel(tr(title))
    text.setProperty("text_style", "section")
    heading.addWidget(text, 1)
    layout.addLayout(heading)
    return frame, layout


def meta(text=""):
    label = QLabel(text)
    label.setProperty("text_style", "meta")
    label.setWordWrap(True)
    return label


class ProjExportScreen(Screen):
    slot = "reports"
    watches = ("project", "scans", "scan_status")

    def __init__(self, host):
        super().__init__(host)
        self.dataset = "url"
        self.fmt = "xlsx"
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        bar = QFrame()
        bar.setProperty("page_bar", True)
        bar_layout = QHBoxLayout(bar)
        bar_layout.setContentsMargins(20, 8, 16, 8)
        title = QLabel(tr("Экспорт и отчёты"))
        title.setProperty("text_style", "title")
        bar_layout.addWidget(title)
        bar_layout.addSpacing(12)
        self.scan_meta = meta()
        bar_layout.addWidget(self.scan_meta)
        bar_layout.addStretch(1)
        root.addWidget(bar)
        self.stack = QStackedWidget()
        root.addWidget(self.stack, 1)
        self.stack.addWidget(self._content())
        self.no_scan = StatePanel("empty", "Скан не выбран", "Выберите скан в разделе «Сканы», чтобы выгрузить его данные.")
        self.stack.addWidget(self.no_scan)
        self.no_project = no_project_panel(host, tr("Экспорт данных скана доступен после открытия проекта."))
        self.stack.addWidget(self.no_project)
        self.refresh()

    def _content(self):
        page = QWidget()
        row = QHBoxLayout(page)
        row.setContentsMargins(20, 16, 20, 16)
        row.setSpacing(16)
        row.addWidget(self._left_column(), 0, Qt.AlignTop)
        row.addWidget(self._columns_card(), 1, Qt.AlignTop)
        row.addWidget(self._right_column(), 0, Qt.AlignTop)
        row.addStretch(0)
        return page

    def _left_column(self):
        frame, layout = card("Набор данных", "dataset", width=260)
        self.dataset_group = QButtonGroup(self)
        self.dataset_group.setExclusive(True)
        self.dataset_buttons = {}
        for key, icon, label, _record in DATASETS:
            button = QToolButton()
            button.setProperty("card", "choice")
            button.setCheckable(True)
            button.setIcon(material_icon(icon, "role:text_2"))
            button.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
            button.setFixedHeight(60)  # two lines inside the choice-card padding
            button.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
            button.setAccessibleName(tr(label))
            button.clicked.connect(lambda _checked=False, value=key: self.pick_dataset(value))
            self.dataset_group.addButton(button)
            self.dataset_buttons[key] = button
            layout.addWidget(button)
        layout.addWidget(meta(tr("Формат")))
        segment = QFrame()
        segment.setProperty("segmented", "true")
        segment_layout = QGridLayout(segment)  # 2 x 2: four labels fit the 260 px column without eliding
        segment_layout.setContentsMargins(3, 3, 3, 3)
        segment_layout.setSpacing(2)
        self.format_group = QButtonGroup(self)
        self.format_group.setExclusive(True)
        self.format_buttons = {}
        for index, (key, label, _hint) in enumerate(FORMATS):
            button = self._segment(key, label)
            self.format_buttons[key] = button
            segment_layout.addWidget(button, index // 2, index % 2)
        layout.addWidget(segment)
        self.format_hint = meta()
        layout.addWidget(self.format_hint)
        layout.addWidget(meta(tr(PDF_NOTE)))
        layout.addStretch(1)
        return frame

    def _segment(self, key, label):
        button = QToolButton()
        button.setProperty("segment", "true")
        button.setCheckable(True)
        button.setText(label)
        button.setAccessibleName(label)
        button.clicked.connect(lambda _checked=False, value=key: self.pick_format(value))
        self.format_group.addButton(button)
        return button

    def _columns_card(self):
        frame, layout = card("Колонки", "view_column")
        layout.addWidget(StatePanel("waiting", "Выбор колонок пока недоступен",
                                    "Ядро выгружает записи целиком. Список полей по типам записи в приложении не подключён.",
                                    hint=tr("Выбор полей записей ядра")))
        return frame

    def _right_column(self):
        column = QWidget()
        column.setFixedWidth(360)
        layout = QVBoxLayout(column)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(16)
        frame, box = card("Куда сохранить", "folder")
        row = QHBoxLayout()
        self.folder = QLineEdit()
        self.folder.setReadOnly(True)
        self.folder.setAccessibleName(tr("Папка экспорта"))
        row.addWidget(self.folder, 1)
        browse = QPushButton("…")
        browse.setEnabled(False)
        browse.setToolTip(tr(UNAVAILABLE))
        row.addWidget(browse)
        box.addLayout(row)
        self.file_name = meta()
        box.addWidget(self.file_name)
        self.run = QPushButton(tr("Экспортировать"))
        self.run.setProperty("size", "lg")
        self.run.setProperty("role", "primary")
        self.run.setIcon(material_icon("download"))
        self.run.setEnabled(False)
        self.run.setToolTip(tr("Запуск экспорта из приложения пока не подключён"))
        box.addWidget(self.run)
        layout.addWidget(frame)
        history, history_box = card("Недавние экспорты", "history")
        history_box.addWidget(StatePanel("waiting", "Журнал экспортов пока недоступен",
                                         "Ядро не ведёт журнал выгрузок. Список появится, когда он будет.",
                                         hint=tr("Журнал экспортов ядра")))
        layout.addWidget(history, 1)
        return column

    def pick_dataset(self, key):
        self.dataset = key
        self.refresh()

    def pick_format(self, key):
        self.fmt = key
        self.refresh()

    def refresh(self):
        if not self.project_open:
            self.scan_meta.setText("")
            self.stack.setCurrentWidget(self.no_project)
            return
        scan = selected_scan(self.host)
        if scan is None:
            self.scan_meta.setText("")
            self.stack.setCurrentWidget(self.no_scan)
            return
        self.stack.setCurrentIndex(0)
        urls = scan_urls(scan)
        self.scan_meta.setText(trf("скан {run} · {count}", run=short_run_id(scan.get("uuid")),
                                   count=trf("{n} URL", n=number(urls)) if urls is not None else tr("Нет данных")))
        for key, _icon, label, record in DATASETS:
            button = self.dataset_buttons[key]
            button.setEnabled(record is not None)
            button.setChecked(key == self.dataset)
            button.setText(f"{tr(label)}\n{dataset_count(record, scan)}")
            button.setToolTip("" if record is not None else tr(UNAVAILABLE))
        for key, _label, _hint in FORMATS:
            self.format_buttons[key].setChecked(key == self.fmt)
        self.format_hint.setText(tr(next(hint for key, _label, hint in FORMATS if key == self.fmt)))
        name = f"{self.dataset}_{short_run_id(scan.get('uuid'))}.{self.fmt}"
        self.folder.setText(f"{self.host.project_directory}/exports")
        self.file_name.setText(f"{tr('Файл')}: {name}")
