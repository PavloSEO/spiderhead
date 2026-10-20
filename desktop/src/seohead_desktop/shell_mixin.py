"""Main-window methods: status bar, profile menu, display (agent/simple), settings dialog, preference wiring."""

from __future__ import annotations

import os
import sys
from datetime import datetime, timezone
from importlib import metadata

from PyQt5.QtCore import QPoint, QTimer
from PyQt5.QtGui import QKeySequence, QPalette
from PyQt5.QtWidgets import (
    QAction,
    QActionGroup,
    QApplication,
    QLabel,
    QMenu,
    QShortcut,
    QSizePolicy,
    QVBoxLayout,
    QWidget,
    QWidgetAction,
)

from . import i18n, shortcuts, theming
from .screens.graph import GraphScreen
from .screens.scan_common import parse_time
from .source_service import SourceService
from .ui import motion
from .ui.controls import Segmented
from .ui.icons import material_icon as icon
from .ui.motion import BusyIndicator
from .ui.presentation import ElidedLabel
from .ui.settings.context import SettingsContext
from .ui.settings.dialog import SettingsDialog
from .ui.shell import NUMBERED_SECTIONS
from .ui.shortcuts_sheet import ShortcutsSheet

tr, trf, joined = i18n.tr, i18n.trf, i18n.joined

UNAVAILABLE = "Недоступно в этой сборке"


