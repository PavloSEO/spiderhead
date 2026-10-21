"""Native Qt Widgets shell over declared, local SEOHEAD CLI projections."""

from __future__ import annotations

import argparse
import sys
from functools import lru_cache
from pathlib import Path

from PyQt5.QtCore import (
    QEasingCurve,
    QSettings,
    Qt,
    QThreadPool,
    QTimer,
    QVariantAnimation,
    pyqtSignal,
)
from PyQt5.QtGui import QFontDatabase, QKeySequence, QPainter
from PyQt5.QtSvg import QSvgGenerator
from PyQt5.QtWidgets import (
    QActionGroup,
    QApplication,
    QHBoxLayout,
    QMainWindow,
    QShortcut,
    QVBoxLayout,
    QWidget,
)

from . import brand, i18n, qt, theming
from .agent import AgentMixin
from .chrome import ChromeMixin
from .commands import CommandsMixin
from .common import (  # noqa: F401
    CONSUMER_ID,
    PAGE_LIMIT,
    ROOT,
    configure_table,
    emits,
    plain,
    scan_request_key,
)
from .comparison import ComparisonController
from .content_search import ContentSearchController
from .core_discovery import discover_core
from .inbox import InboxMixin
from .pages import PagesMixin
from .project_io import ProjectMixin
from .scan_control import ScanControlMixin
from .scans_urls import ScansUrlsMixin
from .screens import install_screens
from .settings_store import AppSettings
from .shell_mixin import ShellMixin
from .tray import install as install_tray
from .ui import motion
from .ui.content_search_panel import ContentSearchPanel
from .ui.menus import entry, fill_menu, unavailable
from .ui.panels import component_stylesheet
from .ui.popup_style import install_popup_style
from .ui.presentation import (
    InlineNotice,
    theme_tokens,
)
from .ui.settings import full_schema
from .ui.shell import NavPanel
from .ui.workspace import (
    LAYOUTS,
    keep_on_screen,
    system_reduced_motion,
)
from .ui.workspace_tabs import WorkspaceContext, WorkspaceTabs
from .view_state import ViewStateMixin
from .workspace_mixin import WorkspaceMixin


@lru_cache(maxsize=4)
def _sheet(_theme):
    return theming.stylesheet() + component_stylesheet(theme_tokens())


def load_theme(app, theme=None):
    """Apply fonts, icon and the generated QSS of one design-v2 theme (default: the active one)."""
    if theme:
        theming.set_active_theme(theme)
    tokens = theme_tokens()
    app.setWindowIcon(brand.app_icon())
    if not app.property("spiderheadFontsRegistered"):
        font_ids = [
            QFontDatabase.addApplicationFont(str(font))
            for font in sorted((ROOT / "assets/fonts").glob("*.ttf"))
        ]
        app.setProperty(
            "spiderheadFontsRegistered",
            bool(font_ids) and all(font_id >= 0 for font_id in font_ids),
        )
    # Clearing first lets Qt skip the per-widget restyle of the whole tree (about 1.4 s -> 0.2 s with 3.7k widgets).
    app.setStyleSheet("")
    app.setStyleSheet(_sheet(theming.active_theme()))
    if not app.property("spiderheadMotion"):
        app.setProperty("spiderheadMotion", True)
        motion.install(app)
    install_popup_style(app, tokens["radius_popup"])
    return tokens


