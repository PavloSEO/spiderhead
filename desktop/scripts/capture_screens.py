#!/usr/bin/env python3
"""Render native screens offscreen to PNG: capture_screens.py OUT_DIR NAME [NAME ...] [--theme light] [--lang ru|en] [--sizes 1440x900,800x800].

NAME: settings:<section id> | start[:banner] (Start, no project open; banner = one missing recent folder) | tabsettings | shell[:<section>] | url[:<detail tab>[:<path>]] | help[:url-section] | logsbots (Проверка ботов, extra screen) | newscan[:state] | scanset:<page> | quickscan[:state] | projschedule (project settings → Расписание) | menu | gallery | modal:<id> (one per modal of the Modals board: settings, project-settings, help, action-finder, tab-config, new-project, crawl-config, permission, new-scan, scan-settings, quick-scan) (the three scan kinds: capture_scan_dialog.py). In-memory settings; scans only through --project.
Options for shell: --project DIR opens an existing project through the core CLI (read-only; e.g. the QA project) and
--display simple switches the display; ``shell:scans`` selects a navigation section after the project has loaded.
``shell:graph`` needs a QA project with a saved scan: --project DIR, or the SEOHEAD_QA_PROJECT environment variable.
``url:hist`` needs --project with at least two saved scans: it selects the first URL of the URL section and opens its
detail tab (``hist`` = История, reads every saved scan through the core). Such a project: ``seohead project-new``
on a local fixture host, then two ``seohead crawl-site --approve-large-crawl --producer-build <sha>`` runs into it
(examples/qa-site serves the fixture; no crawl leaves loopback).
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from PyQt5.QtCore import Qt
from PyQt5.QtGui import QPixmap
from PyQt5.QtWidgets import QApplication, QDialog

from seohead_desktop import i18n, qt, theming
from seohead_desktop.app import load_theme
from seohead_desktop.settings_store import AppSettings
from seohead_desktop.ui.settings import full_schema
from seohead_desktop.ui.settings.context import SettingsContext
from seohead_desktop.ui.settings.dialog import SettingsDialog

OPTIONS = {}
SEMIMPORT_SAMPLE = Path(__file__).resolve().parents[1] / "tests" / "core_fixtures" / "semimport_sample.csv"


def open_project(window, directory, timeout=60):
    """Read an existing project through the core and wait (event loop running) until the window is idle."""
    import time

    from PyQt5.QtWidgets import QApplication

    window.core_executable = OPTIONS.get("core") or window.core_executable
    window.read_project(str(Path(directory).resolve()))
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        QApplication.processEvents()
        if window.project_result is not None and not window._project_loading and not window.requests and window._workspace_restore is None:
            break
        time.sleep(0.05)
    for _ in range(40):  # let queued follow-up reads (scans, tasks, observer) land
        QApplication.processEvents()
        time.sleep(0.05)


def render_modal(dialog, width, height, theme, lang):
    """Settings are a modal window: paint the dialog centred over the dimmed application window, as in the canvas."""
    from PyQt5.QtGui import QColor, QPainter

    from seohead_desktop.app import MainWindow

    window = MainWindow(persistent=False)
    window.prefs.set("view.theme", theme)
    window.prefs.set("view.language", lang)
    window.show_startup_workspace()
    if OPTIONS.get("project"):
        open_project(window, OPTIONS["project"])
    window.setAttribute(Qt.WA_DontShowOnScreen, True)
    window.resize(width, height)
    window.show()
    QApplication.processEvents()
    window.resize(width, height)  # the offscreen screen is 800x600 and clamps the first resize
    QApplication.processEvents()
    image = QPixmap(width, height)
    window.render(image)
    size = dialog.size().boundedTo(image.size() * 0.92)
    dialog.setAttribute(Qt.WA_DontShowOnScreen, True)
    cap = getattr(dialog, "capture_max", (920, 640))
    dialog.resize(min(size.width(), cap[0]), min(size.height(), cap[1]))
    dialog.show()
    QApplication.processEvents()
    painter = QPainter(image)
    painter.fillRect(image.rect(), QColor(0, 0, 0, 100))
    shot = dialog.grab()
    painter.drawPixmap((width - shot.width()) // 2, (height - shot.height()) // 2, shot)
    painter.end()
    window.close()
    return image


def projsources_dialog(state, theme, lang):
    """«Настройки проекта» over a QA-project window; the readiness answers are the saved real core answer (tests/core_fixtures).

    States: ready (as the core answered) | connected (verified access, as provider-verify would report) | key (nothing
    configured) | error (unreadable keys) | failed (the request failed) | partial (empty list) | loading | noproject.
    """
    import copy

    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    from seohead_desktop.app import MainWindow
    from seohead_desktop.screens.project_sources_page import ProjectSettingsDialog
    from tests._screens_core import fixture, open_qa

    window = MainWindow(persistent=False)
    window.prefs.set("view.theme", theme)
    window.prefs.set("view.language", lang)
    if state != "noproject":
        open_qa(window)
    data = copy.deepcopy(fixture("provider_readiness.json"))
    for provider in data["providers"].values():
        if state == "connected" and provider["readiness_state"] == "configured_unverified":
            provider["readiness_state"] = "verified"
        elif state == "key" and provider["readiness_state"] != "not_required":
            provider["readiness_state"] = "missing"
        elif state == "error" and provider["readiness_state"] == "configured_unverified":
            provider["readiness_state"] = "invalid"
    if state == "partial":
        data["providers"] = {}

    def request(callback, on_error):
        if state == "failed":
            on_error("CLI ядра seohead не найден")
        elif state != "loading":
            callback(data)

    window.request_providers = request
    dialog = ProjectSettingsDialog(window, window)
    dialog._owner_window = window
    dialog.capture_max = (960, 820)
    return dialog


def wait_until(predicate, timeout=60):
    """Run the event loop (core answers arrive on worker threads) until ``predicate`` holds or the timeout passes."""
    import time

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and not predicate():
        QApplication.processEvents()
        time.sleep(0.05)
    QApplication.processEvents()


def sourcekey_dialog(pid, theme, lang):
    """Settings → Источники данных → one provider, answered by the real core CLI (read-only; the project is optional)."""
    from seohead_desktop.app import MainWindow

    if OPTIONS.get("core"):  # the status bar and the core service look the CLI up on PATH when the window is built
        os.environ["PATH"] = os.pathsep.join([str(Path(OPTIONS["core"]).parent), os.environ.get("PATH", "")])
    window = MainWindow(persistent=False)
    window.prefs.set("view.theme", theme)
    window.prefs.set("view.language", lang)
    window.core_executable = OPTIONS.get("core") or window.core_executable
    if OPTIONS.get("project"):
        open_project(window, OPTIONS["project"])
    dialog = SettingsDialog(window.prefs, window.settings_context(), None, "sources")
    dialog._owner_window = window  # keeps the window (and its core service) alive while the dialog is captured
    page = dialog._pages["sources"][1]
    wait_until(lambda: page.loaded)
    page.show_detail(pid)
    wait_until(lambda: not page._busy and page.verified.get(pid) != "running")
    dialog.capture_max = (1000, 760)
    return dialog


# modal:<id> — one capture per modal dialog of the Modals board; scan modals reuse the scan captures.
MODAL_SCREENS = {"new-scan": "newscan", "scan-settings": "scanset:speed", "quick-scan": "quickscan"}
MODAL_DIALOGS = ("settings", "project-settings", "help", "action-finder", "tab-config", "new-project", "crawl-config", "permission")


def modal_dialog(arg, width, height, theme, lang):
    """A real dialog of the app over a window; crawl-config takes the saved core descriptor (tests/core_fixtures)."""
    from seohead_desktop.app import MainWindow

    window = MainWindow(persistent=False)
    window.prefs.set("view.theme", theme)
    window.prefs.set("view.language", lang)
    window.show_startup_workspace()
    if OPTIONS.get("project"):
        open_project(window, OPTIONS["project"])
    if arg == "help":
        from seohead_desktop.ui.help_guide import HelpGuideDialog

        return HelpGuideDialog(window)
    if arg == "action-finder":
        from seohead_desktop.ui.workspace import ActionFinder

        return ActionFinder(window.action_registry(), window)
    if arg == "tab-config":
        from seohead_desktop.ui.components import TabConfigurationDialog
        from seohead_desktop.ui.tabcatalogue import MAIN_TABS

        return TabConfigurationDialog(MAIN_TABS, [spec.id for spec in MAIN_TABS], window)
    if arg == "new-project":
        from seohead_desktop.screens.start import NewProjectDialog

        return NewProjectDialog(window, window)
    if arg == "permission":
        from seohead_desktop.mcp_integration import PermissionDialog

        return PermissionDialog(OPTIONS.get("core") or "seohead", ("claude-code",), window)
    if arg == "crawl-config":
        from seohead_desktop.ui.crawl_configuration_dialog import CrawlConfigurationDialog
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from tests._screens_core import fixture

        return CrawlConfigurationDialog(fixture("crawl_describe_settings.json"), window)
    raise SystemExit(f"unknown modal: {arg}")


def url_tab(window, tab, path="", timeout=120):
    """Select a saved URL by path (or the first row) and open its detail on ``tab``; every answer is read through the core."""
    import time

    deadline = time.monotonic() + timeout

    def wait(done):
        while time.monotonic() < deadline and not done():
            QApplication.processEvents()
            time.sleep(0.05)
        QApplication.processEvents()

    window.navigation.select_section("url")
    screen = window.screens["url"]
    wait(lambda: bool(screen.rows))
    from urllib.parse import urlsplit

    index = next((i for i, row in enumerate(screen.rows) if path and urlsplit(row.get("url", "")).path == path), 0)
    screen.table.selectRow(index)
    wait(lambda: screen.detail_job.busy or screen._select_timer.isActive() or screen.detail_data is not None)
    wait(lambda: not screen.detail_job.busy and not screen._select_timer.isActive())
    screen._open_tab(tab)
    wait(lambda: not screen.bottom.history.busy if tab == "hist" else True)
    for _ in range(20):
        QApplication.processEvents()
        time.sleep(0.02)


def build(name, width, height, store, theme="light", lang="ru"):
    kind, _, arg = name.partition(":")
    i18n.set_language(lang)
    if kind == "modal":
        if arg in MODAL_SCREENS:
            return build(MODAL_SCREENS[arg], width, height, store, theme, lang)
        if arg == "settings":
            return build("settings:general", width, height, store, theme, lang)
        if arg == "project-settings":
            return projsources_dialog("ready", theme, lang)
        if arg in MODAL_DIALOGS:
            return modal_dialog(arg, width, height, theme, lang)
        raise SystemExit(f"unknown modal: {arg}")
    if kind == "settings":
        dialog = SettingsDialog(store, SettingsContext(), section=arg or "general")
        dialog.resize(width, height)
        return dialog
    if kind in ("start", "tabsettings", "url"):
        # Start («Проекты») before any project is open. Without recent projects the first-run wizard shows instead
        # (screens.show_start) until it is passed, so the plain state sets shell.onboarding_done: the empty list.
        # "banner" = one recent folder that no longer exists (the relocate/forget banner).
        # «Вкладки и панели» opened the way a user does it: the tune button at the right of the tab strip.
        from seohead_desktop.app import MainWindow

        window = MainWindow(persistent=False)
        window.prefs.set("view.theme", theme)
        window.prefs.set("view.language", lang)
        if kind == "tabsettings":
            window.show_startup_workspace()
            if OPTIONS.get("project"):
                open_project(window, OPTIONS["project"])
            window.workspace_tabs.overflow_button.click()
            return window.tab_settings
        if kind == "url":
            if not OPTIONS.get("project"):
                raise SystemExit("url:<tab> needs --project")
            window.show_startup_workspace()
            open_project(window, OPTIONS["project"])
            tab, _, path = arg.partition(":")
            url_tab(window, tab or "info", path)
        else:
            window.prefs.set("shell.onboarding_done", True)
            if arg == "banner":
                window.recent_projects = [{"label": "Старый блог", "path": "/nonexistent/old-blog", "opened_at": "2026-10-10T09:00+03:00"}]
            window.show_startup_workspace()
        return window
    if kind == "shell":
        from seohead_desktop.app import MainWindow

        window = MainWindow(persistent=False)
        window.prefs.set("view.theme", theme)
        window.prefs.set("view.language", lang)
        window.prefs.set("shell.onboarding_done", True)  # captures show the workspace, not the first-run wizard
        if arg == "simple" or OPTIONS.get("display") == "simple":
            window.set_display("simple", remember=False)
        window.show_startup_workspace()
        if OPTIONS.get("project"):
            open_project(window, OPTIONS["project"])
        if arg and arg != "simple":
            window.navigation.select_section(arg)
        return window
    if kind == "help":
        # «Справка по разделам» (menu → show_screen("help")); ``help:<view>`` selects the guide section of that view.
        from seohead_desktop.app import MainWindow
        from seohead_desktop.screens.help import ITEMS

        window = MainWindow(persistent=False)
        window.prefs.set("view.theme", theme)
        window.prefs.set("view.language", lang)
        window.show_startup_workspace()
        window.show_screen("help")
        if arg:
            screen = window.extra_screens["help"]
            view = {"url-section": "url"}.get(arg, arg)  # NAME help:url-section → the URL guide section
            index = next(i for i, item in enumerate(ITEMS) if item[4] == view)
            screen._show(index)
        return window
    if kind in ("newscan", "scanset", "quickscan"):
        from capture_scan_dialog import Job

        return Job(name, OPTIONS, open_project)
    if kind == "logsbots":
        # Проверка ботов is an extra screen (not a navigation slot): the whole window, with the screen in front.
        from seohead_desktop.app import MainWindow

        window = MainWindow(persistent=False)
        window.prefs.set("view.theme", theme)
        window.prefs.set("view.language", lang)
        window.show_startup_workspace()
        if OPTIONS.get("project"):
            open_project(window, OPTIONS["project"])
        window.show_screen("logs_bots")
        return window
    if kind == "projsources":
        return projsources_dialog(arg or "ready", theme, lang)
    if kind == "projschedule":
        return projschedule_dialog(theme, lang)
    if kind == "sources":
        from capture_sources import dialog

        return dialog(arg or "list")
    if kind == "sourcekey":
        return sourcekey_dialog(arg or "arsenkin", theme, lang)
    if kind == "menu":
        from seohead_desktop.app import MainWindow

        window = MainWindow(persistent=False)
        window.prefs.set("view.theme", theme)
        window.prefs.set("view.language", lang)
        return window.build_profile_menu()
    if kind == "semimport":
        # Проект → Импорт фраз в ядро: the sample CSV is a test fixture, the project is the one given with --project.
        from seohead_desktop.app import MainWindow

        window = MainWindow(persistent=False)
        window.prefs.set("view.theme", theme)
        window.prefs.set("view.language", lang)
        window.show_startup_workspace()
        if OPTIONS.get("project"):
            open_project(window, OPTIONS["project"])
        window.show_screen("semimport")
        window.extra_screens["semimport"].load_file(SEMIMPORT_SAMPLE)
        return window
    if kind == "gallery":
        from seohead_desktop.ui.theme_gallery import build_board
        return build_board(width, height)
    raise SystemExit(f"unknown screen: {name}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out_dir", type=Path)
    parser.add_argument("names", nargs="+")
    parser.add_argument("--theme", default="light", choices=theming.THEMES)
    parser.add_argument("--lang", default="ru", choices=i18n.LANGUAGES)
    parser.add_argument("--sizes", default="1440x900,800x800")
    parser.add_argument("--project", type=Path, help="existing project directory to open through the core (read-only)")
    parser.add_argument("--core", help="seohead CLI executable (default: .venv-desktop/bin/seohead next to the repo)")
    parser.add_argument("--display", choices=("agent", "simple"), default="agent")
    args = parser.parse_args(argv)
    if args.project is None and os.environ.get("SEOHEAD_QA_PROJECT"):
        args.project = Path(os.environ["SEOHEAD_QA_PROJECT"])
    if any(name.startswith("shell:graph") for name in args.names) and args.project is None:
        parser.error("shell:graph needs a QA project with a saved scan: pass --project DIR or set SEOHEAD_QA_PROJECT")
    default_core = Path(__file__).resolve().parents[2] / ".venv-desktop/bin/seohead"
    OPTIONS.update(project=args.project, display=args.display, core=args.core or (str(default_core) if default_core.exists() else None))
    os.environ.setdefault("SEOHEAD_ALLOW_PRIVATE_HOSTS", "crawl.localhost,127.0.0.1")
    app = qt.app(sys.argv[:1])
    app.setStyle("Fusion")
    load_theme(app, args.theme)
    store = AppSettings(schema=full_schema())
    args.out_dir.mkdir(parents=True, exist_ok=True)
    for name in args.names:
        for size in args.sizes.split(","):
            width, height = (int(v) for v in size.split("x"))
            widget = build(name, width, height, store, args.theme, args.lang)
            suffix = "" if args.lang == "ru" else f"-{args.lang}"
            path = args.out_dir / f"{name.replace(':', '-').replace('/', '_')}-{args.theme}-{size}{suffix}.png"
            if name.startswith(("settings", "tabsettings", "projsources", "projschedule", "sources", "sourcekey")) or isinstance(widget, QDialog):  # settings, sources, project settings and every modal:<id> dialog
                image = render_modal(widget, width, height, args.theme, args.lang)
                image.save(str(path))
            elif hasattr(widget, "render_image"):
                widget.render_image(width, height, args.theme, args.lang).save(str(path))
            elif name.startswith(("shell", "start", "url", "help", "logsbots", "semimport")):
                # The offscreen screen is 800x600 and clamps top-level windows; render at the requested size instead.
                widget.setAttribute(Qt.WA_DontShowOnScreen, True)
                widget.resize(width, height)
                widget.show()
                app.processEvents()
                widget.resize(width, height)
                app.processEvents()
                image = QPixmap(width, height)
                widget.render(image)
                image.save(str(path))
            else:
                widget.show()
                app.processEvents()
                widget.resize(widget.sizeHint())  # a popup taller than the 800x600 offscreen screen is clamped otherwise
                app.processEvents()
                image = QPixmap(widget.size())
                widget.render(image)
                image.save(str(path))
            print(path)
            widget.close()
    app.quit()
    return 0


if __name__ == "__main__":
    qt.exit_now(main())  # no interpreter finalisation: Qt objects must not be torn down by Python