class ShellMixin:
    placeholder_pages = {}  # navigation row -> «Раздел готовится» page (replaced per window)

    def build_placeholder_pages(self):
        """Methods opens the redirect generator; Link graph is GraphScreen (site-wide waiting state)."""
        from .ui.kit import StatePanel
        from .ui.workspace import VIEW_IDS

        self.placeholder_pages = {}
        # «Сканы» without a project would be a misleading project-less screen until the crawler mode exists
        self.scans_placeholder = QWidget()
        placeholder_layout = QVBoxLayout(self.scans_placeholder)
        placeholder_layout.setContentsMargins(0, 0, 0, 0)
        placeholder_layout.addWidget(
            StatePanel(
                "partial",
                "Раздел готовится",
                "Экран появится в одной из следующих версий приложения.",
            )
        )
        self.pages.addWidget(self.scans_placeholder)
        from .screens.crawler import CrawlerScreen
        from .screens.methods import MethodsWorkspace

        for view in ("crawler", "methods"):
            if view == "crawler":
                page = CrawlerScreen(self)
            elif view == "methods":
                page = MethodsWorkspace(self)
            else:
                page = QWidget()
                layout = QVBoxLayout(page)
                layout.setContentsMargins(0, 0, 0, 0)
                layout.addWidget(
                    StatePanel(
                        "partial",
                        "Раздел готовится",
                        "Экран появится в одной из следующих версий приложения.",
                    )
                )
            self.pages.addWidget(page)
            self.placeholder_pages[VIEW_IDS.index(view)] = page

        self.graph_page = GraphScreen(self)
        self.pages.addWidget(self.graph_page)
        self.placeholder_pages[VIEW_IDS.index("graph")] = self.graph_page

    def sync_scans_placeholder(self):
        """Show the placeholder for «Сканы» while no project is open and bring the screen back when one is."""
        if self.navigation.current_section() != "scans" or not hasattr(self, "scans_placeholder"):
            return
        showing = self.pages.currentWidget() is self.scans_placeholder
        if not self.project_directory and not showing:
            self.pages.setCurrentWidget(self.scans_placeholder)
        elif self.project_directory and showing:
            self.navigate(self.navigation.currentRow())

    def sync_navigation_state(self):
        """Locked items without a project and the counters at the right of the items; no data means no number."""
        try:
            opened = bool(self.project_directory)
            self.navigation.set_project_open(opened)
            new_scan = getattr(self, "new_scan", None)
            if (
                new_scan is not None
            ):  # without a project a scan has no target: disabled, with the reason
                new_scan.setEnabled(opened)
                new_scan.setToolTip(
                    ""
                    if opened
                    else tr("Сначала откройте или создайте проект: скан сохраняется в проект")
                )
            self.sync_scans_placeholder()
            url_total = issues = None
            scan = (
                next(
                    (
                        row
                        for row in self.scan_model.rows
                        if row.get("path") == self.selected_scan_path
                    ),
                    None,
                )
                if opened and self.selected_scan_path
                else None
            )
            if scan:
                from .screens.scan_common import number

                url_screen = getattr(self, "screens", {}).get("url")
                total = getattr(url_screen, "total", None)
                if type(total) is not int:
                    total = (
                        ((scan.get("evidence") or {}).get("frontier") or {})
                        .get("counts", {})
                        .get("done")
                    )
                url_total = number(total) if type(total) is int and total > 0 else None
                findings = (scan.get("evidence") or {}).get("findings") or {}
                if (
                    findings.get("state") == "available" and findings.get("truncated") is False
                ):  # the sample is complete: its checks are all of them
                    checks = {
                        item.get("check")
                        for item in findings.get("items") or []
                        if isinstance(item, dict) and item.get("check")
                    }
                    issues = len(checks) or None
            self.navigation.set_count(
                "scans", len(self.scan_model.rows) if opened else None
            )  # real project scans, no number without a project
            self.navigation.set_count("url", url_total)
            self.navigation.set_count("issues", issues)
        except RuntimeError:  # the window was deleted while a timer fired
            pass

    def build_status_bar(self):
        """26 px bar: transient messages on the left, source / display mode / core on the right."""
        bar = self.statusBar()
        bar.setSizeGripEnabled(False)
        bar.setContentsMargins(
            0, 0, 12, 0
        )  # keep the right-hand core/source text off the window edge in every theme
        self.source_badge = ElidedLabel("Проект не открыт")
        self.source_badge.setObjectName("sourceBadge")
        self.source_badge.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Preferred)
        self.source_badge.setMinimumWidth(240)
        self.project_label = QLabel()
        self.scans_label = QLabel()
        self.mode_label = QLabel()
        self.core_label = QLabel()
        self.observed_label = QLabel()
        for widget in (
            self.project_label,
            self.scans_label,
            self.mode_label,
            self.core_label,
            self.observed_label,
        ):
            widget.setContentsMargins(8, 0, 8, 0)
        self.busy = BusyIndicator(14)
        self.busy.hide()
        motion.activity.changed.connect(lambda count: self.busy.setVisible(count > 0))
        bar.addPermanentWidget(self.busy)
        for widget in (
            self.source_badge,
            self.project_label,
            self.scans_label,
            self.mode_label,
            self.core_label,
            self.observed_label,
        ):
            bar.addPermanentWidget(widget)
        self.data_changed.connect(lambda _kind: self.update_status_tail())
        self._status_timer = QTimer(self)  # keeps «N с назад» honest between observations
        self._status_timer.setInterval(5000)
        self._status_timer.timeout.connect(self.update_status_tail)
        self._status_timer.timeout.connect(self.sync_navigation_state)
        self._status_timer.start()
        self.update_display_widgets()

    @staticmethod
    def installed_core_version(core_executable):
        """«3.4» when the core executable belongs to this application's environment and its package version is known."""
        if not core_executable or os.path.dirname(core_executable) != os.path.dirname(
            sys.executable
        ):
            return None
        try:
            parts = metadata.version("seohead-seotools").split(".")
        except metadata.PackageNotFoundError:
            return None
        return ".".join(parts[:2]) if len(parts) >= 2 else None

    def core_state(self):
        """(text, tooltip): the real state of the core. «не найдено» only when nothing answered and no executable is known."""
        answered = bool(self.mcp_ready or self.project_result is not None)
        if not self.core_executable and not answered:
            return tr("Ядро не найдено"), tr("Ядро seohead не найдено ни в одном из известных мест")
        version = self.installed_core_version(self.core_executable)
        path = self.core_executable or ""
        if version:
            return trf("Ядро seohead {version}", version=version), path
        return tr("Ядро подключено · версия неизвестна" if answered else "Ядро найдено"), joined(
            "\n", [path, tr("Версию ядро пока не сообщает")]
        ) if path else tr("Версию ядро пока не сообщает")

    def update_status_tail(self):
        """Right side (SHELL-CANON §6): at most three segments — active scans · display mode · core. The project name lives
        in the window centre text and the observation age in the core tooltip."""
        try:
            opened = bool(self.project_directory)
            self.project_label.setVisible(False)
            self.observed_label.setVisible(False)
            self.scans_label.setVisible(opened)
            if opened:
                from .screens.scan_common import build_rows

                self.scans_label.setText(
                    trf("Активных сканов: {n}", n=sum(1 for row in build_rows(self) if row.active))
                )
            text, tip = self.core_state()
            observed = ""
            if opened:
                stamp = parse_time(self.observed_at) if isinstance(self.observed_at, str) else None
                if stamp is None:
                    observed = (
                        tr("Наблюдение: нет данных") + " · " + tr("Недоступно в этой версии ядра")
                    )
                else:
                    seconds = max(0, int((datetime.now(timezone.utc) - stamp).total_seconds()))
                    ago = (
                        trf("{n} с", n=seconds)
                        if seconds < 60
                        else trf("{n} мин", n=seconds // 60)
                        if seconds < 3600
                        else trf("{n} ч", n=seconds // 3600)
                    )
                    observed = trf("Наблюдение: {ago} назад", ago=ago)
            key = (self.core_executable, bool(self.mcp_ready or self.project_result is not None))
            if (
                key != getattr(self, "_core_key", None) or "seohead" in text
            ):  # plain texts follow the language by retranslate
                self._core_key = key
                self.core_label.setText(text)
            self.core_label.setToolTip(joined("\n", [tip, observed]))
        except RuntimeError:  # the window was deleted while a timer fired
            pass

    def update_display_widgets(self):
        simple = self.display == "simple"
        self.mode_label.setText(
            tr("Простой режим · агент и MCP выключены" if simple else "С агентом")
        )
        self.simple_pill.setVisible(simple)
        self.agent_pill.setVisible(
            False
        )  # shown only from a real agent heartbeat (step 7); never claimed here
        self.update_status_tail()

    def set_display(self, mode, remember=True):
        if mode not in ("agent", "simple"):
            raise ValueError(mode)
        self.display = mode
        self.navigation.set_display(mode == "simple")
        if self.navigation.current_section() is None:
            self.navigation.select_section("url")
        self.update_display_widgets()
        self.show_display_page()
        if remember:
            self.prefs.set("shell.display", mode)

    def install_display_menus(self, view_menu):
        """View › Display (⌥⌘1 / ⌥⌘2) and View › Theme, shared by the agent and the Simple menu bars."""
        group = QActionGroup(self)
        self.display_actions = {}
        for mode, title, keys in (("simple", "Display: Simple", "Ctrl+Alt+1"), ("agent", "Display: With agent", "Ctrl+Alt+2")):
            action = QAction(title, self)
            action.setCheckable(True)
            action.setShortcut(QKeySequence(keys))
            action.triggered.connect(lambda _c=False, m=mode: self.set_display(m))
            group.addAction(action)
            self.addAction(action)
            self.display_actions[mode] = action
        self.add_display_items(view_menu)
        self.agent_menus = list(self.menuBar().actions())
        self.simple_menus = []
        self._agent_shortcuts = set()

    def add_display_items(self, menu):
        menu.addSeparator()
        for action in self.display_actions.values():
            menu.addAction(action)
        theme = menu.addMenu("Theme")
        theme.aboutToShow.connect(lambda: self.fill_theme_items(theme))

    def fill_theme_items(self, menu):
        menu.clear()
        current = self.prefs.get("view.theme")
        group = QActionGroup(menu)
        for value, title in (("light", "Light"), ("dark", "Dark"), ("system", "System"), ("hc", "High contrast")):
            action = menu.addAction(title)
            action.setCheckable(True)
            action.setChecked(value == current)
            group.addAction(action)
            action.triggered.connect(lambda _c, v=value: self.prefs.set("view.theme", v))

    def _toggle_agent_menu_keys(self, simple):
        """Agent menu actions with keys are disabled while Simple mode shows and restored as they were."""
        if simple:
            saved = getattr(self, "_agent_menu_state", None) or {}
            for top in self.agent_menus:
                for action in self._menu_tree(top.menu()):
                    if (
                        not action.shortcut().isEmpty()
                        and action not in saved
                        and action not in self.display_actions.values()
                    ):
                        saved[action] = action.isEnabled()
                        action.setEnabled(False)
            self._agent_menu_state = saved
        else:
            for action, enabled in (getattr(self, "_agent_menu_state", None) or {}).items():
                try:
                    action.setEnabled(enabled)
                except RuntimeError:
                    pass
            self._agent_menu_state = {}

    @classmethod
    def _menu_tree(cls, menu):
        for action in menu.actions() if menu is not None else ():
            if action.menu() is not None:
                yield from cls._menu_tree(action.menu())
            else:
                yield action

    def show_display_page(self):
        """Simple mode is its own central page with its own menus and status strip; the agent display is untouched."""
        if not hasattr(self, "display_actions"):
            return  # the window is still being built
        simple = self.display == "simple"
        self.display_actions[self.display].setChecked(True)
        if simple and self.simple_page is None:
            from .screens.simple_crawler import SimpleCrawlerScreen

            self.simple_page = SimpleCrawlerScreen(self)
            self.mode_stack.addWidget(self.simple_page)
            self.simple_menus = self.simple_page.build_menus(self.menuBar(), self)
        if self.simple_page is None:
            return
        self.mode_stack.setCurrentWidget(self.simple_page if simple else self.workspace_widget)
        self.statusBar().setVisible(not simple)
        for action in self.agent_menus:
            action.setVisible(not simple)
        shared = set(self.display_actions.values())
        for action in self.simple_menus:
            action.setVisible(simple)
            for item in self._menu_tree(action.menu()):
                if item not in shared:
                    item.setEnabled(simple)  # hidden menus must not keep their keys
        self._toggle_agent_menu_keys(simple)
        if simple:  # window-wide agent keys (⌘F, ⌘1…4, F6, …) must not fire over the Simple page
            for shortcut in self.findChildren(QShortcut):
                owner = shortcut.parent()
                if shortcut.isEnabled() and not (owner is self.simple_page or self.simple_page.isAncestorOf(owner)):
                    shortcut.setEnabled(False)
                    self._agent_shortcuts.add(shortcut)
        else:
            for shortcut in self._agent_shortcuts:
                try:
                    shortcut.setEnabled(True)
                except RuntimeError:  # replaced by apply_shortcuts meanwhile
                    pass
            self._agent_shortcuts = set()

    # Profile menu (bottom-left)
    def show_profile_menu(self):
        menu = self.build_profile_menu()
        menu.exec_(self.navigation.profile.mapToGlobal(QPoint(0, -menu.sizeHint().height() - 6)))

    def build_profile_menu(self):
        button = self.navigation.profile
        menu = QMenu(self)
        header = QWidget()
        header_layout = QVBoxLayout(header)
        header_layout.setContentsMargins(10, 8, 10, 6)
        header_layout.setSpacing(2)
        name = QLabel(button.name.text())
        name.setProperty("text_style", "control")
        where = QLabel("локально · этот компьютер")
        where.setProperty("text_style", "meta")
        header_layout.addWidget(name)
        header_layout.addWidget(where)
        self._menu_widget(menu, header)
        menu.addSeparator()
        box = QWidget()
        box_layout = QVBoxLayout(box)
        box_layout.setContentsMargins(10, 4, 10, 8)
        label = QLabel("ОТОБРАЖЕНИЕ")
        label.setProperty("text_style", "overline")
        segmented = Segmented(
            [("agent", "С агентом"), ("simple", "Простой")], self.display, "Отображение"
        )
        segmented.changed.connect(lambda mode: (self.set_display(mode), menu.close()))
        box_layout.addWidget(label)
        box_layout.addWidget(segmented)
        self._menu_widget(menu, box)
        crawler = menu.addAction(icon("travel_explore"), "Быстрый краул без проекта")
        crawler.triggered.connect(self.open_crawler)
        settings = menu.addAction(icon("settings"), "Настройки")
        settings.setShortcut(QKeySequence.Preferences)
        settings.triggered.connect(lambda: self.open_settings())
        self._theme_menu(menu)
        self._language_menu(menu)
        mcp = menu.addAction(icon("hub"), "MCP-сервер")
        mcp.triggered.connect(lambda: self.open_settings("mcp"))
        menu.addSeparator()
        cli = menu.addAction(icon("terminal"), "Командная строка")
        cli.setEnabled(False)
        cli.setToolTip(UNAVAILABLE)
        help_action = menu.addAction(icon("help"), tr("Справка"), self.show_help)
        help_action.setShortcut(shortcuts.sequences(self.prefs).get("help", QKeySequence()))
        menu.setMinimumWidth(264)
        i18n.retranslate(menu)
        return menu

    def show_display_menu(self):
        """Chip «Простой режим ▾» (SHELL-CANON §2b): display switch and crawler entry."""
        menu = QMenu(self)
        group = QActionGroup(menu)
        for mode, title in (("agent", "С агентом"), ("simple", "Простой режим")):
            action = menu.addAction(title)
            action.setCheckable(True)
            action.setChecked(mode == self.display)
            group.addAction(action)
            action.triggered.connect(lambda _c, m=mode: self.set_display(m))
        menu.addSeparator()
        crawler = menu.addAction(icon("travel_explore"), "Быстрый краул без проекта")
        crawler.triggered.connect(self.open_crawler)
        i18n.retranslate(menu)
        menu.exec_(self.simple_pill.mapToGlobal(QPoint(0, self.simple_pill.height() + 4)))

    def can_open_crawler(self):
        return True

    def open_crawler(self):
        self.navigation.select_section("crawler")

    @staticmethod
    def _menu_widget(menu, widget):
        action = QWidgetAction(menu)
        action.setDefaultWidget(widget)
        menu.addAction(action)

    def _theme_menu(self, menu):
        names = {
            "light": "Светлая",
            "dark": "Тёмная",
            "hc": "Высокий контраст",
            "system": "Как в системе",
        }
        current = self.prefs.get("view.theme")
        sub = menu.addMenu(icon("palette"), trf("Тема · {name}", name=names[current]))
        group = QActionGroup(sub)
        for value, title in names.items():
            action = sub.addAction(title)
            action.setCheckable(True)
            action.setChecked(value == current)
            group.addAction(action)
            action.triggered.connect(lambda _c, v=value: self.prefs.set("view.theme", v))

    def _language_menu(self, menu):
        names = {"en": "English"}
        current = self.prefs.get("view.language")
        sub = menu.addMenu(icon("translate"), trf("Язык · {name}", name=names[current]))
        group = QActionGroup(sub)
        for value, title in names.items():
            action = sub.addAction(title)
            action.setCheckable(True)
            action.setChecked(value == current)
            group.addAction(action)
            action.triggered.connect(lambda _c, v=value: self.prefs.set("view.language", v))

    # Settings
    def request_providers(self, callback, on_error):
        """Settings → Источники данных: the core's provider-readiness (local, no network), delivered on the UI thread."""
        self._provider_handlers = (callback, on_error)
        if not self.core_executable:
            on_error(tr("CLI ядра seohead не найден"))
            return
        self.start_command("providers", "seo_provider_readiness", {}, self._providers_loaded)

    def _providers_loaded(self, result):
        self._deliver_providers(0, result)

    def providers_failed(self, text):
        self._deliver_providers(1, text)

    def _deliver_providers(self, index, value):
        handlers = getattr(self, "_provider_handlers", None)
        if handlers:
            try:
                handlers[index](value)
            except RuntimeError:
                pass  # the settings dialog was closed before the answer arrived

    def request_semantics_import(self, path, callback, on_error):
        """Project menu → Import phrases into the core: the core's offline init, then the CSV import, one MCP call at a time."""
        self._semimport_handlers = (callback, on_error)
        if not self.core_executable:
            on_error(tr("CLI ядра seohead не найден"))
            return
        if not self.project_directory:
            on_error(tr("Проект не открыт"))
            return
        project = str(self.project_directory)
        self.start_command(
            "semimport",
            "seo_semantics_run",
            {"stage": "init", "project": project},
            lambda _result: self.start_command(
                "semimport",
                "seo_semantics_run",
                {"stage": "import", "project": project, "file": path},
                self._semimport_loaded,
            ),
        )

    def _semimport_loaded(self, result):
        self._deliver_semimport(0, result)

    def semimport_failed(self, text):
        self._deliver_semimport(1, text)

    def _deliver_semimport(self, index, value):
        handlers = getattr(self, "_semimport_handlers", None)
        if handlers:
            handlers[index](value)

    def request_sources(self, operation, callback, on_error, owner, **kwargs):
        """Settings → Источники данных: allowlisted read-only core calls on a worker (source_service); answers for a closed page are dropped."""
        if (
            getattr(self, "source_service", None) is None
            or self.source_service.executable != self.core_executable
        ):
            self.source_service = SourceService(self.core_executable, self)
        self.source_service.request(operation, callback, on_error, owner, **kwargs)

    def settings_context(self):
        return SettingsContext(
            core_executable=self.core_executable,
            project_directory=self.project_directory,
            actions={"providers": self.request_providers, "sources": self.request_sources},
        )

    def open_settings(self, section="general"):
        """Settings are a modal window over the application (sheet Settings), never a workspace tab."""
        SettingsDialog(self.prefs, self.settings_context(), self, section).exec_()

    def shortcut_label(self, action_id):
        """Key-cap text of the user's current binding for menus ('' when unassigned), so labels follow rebinding."""
        return shortcuts.display(shortcuts.bindings(self.prefs).get(action_id, ""))

    def open_shortcuts(self):
        """Canvas «Shortcuts»: the active bindings as a modal sheet; editing and help are one click away."""
        sheet = ShortcutsSheet(self.prefs, self)
        sheet.settingsRequested.connect(lambda: self.open_settings("keys"))
        sheet.helpRequested.connect(self.show_help)
        sheet.exec_()

    def connect_preferences(self):
        self.prefs.changed.connect(self.apply_preference)
        shortcut = QAction("Настройки…", self)
        shortcut.setShortcut(QKeySequence.Preferences)
        shortcut.triggered.connect(lambda: self.open_settings())
        self.addAction(shortcut)
        i18n.signals.changed.connect(self.apply_language)
        for key in ("view.language", "view.theme", "view.density", "view.reduce_motion"):
            self.apply_preference(key)

    def apply_shortcuts(self):
        """Attach the user's bindings (settings → Горячие клавиши) to the existing actions and a few QShortcuts."""
        seqs = shortcuts.sequences(self.prefs)
        none = QKeySequence()
        self.action_finder_action.setShortcut(seqs.get("palette", none))
        self.help_action.setShortcut(seqs.get("help", none))
        self.expand_action.setShortcut(seqs.get("expand_table", none))
        self.find_shortcut.setKey(seqs.get("find_in_table", none))
        handlers = {
            "new_scan": self.scan_preview,
            "settings": self.open_settings,
            "stop_scan": self.cancel_active_work,
            "copy_url": self.copy_url_selection,
            "all_shortcuts": self.open_shortcuts,
            "new_tab": self.new_workspace_tab,
            "close_tab": lambda: self.close_workspace_tab(self._active_workspace_id),
            "copy_tsv": lambda: self.screens["url"].copy_tsv(),
        }
        for old in getattr(self, "_bound_shortcuts", ()):
            old.setEnabled(False)
            old.deleteLater()
        self._bound_shortcuts = []
        for action_id, handler in handlers.items():
            if action_id in seqs:
                shortcut = QShortcut(seqs[action_id], self)
                shortcut.activated.connect(lambda h=handler: h())
                self._bound_shortcuts.append(shortcut)
        for (
            number
        ) in NUMBERED_SECTIONS:  # fixed by the canon: Ctrl+1 Работа · 2 Сканы · 3 URL · 4 Проблемы
            shortcut = QShortcut(QKeySequence(f"Ctrl+{number}"), self)
            shortcut.activated.connect(lambda n=number: self.navigation.select_number(n))
            self._bound_shortcuts.append(shortcut)
        if getattr(self, "display", None) == "simple":
            self.show_display_page()  # new window keys stay off while the Simple page shows

    def apply_preference(self, key):
        if key.startswith("keys."):
            self.apply_shortcuts()
            return
        value = self.prefs.get(key)
        if key == "view.language":
            i18n.set_language(value)
        elif key == "view.theme":
            self.apply_theme(value)
        elif key == "view.density":
            self.set_density(value)
        elif key == "view.reduce_motion":
            self.set_reduced_motion(bool(value) or self.system_reduced_motion)

    def apply_language(self, _language=None):
        """The shell, the settings and every open child window follow the language; selection and data stay as they are."""
        try:
            i18n.retranslate(self)
            for combo, button in (
                (self.project_picker, self.project_button),
                (self.scan_picker, self.scan_button),
            ):
                self.sync_picker(combo, button)
            self.update_display_widgets()
        except RuntimeError:  # the window was deleted while a test or shutdown was running
            pass

    def apply_theme(self, choice):
        name = choice
        if choice == "system":
            if theming.system_prefers_high_contrast():
                name = "hc"
            else:
                window = QApplication.instance().palette().color(QPalette.Window)
                name = "dark" if window.lightness() < 128 else "light"
        if name == theming.active_theme() and QApplication.instance().styleSheet():
            return
        from .app import load_theme

        load_theme(QApplication.instance(), name)
        self.retheme()

    def retheme(self):
        """Re-tint icons that were created with a fixed colour (everything else follows the QSS)."""
        r = theming.roles()
        self.nav_toggle.setIcon(icon("side_navigation"))
        self.refresh_button.setIcon(icon("refresh"))
        self.action_finder_button.setIcon(icon("search"))
        self.cancel_button.setIcon(icon("stop", r["error"]))
        self.new_scan.setIcon(icon("play_arrow", r["on_primary"]))
        self.navigation.set_display(self.display == "simple")
        for button, combo in (
            (self.project_button, self.project_picker),
            (self.scan_button, self.scan_picker),
        ):
            self.sync_picker(combo, button)
        self.update_display_widgets()