class MainWindow(
    ShellMixin,
    ChromeMixin,
    PagesMixin,
    CommandsMixin,
    ProjectMixin,
    ScansUrlsMixin,
    InboxMixin,
    WorkspaceMixin,
    AgentMixin,
    ViewStateMixin,
    ScanControlMixin,
    QMainWindow,
):
    """Desktop presentation adapter. All scanning remains owned by core CLI."""

    crawl_descriptor_changed = pyqtSignal()
    data_changed = pyqtSignal(
        str
    )  # "project" | "scans" | "scan_status" | "tasks" | "inbox" | "observer" | "progress"

    def __init__(self, *, persistent=True, core_executable=None):
        super().__init__()
        self.setWindowTitle("SpiderHead")
        self.resize(1440, 900)
        self.setMinimumSize(
            theming.metrics()["layout"]["min_width"], theming.metrics()["layout"]["min_height"]
        )
        self.persistent = persistent
        self.settings = QSettings("SEOHEAD", "DesktopPreparation") if persistent else None
        self.prefs = AppSettings(self.settings, full_schema())
        i18n.set_language("en")
        self.display = self.prefs.get("shell.display")
        self.core_executable = core_executable or discover_core(self.prefs)
        self.pool = QThreadPool(self)
        self.pool.setMaxThreadCount(4)
        self.read_generation = 0
        self.project_directory = None
        self.project_result = None
        self._known_projects = {}
        self._active_workspace_id = None
        self._workspace_restore = None
        self._switching_workspace = False
        self._url_page_offset = 0
        self._url_page_has_more = False
        self.inbox_revision = None
        self._note_drafts = {}
        self._pending_note = None
        self._project_loading = False
        self.requests = {}
        self.request_handlers = {}
        self.active_commands = {}
        self.pending_commands = {}
        self.mcp_gateway = None
        self.mcp_ready = False
        self.control_server = None
        self.control_endpoint = None
        self.content_search = ContentSearchController(self)
        self.content_search.idle.connect(self._finish_owned_shutdown)
        self.crawl_descriptor = None
        self._crawl_descriptor_error = None
        self._scan_drafts = {}
        self.scan_manager = None
        self.current_project_uuid = None
        self.selected_managed_run_id = None
        self.selected_observed_run_id = None
        self._resume_eligible_path = None
        self._pending_resume_paths = set()
        self._shown_run_error_key = None
        self.selected_scan_path = None
        self.selected_scan_uuid = None
        self.selected_url = None
        self.url_selection_generation = 0
        self.last_observer_signature = None
        self._reload_selected_scan = False
        self.poll_backoff_ms = 500
        self._close_waiting = False
        self._compact = None
        self._narrow_chrome = None
        self._focus_mode = False
        self._panel_intent = {"Навигация": True, "Сводка": None, "Инспектор URL": True}
        self._navigation_compact_intent = None
        self._syncing_panel = False
        self._density = "standard"
        self.recent_projects = self.settings.value("recent_projects", []) if self.settings else []
        if not isinstance(self.recent_projects, list):
            self.recent_projects = []
        self.recent_projects = [
            item
            for item in self.recent_projects[:20]
            if isinstance(item, dict)
            and isinstance(item.get("path"), str)
            and isinstance(item.get("label"), str)
        ]
        system_motion = system_reduced_motion()
        self.system_reduced_motion = system_motion
        self.reduced_motion = system_motion or (
            self.settings.value("reduced_motion", False, type=bool) if self.settings else False
        )
        motion.set_reduced(self.reduced_motion)
        self._navigation_animation = QVariantAnimation(self)
        self._navigation_animation.setDuration(theme_tokens()["motion"]["duration_ms"])
        self._navigation_animation.setEasingCurve(QEasingCurve.InOutCubic)
        self._navigation_animation.valueChanged.connect(
            lambda value: self.navigation.setFixedWidth(int(value))
        )
        self.observed_runs = []
        self.observed_at = None
        self._work_progress = {}
        self._run_envelope = {}
        self._run_history_offset = 0
        self._run_history_limit = 20
        self._run_history_supported = False
        self._run_history_controls = []
        self.monitor = None
        self.current_layout = "url"
        self._single_window_geometry = None
        self.scan_poll_timer = QTimer(self)
        self.scan_poll_timer.setInterval(self.poll_backoff_ms)
        self.scan_poll_timer.timeout.connect(self.poll_active_scan)

        workspace = QWidget()
        workspace.setObjectName("workspace")
        self.setCentralWidget(workspace)
        shell = QVBoxLayout(workspace)
        shell.setContentsMargins(0, 0, 0, 0)
        shell.setSpacing(0)
        self.workspace_tabs = WorkspaceTabs(max_tabs=12)
        shell.addWidget(self.topbar())
        shell.addWidget(self.workspace_tabs)
        workspace.installEventFilter(self)
        self.notice = InlineNotice()
        shell.addWidget(self.notice)

        body = QHBoxLayout()
        body.setContentsMargins(0, 0, 0, 0)
        body.setSpacing(0)
        self.navigation = NavPanel()
        self.navigation.setFixedWidth(theme_tokens()["layout"]["navigation_width"])
        self.navigation.profileClicked.connect(self.show_profile_menu)
        self.navigation.openScanRequested.connect(lambda: self.open_scan_monitor())
        self.navigation.set_display(self.display == "simple")
        body.addWidget(self.navigation)
        from .ui.components import PanelStack

        self.pages = PanelStack()
        motion.fade_stack(self.pages)
        body.addWidget(self.pages, 1)
        shell.addLayout(body, 1)

        self.pages.addWidget(self.work_page())
        self.pages.addWidget(self.url_page())
        self.pages.addWidget(self.audit_page())
        self.pages.addWidget(self.project_page())
        self.comparison = ComparisonController(self)
        self.comparison.changed.connect(self.load_comparison)
        self.project_panels.panel("compare").pair_changed.connect(
            lambda: self.comparison.clear("Выбрана другая пара; нажмите «Сравнить»")
        )
        self.pages.addWidget(self.tasks_page())
        self.pages.addWidget(self.scans_page())
        self.pages.addWidget(self.inbox_page())
        self.pages.addWidget(self.reports_page())
        self.pages.addWidget(self.journal_page())
        self.content_search_panel = ContentSearchPanel()
        self.pages.addWidget(self.content_search_panel)
        self.content_search_panel.searchRequested.connect(
            lambda values: self.content_search.start(**values)
        )
        self.content_search_panel.pageRequested.connect(self.content_search.page)
        self.content_search_panel.cancelRequested.connect(self.content_search.cancel)
        self.content_search_panel.helpRequested.connect(self.show_help)
        self.content_search_panel.openProjectRequested.connect(self.choose_project)
        self.update_content_search_context()
        self.content_search.changed.connect(self.load_content_search)
        install_screens(self)
        self.build_placeholder_pages()
        self.data_changed.connect(lambda _kind: self.sync_navigation_state())
        self.navigation.lockedClicked.connect(lambda _section: self.show_start())
        self.data_changed.connect(lambda _kind: self.refresh_gates())
        self.pages.currentChanged.connect(lambda _index: self.refresh_gates())
        self.navigation.currentRowChanged.connect(self.navigate)
        self.navigation.setCurrentRow(1)
        self.build_status_bar()
        self.sync_navigation_state()

        self.connect_preferences()
        file_menu = self.menuBar().addMenu("Проект")
        file_menu.addAction("Открыть проект…", self.choose_project, QKeySequence.Open)
        fill_menu(
            self.menuBar().addMenu("Правка"),
            [
                unavailable("undo", "Отменить", "⌘Z"),
                None,
                entry(
                    "content_copy", "Копировать URL", lambda: self.screens["url"].copy_row(), "⌘C"
                ),
                entry(
                    "table_rows",
                    "Копировать как TSV",
                    lambda: self.screens["url"].copy_tsv(),
                    "⌘⇧C",
                ),
                entry(
                    "select_all",
                    "Выделить видимые строки",
                    lambda: self.screens["url"].table.selectAll(),
                    "⌘A",
                ),
                None,
                entry("search", "Найти в таблице", self.focus_search, "⌘F"),
            ],
        )
        file_menu.addAction("Импорт фраз в ядро…", lambda: self.show_screen("semimport"))
        file_menu.addAction("Конкуренты…", lambda: self.show_screen("competitors"))
        view_menu = self.menuBar().addMenu("Вид")
        self.panel_actions = {}
        for name, widget in [
            ("Навигация", self.navigation),
            ("Сводка", self.overview),
            ("Инспектор URL", self.inspector),
        ]:
            action = view_menu.addAction(name)
            action.setCheckable(True)
            action.setChecked(True)
            action.toggled.connect(
                lambda shown, name=name, widget=widget: self.panel_action_changed(
                    name, widget, shown
                )
            )
            self.panel_actions[name] = action
        self.panel_actions["Инспектор URL"].toggled.connect(self.inspector_toggle.setChecked)
        view_menu.addSeparator()
        self.expand_action = view_menu.addAction(
            "Развернуть таблицу / вернуть панели", self.toggle_focus_mode
        )
        self.restore_action = view_menu.addAction("Восстановить панели", self.restore_panels)
        # The overview, the inspector and the focus mode belong to the audit page: on every other
        # screen these items toggled hidden legacy widgets and looked broken (D01). The menu only
        # shows them while that page is on screen.
        self._legacy_panel_actions = [
            self.panel_actions["Сводка"],
            self.panel_actions["Инспектор URL"],
            self.expand_action,
            self.restore_action,
        ]
        view_menu.aboutToShow.connect(self._sync_legacy_panel_items)
        self._sync_legacy_panel_items()  # the default page is not the audit one: hide at once
        density_menu = view_menu.addMenu("Плотность таблиц")
        density_group = QActionGroup(self)
        for label, density in [
            ("Компактная · 28 px", "compact"),
            ("Обычная · 32 px", "standard"),
            ("Свободная · 40 px", "comfortable"),
        ]:
            action = density_menu.addAction(label)
            action.setCheckable(True)
            action.setChecked(density == "standard")
            density_group.addAction(action)
            action.triggered.connect(lambda checked, density=density: self.set_density(density))
        layout_menu = view_menu.addMenu("Раскладка")
        for identifier, title in LAYOUTS.items():
            layout_menu.addAction(
                title, lambda checked=False, identifier=identifier: self.apply_layout(identifier)
            )
        layout_menu.addSeparator()
        layout_menu.addAction("Сохранить расположение", self.save_workspace_layout)
        layout_menu.addAction("Восстановить сохранённое", self.restore_workspace_layout)
        view_menu.addAction("Монитор в отдельном окне", self.open_monitor_window)
        self.action_finder_action = view_menu.addAction("Найти действие…", self.show_action_finder)
        self.action_finder_action.setShortcutContext(Qt.ApplicationShortcut)
        motion_action = view_menu.addAction("Уменьшить движение")
        motion_action.setCheckable(True)
        motion_action.setChecked(self.reduced_motion)
        motion_action.setEnabled(not self.system_reduced_motion)
        motion_action.setToolTip(
            "Системное уменьшение движения имеет приоритет"
            if self.system_reduced_motion
            else "Отключить плавное сворачивание навигации"
        )
        motion_action.toggled.connect(self.set_reduced_motion)
        fill_menu(
            self.menuBar().addMenu("Скан"),
            [
                entry(
                    "play_arrow", "Новый скан…", self.scan_preview, self.shortcut_label("new_scan")
                ),
                unavailable("format_list_bulleted", "Скан по списку URL…"),
                None,
                entry(
                    "stop_circle",
                    "Запросить остановку",
                    self.stop_selected_run,
                    self.shortcut_label("stop_scan"),
                ),
                entry("resume", "Продолжить запуск", self.resume_selected_scan),
                None,
                entry(
                    "compare_arrows",
                    "Сравнить запуски…",
                    lambda: self.navigation.select_section("compare"),
                ),
                unavailable("download", "Экспорт…", "⌘E"),
            ],
        )
        fill_menu(
            self.menuBar().addMenu("Агент"),
            [
                entry("smart_toy", "Подключение агента…", self.show_agent_connection),
                entry("inbox", "Входящие", lambda: self.navigation.select_section("inbox")),
                unavailable("edit_note", "Новая заметка агенту", "⌘⇧N"),
                None,
                entry("terminal", "Журнал действий", lambda: self.navigation.select_section("log")),
            ],
        )
        fill_menu(
            self.menuBar().addMenu("Окно"),
            [
                entry("minimize", "Свернуть", self.showMinimized, "⌘M"),
                entry(
                    "open_in_full",
                    "Масштаб",
                    lambda: self.showNormal() if self.isMaximized() else self.showMaximized(),
                ),
                None,
                entry("sensors", "Наблюдение в отдельном окне", self.open_monitor_window),
            ],
        )
        help_menu = self.menuBar().addMenu("Справка")
        self.help_action = help_menu.addAction("Как работать с SEOHEAD…", self.show_help)
        help_menu.addAction("Горячие клавиши…", self.open_shortcuts)
        help_menu.addAction("Справка по разделам", lambda: self.show_screen("help"))
        self.find_shortcut = QShortcut(QKeySequence.Find, self)
        self.find_shortcut.activated.connect(self.focus_search)
        self.region_shortcut = QShortcut("F6", self)
        self.region_shortcut.activated.connect(self.focus_next_region)
        self.copy_shortcut = QShortcut(QKeySequence.Copy, self.table)
        self.copy_shortcut.setContext(Qt.WidgetShortcut)
        self.copy_shortcut.activated.connect(self.copy_url_selection)
        self.apply_shortcuts()
        if self.settings:
            for key, widget in [
                ("geometry", self),
                ("horizontal", self.horizontal),
                ("vertical", self.vertical),
            ]:
                value = self.settings.value(key)
                if value:
                    (widget.restoreGeometry if widget is self else widget.restoreState)(value)
        self.workspace_tabs.selected.connect(self.switch_workspace_tab)
        self.workspace_tabs.newRequested.connect(self.new_workspace_tab)
        self.workspace_tabs.closeRequested.connect(self.close_workspace_tab)
        self.workspace_tabs.duplicateRequested.connect(self.duplicate_workspace_tab)
        self.workspace_tabs.settingsRequested.connect(self.open_tab_settings)
        self.workspace_tabs.install_shortcuts(self)
        initial = WorkspaceContext(view_id="url")
        self._active_workspace_id = initial.id
        self.workspace_tabs.add(initial, select=True)
        self.table.selectRow(0)
        if self.settings:
            QTimer.singleShot(0, self.restore_workspace_layout)
        QTimer.singleShot(0, lambda: keep_on_screen(self))
        self.apply_language()


