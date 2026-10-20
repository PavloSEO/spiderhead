"""«Экспорт и отчёты»: export of the selected scan through the core ``scan-export`` command.

The dataset list comes from the selected scan; pages (URL table) and findings go out as CSV, XLSX, JSON or XML into the
chosen folder (default: the project's ``exports``). Compare and task exports, the PDF report, the column catalogue and
the history of earlier exports do not exist in the core, so they are not offered.
"""

from __future__ import annotations

from pathlib import Path

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import (
    QButtonGroup,
    QFileDialog,
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
from ..ui import motion
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import StatePanel, no_project_panel
from ..ui.presentation import short_run_id
from .base import Screen
from .scan_common import number
from .url_query import CoreJob, reason_text

UNAVAILABLE = "Недоступно в этой версии ядра"
SHORT_UNAVAILABLE = "Недоступно"
# (id, icon, label, record type the core exports or None when the core has no such export)
DATASETS = (
    ("url", "table_view", "Таблица URL", "pages"),
    ("issues", "rule", "Проблемы", "findings"),
)
FORMATS = (
    ("csv", "CSV", "CSV: отдельный файл на каждый тип записи"),
    ("xlsx", "XLSX", "XLSX: книга Excel"),
    ("json", "JSON", "JSON: одна запись в строке, удобно для скриптов"),
    ("xml", "XML", "XML: структурированный файл с описанием полей"),
)


def dataset_count(record, scan):
    """Row count shown under a dataset name: the URL count of the scan; findings and the rest have none in the core."""
    if record is None:
        return tr(SHORT_UNAVAILABLE)
    if record == "pages" and scan is not None and scan_urls(scan) is not None:
        return trf("{n} URL", n=number(scan_urls(scan)))
    return tr("Ядро не сообщает число строк до экспорта")


def selected_scan(host):
    """The scan the page exports: the one the window selected, never a guess."""
    if host.selected_scan_path is None:
        return None
    return next(
        (row for row in host.scan_model.rows if row.get("path") == host.selected_scan_path), None
    )


def scan_urls(scan):
    done = (((scan.get("evidence") or {}).get("frontier") or {}).get("counts") or {}).get("done")
    return done if type(done) is int else None


def output_name(scan, dataset, fmt):
    """Name of the file ``scan-export`` gets as ``--out``; CSV goes to ``<stem>.<record>.csv`` plus a manifest."""
    return f"{dataset}_{short_run_id(scan.get('uuid'))}.{fmt}"


def shown_name(scan, dataset, fmt):
    """The file name the user sees: for CSV the core writes one file per record type next to a manifest."""
    if fmt != "csv":
        return output_name(scan, dataset, fmt)
    record = dict((key, rec) for key, _i, _l, rec in DATASETS).get(dataset)
    return output_name(scan, dataset, fmt).replace(".csv", f".{record}.csv")


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


class _PathField(QLineEdit):
    """Read-only path elided in the middle: the end of a deep path matters more than its head (#1385)."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.full_path = ""

    def set_path(self, path):
        self.full_path = path
        self._elide()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._elide()

    def _elide(self):
        width = self.contentsRect().width() - 10
        self.setText(self.fontMetrics().elidedText(self.full_path, Qt.ElideMiddle, max(width, 20)))
        self.setCursorPosition(0)
        self.setToolTip(self.full_path)


class ProjExportScreen(Screen):
    slot = "reports"
    watches = ("project", "scans", "scan_status")

    def __init__(self, host):
        super().__init__(host)
        self.dataset = "url"
        self.fmt = "xlsx"
        self.folder_choice = None
        self.folder_project = None
        self.job = CoreJob(host, self)
        self.job.done.connect(self._exported)
        self.job.failed.connect(self._export_failed)
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
        self.no_scan = StatePanel(
            "empty",
            "Скан не выбран",
            "Выберите скан в разделе «Сканы», чтобы выгрузить его данные.",
        )
        self.stack.addWidget(self.no_scan)
        self.no_project = no_project_panel(
            host, tr("Экспорт данных скана доступен после открытия проекта.")
        )
        self.stack.addWidget(self.no_project)
        self.refresh()

    def _content(self):
        page = QWidget()
        row = QHBoxLayout(page)
        row.setContentsMargins(20, 16, 20, 16)
        row.setSpacing(16)
        row.addWidget(self._left_column(), 0, Qt.AlignTop)
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
        segment_layout = QGridLayout(
            segment
        )  # 2 x 2: four labels fit the 260 px column without eliding
        segment_layout.setContentsMargins(3, 3, 3, 3)
        segment_layout.setSpacing(2)
        self.format_group = QButtonGroup(self)
        self.format_group.setExclusive(True)
        self.format_buttons = {}
        for index, (key, label, hint) in enumerate(FORMATS):
            button = self._segment(key, label, hint)
            self.format_buttons[key] = button
            segment_layout.addWidget(button, index // 2, index % 2)
        layout.addWidget(segment)
        self.format_hint = meta()
        layout.addWidget(self.format_hint)
        layout.addStretch(1)
        return frame

    def _segment(self, key, label, hint):
        button = QToolButton()
        button.setProperty("segment", "true")
        button.setCheckable(True)
        button.setText(label)
        button.setAccessibleName(label)
        button.setToolTip(tr(hint))
        button.clicked.connect(lambda _checked=False, value=key: self.pick_format(value))
        self.format_group.addButton(button)
        return button

    def _right_column(self):
        column = QWidget()
        column.setFixedWidth(360)
        layout = QVBoxLayout(column)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(16)
        frame, box = card("Куда сохранить", "folder")
        row = QHBoxLayout()
        self.folder = _PathField()
        self.folder.setReadOnly(True)
        self.folder.setAccessibleName(tr("Папка экспорта"))
        row.addWidget(self.folder, 1)
        browse = QPushButton()
        browse.setIcon(material_icon("folder_open", "role:text_2"))
        browse.setAccessibleName(tr("Выбрать папку"))
        browse.setToolTip(tr("Выбрать папку"))
        browse.clicked.connect(self.browse)
        row.addWidget(browse)
        box.addLayout(row)
        self.file_name = meta()
        box.addWidget(self.file_name)
        self.run = QPushButton(tr("Экспортировать"))
        self.run.setProperty("size", "lg")
        self.run.setProperty("role", "primary")
        self.run.setIcon(material_icon("download", "role:on_primary"))
        self.run.clicked.connect(self.export)
        box.addWidget(self.run)
        self.cancel_button = QPushButton(tr("Отмена"))
        self.cancel_button.setVisible(False)
        self.cancel_button.clicked.connect(self.cancel_export)
        self.job.busyChanged.connect(self.cancel_button.setVisible)
        box.addWidget(self.cancel_button)
        self.result = meta()
        box.addWidget(motion.status_row(self.result, self.job))
        layout.addWidget(frame)
        layout.addStretch(1)
        return column

    def pick_dataset(self, key):
        self.dataset = key
        self.refresh()

    def pick_format(self, key):
        self.fmt = key
        self.refresh()

    def refresh(self):
        if self.folder_project != self.host.project_directory:
            self.folder_project = self.host.project_directory
            self.folder_choice = None  # a folder picked for one project is not offered for another
        if not self.job.busy:
            self.result.setText("")  # an old outcome describes the previous dataset or format
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
        self.scan_meta.setText(
            trf(
                "Скан {run} · {count}",
                run=short_run_id(scan.get("uuid")),
                count=trf("{n} URL", n=number(urls)) if urls is not None else tr("Нет данных"),
            )
        )
        for key, _icon, label, record in DATASETS:
            button = self.dataset_buttons[key]
            button.setEnabled(record is not None)
            button.setChecked(key == self.dataset)
            button.setText(f"{tr(label)}\n{dataset_count(record, scan)}")
            button.setToolTip("" if record is not None else tr(UNAVAILABLE))
        for key, _label, _hint in FORMATS:
            self.format_buttons[key].setChecked(key == self.fmt)
        self.format_hint.setText(tr(next(hint for key, _label, hint in FORMATS if key == self.fmt)))
        self.folder.set_path(self.folder_choice or f"{self.host.project_directory}/exports")
        self.file_name.setText(f"{tr('Файл')}: {shown_name(scan, self.dataset, self.fmt)}")

    def browse(self):
        chosen = QFileDialog.getExistingDirectory(
            self,
            tr("Папка экспорта"),
            self.folder.text() if Path(self.folder.text()).is_dir() else "",
        )
        if chosen:
            self.folder_choice = chosen
            self.refresh()

    def export(self):
        """Run ``scan-export`` for the chosen dataset and format; the core writes the files and reports them."""
        scan = selected_scan(self.host)
        record = dict((key, rec) for key, _i, _l, rec in DATASETS).get(self.dataset)
        if scan is None or record is None or not scan.get("path"):
            return
        folder = Path(self.folder.full_path or self.folder.text())  # the field shows a middle-elided path
        try:
            folder.mkdir(parents=True, exist_ok=True)
        except OSError:
            self.result.setText(tr("Не удалось создать папку экспорта"))
            return
        out = folder / output_name(scan, self.dataset, self.fmt)
        if any(folder.glob(out.stem + ".*")):
            self.result.setText(tr("Файлы с таким именем уже есть: выберите другую папку"))
            return
        self.run.setEnabled(False)
        self.result.setText(tr("Экспортирую…"))
        self.job.start(
            [
                "scan-export",
                "--scan",
                scan["path"],
                "--out",
                str(out),
                "--format",
                self.fmt,
                "--records",
                record,
            ],
            out,
        )

    def cancel_export(self):
        self.job.cancel()
        self.run.setEnabled(True)
        self.result.setText(tr("Экспорт отменён"))

    def _exported(self, _token, payload):
        self.run.setEnabled(True)
        if payload.get("ok") is not True:
            self.result.setText(reason_text(payload))
            return
        files = payload.get("files") or []
        self.result.setText(
            trf("Записано файлов: {n}", n=len(files)) if files else tr("Экспорт не создал файлов")
        )
        self.result.setToolTip("\n".join(str(item) for item in files))

    def _export_failed(self, _token, reason):
        self.run.setEnabled(True)
        self.result.setText(reason)
