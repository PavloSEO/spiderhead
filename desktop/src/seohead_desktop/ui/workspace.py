"""Native workspace controls over the primary window's existing models."""

import plistlib
import sys
from pathlib import Path
from xml.parsers.expat import ExpatError

from PyQt5.QtCore import QEvent, Qt, QTimer
from PyQt5.QtWidgets import (
    QApplication,
    QDialog,
    QDialogButtonBox,
    QDockWidget,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QListWidget,
    QListWidgetItem,
    QPlainTextEdit,
    QPushButton,
    QTableView,
    QTabWidget,
    QVBoxLayout,
    QWidget,
)

from .presentation import ElidedLabel, WorkspaceSplitter

LAYOUTS = {
    "table": "Таблица без панелей",
    "url": "URL и детали",
    "compare": "Сравнение сканов",
    "monitor": "Монитор в двух окнах",
}
LAYOUT_SCHEMA = 2
PANEL_IDS = {"navigation": "Навигация", "overview": "Сводка", "inspector": "Инспектор URL"}
# "audit" was the id of the page now called «Проблемы»; saved tabs, prefs and agent calls may still carry it.
VIEW_ALIASES = {"audit": "issues"}
VIEW_IDS = ("work", "url", "issues", "project", "tasks", "scans", "inbox", "reports", "journal", "compare", "content_search", "crawler", "methods", "graph")


def system_reduced_motion(path=None):
    """Read only the actual macOS preference; a denied/corrupt read disables motion."""
    if path is None:
        if sys.platform != "darwin":
            return False
        path = Path.home() / "Library/Preferences/com.apple.universalaccess.plist"
    try:
        with Path(path).open("rb") as stream:
            data = stream.read(262145)
        if len(data) > 262144:
            return True
        document = plistlib.loads(data)
        return bool(document.get("reduceMotion", False))
    except FileNotFoundError:
        return False
    except (OSError, ValueError, TypeError, AttributeError, ExpatError):
        return True


def keep_on_screen(widget, screen=None):
    """Recover a window stored on a monitor that is no longer connected."""
    if not widget.isWindow() or widget.isFullScreen() or widget.isMaximized():
        return
    screens = QApplication.screens()
    target = next((candidate for candidate in screens if candidate.availableGeometry().contains(widget.frameGeometry().center())), None) or screen or QApplication.primaryScreen()
    if target:
        available = target.availableGeometry()
        frame = widget.frameGeometry()
        extra_width = max(0, frame.width() - widget.width())
        extra_height = max(0, frame.height() - widget.height())
        widget.resize(min(widget.width(), max(1, available.width() - extra_width)), min(widget.height(), max(1, available.height() - extra_height)))
        frame = widget.frameGeometry()
        widget.move(max(available.left(), min(frame.x(), available.right() - frame.width() + 1)), max(available.top(), min(frame.y(), available.bottom() - frame.height() + 1)))



class ProjectMonitor(QDockWidget):
    """One extra native window; no new gateway, worker, polling, or controller."""

    def __init__(self, owner, configure_table):
        super().__init__("Монитор проекта · SEOHEAD", owner)
        self.owner = owner
        self.setObjectName("projectMonitorDock")
        self.setAllowedAreas(Qt.LeftDockWidgetArea | Qt.RightDockWidgetArea | Qt.BottomDockWidgetArea)
        self.setFeatures(QDockWidget.DockWidgetClosable | QDockWidget.DockWidgetMovable | QDockWidget.DockWidgetFloatable)
        self.setAttribute(Qt.WA_DeleteOnClose, False)
        content = QWidget()
        layout = QVBoxLayout(content)
        layout.setContentsMargins(12, 12, 12, 12)
        layout.setSpacing(8)
        self.context = ElidedLabel("Откройте проект в основном окне")
        self.context.setObjectName("sectionCaption")
        layout.addWidget(self.context)
        splitter = WorkspaceSplitter(Qt.Vertical)
        self.progress = QPlainTextEdit()
        self.progress.setReadOnly(True)
        self.progress.setPlainText(owner.progress_text.toPlainText())
        owner.progress_text.textChanged.connect(lambda: self.progress.setPlainText(owner.progress_text.toPlainText()))
        self.progress.setAccessibleName("Тот же согласованный план проекта")
        splitter.addWidget(self.progress)
        self.table = QTableView()
        self.table.setModel(owner.activity_model)
        self.table.setSelectionModel(owner.activity_table.selectionModel())
        self.table.clicked.connect(lambda index: owner.show_observed_run(index, None))
        self.table.setAccessibleName("Те же запуски проекта в дополнительном окне")
        configure_table(self.table)
        for column in (1, 3, 5, 6):
            self.table.setColumnHidden(column, True)
        for column, width in ((0, 180), (2, 140), (4, 75), (7, 110), (8, 90)):
            self.table.setColumnWidth(column, width)
        splitter.addWidget(self.table)
        self.detail = QPlainTextEdit()
        self.detail.setReadOnly(True)
        self.detail.setPlainText(owner.activity_text.toPlainText())
        owner.activity_text.textChanged.connect(lambda: self.detail.setPlainText(owner.activity_text.toPlainText()))
        self.detail.setAccessibleName("Те же измерения выбранного запуска")
        splitter.addWidget(self.detail)
        splitter.setSizes([180, 260, 240])
        from .work_monitor import WorkMonitor
        self.views = QTabWidget()
        self.work_monitor = WorkMonitor()
        self.work_monitor.set_reduced_motion(owner.reduced_motion)
        self.work_monitor.openProjectRequested.connect(owner.choose_project)
        self.work_monitor.set_project_available(bool(owner.project_directory))
        self.work_monitor.runSelected.connect(owner.select_observed_identity)
        self.work_monitor.showResult.connect(owner.open_observed_result)
        self.views.addTab(self.work_monitor, "Монитор")
        self.views.addTab(splitter, "Таблица и детали")
        self.work_monitor.allRunsRequested.connect(lambda: self.views.setCurrentIndex(1))
        layout.addWidget(self.views, 1)
        footer = QHBoxLayout()
        label = QLabel("Наблюдение за текущим проектом основного окна")
        label.setObjectName("metadata")
        label.setWordWrap(True)
        footer.addWidget(label, 1)
        main = QPushButton("К основному окну")
        main.setProperty("role", "quiet")
        main.clicked.connect(self.focus_primary)
        footer.addWidget(main)
        layout.addLayout(footer)
        self.setWidget(content)
        self.resize(540, 720)

    def focus_primary(self):
        if self.owner.isMinimized():
            self.owner.showNormal()
        self.owner.raise_()
        self.owner.activateWindow()

    def sync_context(self):
        project = self.owner.project_picker.currentText()
        self.context.setText(project)
        self.setWindowTitle(f"Монитор · {project} · SEOHEAD")