# Loaders keep updating the legacy models; screens subscribe to the change notification.
for _name, _kind in {
    "project_loaded": "project",
    "clear_workspace_presentation": "project",
    "load_scans": "scans",
    "load_scan_status": "scan_status",
    "load_tasks": "tasks",
    "load_inbox": "inbox",
    "load_unread": "inbox",
    "load_observer": "observer",
    "load_activity": "observer",
    "present_observed_runs": "observer",
    "load_progress": "progress",
}.items():
    setattr(MainWindow, _name, emits(_kind)(getattr(MainWindow, _name)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core-cli", help="Existing seohead CLI executable; local project adapter")
    parser.add_argument(
        "--project",
        type=Path,
        help="Explicit existing SEOHEAD project to open; never starts a scan",
    )
    parser.add_argument(
        "--capture", type=Path, help="Save the native widget rendering to a PNG and exit"
    )
    parser.add_argument(
        "--export-svg", type=Path, help="Export actual Qt painting as SVG for Figma import"
    )
    parser.add_argument("--no-settings", action="store_true")
    parser.add_argument(
        "--tray",
        action="store_true",
        help="Run in the system tray: the core runs due scheduled scans, the window starts hidden",
    )
    parser.add_argument(
        "--agent-control",
        type=Path,
        help="Existing owned runtime directory for explicit local agent control",
    )
    args = parser.parse_args()
    QApplication.setAttribute(Qt.AA_EnableHighDpiScaling)
    QApplication.setAttribute(Qt.AA_UseHighDpiPixmaps)
    app = qt.app(sys.argv[:1])
    brand.set_application_identity(app)
    app.setStyle("Fusion")
    load_theme(app)
    window = MainWindow(persistent=not args.no_settings, core_executable=args.core_cli)
    tray_on = False
    if args.tray:
        app.setQuitOnLastWindowClosed(
            False
        )  # the tray keeps the application alive after the window is hidden
        tray_on = install_tray(window) is not None
        if not tray_on:
            print(
                "seohead: no system tray here; the window opens normally",
                file=sys.stderr,
                flush=True,
            )
    if args.agent_control:
        try:
            descriptor = window.start_agent_control(str(args.agent_control.resolve()))
            print(str(descriptor), file=sys.stderr, flush=True)
        except (OSError, RuntimeError, ValueError) as exc:
            window.notice.show_error("Подключение агента недоступно: " + str(exc))
    if args.capture or args.export_svg:
        window.show()
    else:
        window.show_startup_workspace()
        if tray_on:
            window.hide()
        else:
            window.showMaximized()
    if args.project:
        QTimer.singleShot(0, lambda: window.read_project(str(args.project.resolve())))
    if args.capture or args.export_svg:
        capture_waits = 0

        def capture():
            nonlocal capture_waits
            if args.project and (
                window.project_directory != str(args.project.resolve())
                or window.project_result is None
                or window._project_loading
                or window._workspace_restore is not None
                or window.requests
            ):
                capture_waits += 1
                if capture_waits < 150:
                    QTimer.singleShot(100, capture)
                    return
                print("capture: project did not finish loading", file=sys.stderr)
                window.close()
                app.exit(1)
                return
            if args.capture:
                args.capture.parent.mkdir(parents=True, exist_ok=True)
                window.grab().save(str(args.capture))
            if args.export_svg:
                args.export_svg.parent.mkdir(parents=True, exist_ok=True)
                generator = QSvgGenerator()
                generator.setFileName(str(args.export_svg))
                generator.setSize(window.size())
                generator.setViewBox(window.rect())
                generator.setTitle("SpiderHead")
                painter = QPainter(generator)
                window.render(painter)
                painter.end()
            window.close()
            app.quit()

        QTimer.singleShot(500, capture)
    code = app.exec_()
    if window.settings:
        window.settings.sync()  # objects are not destroyed on exit, so write preferences explicitly
    qt.exit_now(code)  # no interpreter finalisation: PyQt's exit cleanup crashes (SIGSEGV)


if __name__ == "__main__":
    raise SystemExit(main())
