"""Design-v2 screens. Each module registers its class in SCREENS; the window swaps them in for the legacy pages."""

from __future__ import annotations

from importlib import import_module

from .base import SLOTS

# slot -> "module:Class"; modules appear as screens are rebuilt from the canvas
SCREENS = {
    "scans": "scans:ScansScreen",
    "journal": "journal:JournalScreen",
    "work": "work:WorkScreen",
    "tasks": "simple:SimpleScreen",
    "inbox": "inbox:InboxScreen",
    "content_search": "search:SearchScreen",
    "issues": "issues:IssuesScreen",
    "url": "url:UrlScreen",
    "reports": "project_export:ProjExportScreen",
}
# name -> "module:Class"; screens outside the navigation slots (start, first-run wizard), appended after the pages
EXTRAS = {
    "start": "start:StartScreen",
    "onboarding": "onboarding:OnboardingScreen",
    "backlinks": "backlinks:BacklinksScreen",
    "logs_bots": "logs_bots:LogsBotsScreen",
    "logs": "logs:LogsScreen",
    "logs_compare": "logs_compare:LogsCompareScreen",
    "project_settings": "project_sources_page:ProjectSettingsPage",
    "help": "help:HelpScreen",
    "new_tab": "new_tab:NewTabScreen",
    "fatal": "fatal:FatalStatesScreen",
    "semantics_io": "semantics_io:SemImportScreen",
    "tools": "tools:ToolsScreen",
    "semantics": "semantics:SemanticsScreen",
    "semantics_read": "semantics:SemanticsReadScreen",
    "tool_run": "tool_run:ToolRunScreen",
    "semimport": "sem_import:SemImportScreen",
    "competitors": "competitors:CompetitorsScreen",
}
# top-bar widgets that a screen with ``chrome_free = True`` hides (SHELL-CANON §7)
PROJECT_CHROME = (
    "project_button",
    "project_chevron",
    "scan_button",
    "scan_state_badge",
    "new_scan",
    "refresh_button",
)


def install_screens(window):
    """Replace legacy pages by the screens registered in SCREENS (legacy widgets stay referenced, hidden)."""
    window._legacy_pages = getattr(window, "_legacy_pages", {})
    window.screens = getattr(window, "screens", {})
    for slot, target in SCREENS.items():
        module, _, name = target.partition(":")
        screen = getattr(import_module(f"{__name__}.{module}"), name)(window)
        index = SLOTS[slot]
        legacy = window.pages.widget(index)
        window.pages.removeWidget(legacy)
        legacy.hide()
        window._legacy_pages[slot] = legacy
        window.pages.insertWidget(index, screen)
        window.screens[slot] = screen
    window.extra_screens = getattr(window, "extra_screens", {})
    for name, target in EXTRAS.items():
        module, _, cls = target.partition(":")
        screen = getattr(import_module(f"{__name__}.{module}"), cls)(window)
        window.pages.addWidget(screen)
        window.extra_screens[name] = screen
    window.open_url_filtered = lambda label, filters: open_url_filtered(window, label, filters)
    window.show_screen = lambda name: show_screen(window, name)
    window.show_start = lambda: show_start(window)
    window.leave_start = lambda: leave_start(window)
    window.pages.currentChanged.connect(lambda _index: sync_chrome(window))
    window.navigation.contextRequested.connect(lambda name: show_screen(window, name))
    window.data_changed.connect(
        lambda kind: leave_start(window) if kind == "project" and window.project_directory else None
    )


def open_url_filtered(window, label, filters):
    """Open the URL table with an extra condition (e.g. from «Проблемы»); the URL screen applies it to the whole scan."""
    screen = window.screens.get("url")
    if screen is not None and hasattr(screen, "apply_external"):
        screen.apply_external(label, filters)
    window.navigation.select_section("url")


def show_screen(window, name):
    """Bring an extra screen to the front (it refreshes itself when shown)."""
    screen = window.extra_screens[name]
    screen.refresh()
    window.pages.setCurrentWidget(screen)


def show_start(window):
    """The first-run wizard until it was passed (or a project was ever opened), then the project list."""
    first_run = not window.recent_projects and not window.prefs.get("shell.onboarding_done")
    show_screen(window, "onboarding" if first_run else "start")


def leave_start(window):
    """Back to the page of the selected navigation section once a project is (being) opened."""
    if any(window.pages.currentWidget() is screen for screen in window.extra_screens.values()):
        window.navigate(max(0, window.navigation.currentRow()))


def sync_chrome(window):
    """Hide the project / scan switchers while an extra screen without project context is shown."""
    free = bool(getattr(window.pages.currentWidget(), "chrome_free", False))
    settings = window.pages.currentWidget() is window.extra_screens.get("project_settings")
    compact = window.width() < 1000
    window.navigation.set_settings_context(settings)
    window.workspace_tabs.setVisible(not (settings and compact))
    window.workspace_tabs.set_screen_presentation(
        "Project settings" if settings else None, "settings"
    )
    if settings:
        window.statusBar().hide()
        window.navigation.profile.hide()
    else:
        window.statusBar().show()
        window.navigation.profile.show()
        window.navigation.settings_note.hide()
    for name in PROJECT_CHROME:
        widget = getattr(window, name, None)
        if widget is not None:
            widget.setVisible(not free)
    if not free and hasattr(window, "sync_picker"):
        window.sync_picker(
            window.scan_picker, window.scan_button
        )  # the state badge follows the scan choice
    if settings:
        for name in ("scan_button", "project_chevron", "scan_state_badge", "refresh_button"):
            getattr(window, name).setVisible(not compact)
        window.scan_state_badge.setVisible(
            not compact and isinstance(window.scan_picker.currentData(), dict)
        )
        window.agent_gap.hide()
        window.agent_pill.setVisible(
            not compact and bool(window.agent_pill.text()) and not window.agent_pill.isHidden()
        )
        window.project_button.set_compact(False)
        window.project_button.setMinimumWidth(180)
        window.project_button.setMaximumWidth(240 if compact else 360)
        window.project_button._chevron.setVisible(not compact)
        window.new_scan.setProperty("size", "lg")
        window.new_scan.setFixedHeight(36)
        window.action_finder_button.setFixedHeight(34)
        window.action_finder_button.setProperty("canvas_search", compact)
        window.action_finder_button.style().unpolish(window.action_finder_button)
        window.action_finder_button.style().polish(window.action_finder_button)
    else:
        window.project_button._chevron.show()