class ActionFinder(QDialog):
    """Search an explicit UI action registry; text is never a command to execute."""

    def __init__(self, actions, parent=None):
        super().__init__(parent)
        self.actions = tuple(actions)
        self.selected_callback = None
        self.setWindowTitle("Действия SEOHEAD")
        self.resize(640, 480)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(16, 16, 16, 16)
        self.search = QLineEdit()
        self.search.setObjectName("actionSearch")
        self.search.setPlaceholderText("Найти действие, вид или настройку…")
        self.search.setAccessibleName("Поиск доступных действий")
        self.search.setClearButtonEnabled(True)
        self.search.installEventFilter(self)
        layout.addWidget(self.search)
        caption = QLabel("Выбор действия открывает нужный вид или план. Запуск скана требует подтверждения в плане.")
        caption.setWordWrap(True)
        caption.setObjectName("metadata")
        layout.addWidget(caption)
        self.results = QListWidget()
        self.results.setObjectName("actionResults")
        self.results.setAccessibleName("Найденные действия")
        layout.addWidget(self.results, 1)
        self.reason = QLabel()
        self.reason.setWordWrap(True)
        self.reason.setObjectName("metadata")
        layout.addWidget(self.reason)
        buttons = QDialogButtonBox(QDialogButtonBox.Cancel | QDialogButtonBox.Open)
        buttons.button(QDialogButtonBox.Cancel).setText("Отмена")
        self.open_button = buttons.button(QDialogButtonBox.Open)
        self.open_button.setText("Открыть")
        self.open_button.setProperty("role", "primary")
        buttons.accepted.connect(self.activate_current)
        buttons.rejected.connect(self.reject)
        layout.addWidget(buttons)
        self.search.textChanged.connect(self.filter_actions)
        self.search.returnPressed.connect(self.activate_current)
        self.results.itemActivated.connect(self.activate_current)
        self.results.currentItemChanged.connect(self.update_reason)
        self.filter_actions("")
        QTimer.singleShot(0, self.search.setFocus)

    def eventFilter(self, watched, event):
        if watched is self.search and event.type() == QEvent.KeyPress and event.key() in {Qt.Key_Down, Qt.Key_Up}:
            step = 1 if event.key() == Qt.Key_Down else -1
            self.results.setCurrentRow(max(0, min(self.results.count() - 1, self.results.currentRow() + step)))
            self.results.setFocus()
            return True
        return super().eventFilter(watched, event)

    def filter_actions(self, text):
        terms = text.casefold().split()
        self.results.clear()
        for action in self.actions:
            haystack = (action["title"] + " " + action.get("keywords", "")).casefold()
            if all(term in haystack for term in terms):
                label = action["title"] + (" · недоступно" if not action.get("enabled", True) else "")
                item = QListWidgetItem(label)
                item.setData(Qt.UserRole, action)
                item.setToolTip(action.get("reason") or action["title"])
                if not action.get("enabled", True):
                    item.setForeground(self.palette().color(self.palette().Disabled, self.palette().Text))
                self.results.addItem(item)
        if self.results.count():
            self.results.setCurrentRow(0)
        else:
            self.reason.setText("Ничего не найдено. Попробуйте «URL», «сравнение», «окно» или «скан».")
            self.open_button.setEnabled(False)

    def update_reason(self):
        item = self.results.currentItem()
        action = item.data(Qt.UserRole) if item else {}
        self.reason.setText(action.get("reason", ""))
        self.open_button.setEnabled(bool(action) and action.get("enabled", True))

    def activate_current(self):
        item = self.results.currentItem()
        action = item.data(Qt.UserRole) if item else {}
        if action and action.get("enabled", True):
            self.selected_callback = action["callback"]
            self.accept()
