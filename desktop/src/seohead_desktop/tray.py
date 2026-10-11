"""Tray mode: the window may stay hidden while the core's scheduler runs the due scans of the remembered projects.

Each tick calls the core's `schedule-run-due` for every remembered project (the core decides what is due and refuses
overlap). The Mac is kept awake only while the core reports a running scan. Nothing is stored here: the projects come
from the window's recent list, the schedule from the project files.
"""

from __future__ import annotations

from PyQt5.QtCore import QObject, QTimer
from PyQt5.QtWidgets import QAction, QApplication, QMenu, QStyle, QSystemTrayIcon

from .i18n import tr, trf
from .keep_awake import KeepAwake
from .schedule_core import ScheduleJobs, run_due
from .source_service import read_cli

INTERVAL_MS = 60_000


def run_pass(executable, directories, call=read_cli):
    """Run due scans for each project; a project the core cannot answer for is counted, not fatal."""
    total, unavailable = 0, 0
    for directory in directories:
        try:
            result = run_due(executable, directory, call=call)
            total += result["running"]
        except ValueError:
            unavailable += 1
    return {"running": total, "unavailable": unavailable, "projects": len(directories)}


class TrayMode(QObject):
    def __init__(self, window, keep_awake=None, parent=None):
        super().__init__(parent or window)
        self.window = window
        self.keep_awake = keep_awake or KeepAwake()
        self.jobs = ScheduleJobs(self)
        self.timer = QTimer(self)
        self.timer.setInterval(INTERVAL_MS)
        self.timer.timeout.connect(self.check)
        self.icon = QSystemTrayIcon(window.style().standardIcon(QStyle.SP_ComputerIcon), self)
        menu = QMenu()
        show = QAction(tr("Открыть SEOHEAD"), menu)
        show.triggered.connect(self.show_window)
        now = QAction(tr("Проверить расписание сейчас"), menu)
        now.triggered.connect(self.check)
        quit_action = QAction(tr("Выйти"), menu)
        quit_action.triggered.connect(self.quit)
        for action in (show, now):
            menu.addAction(action)
        menu.addSeparator()
        menu.addAction(quit_action)
        self.icon.setContextMenu(menu)
        self.icon.activated.connect(self._activated)
        self.icon.setToolTip(tr("SEOHEAD · расписание"))

    def start(self):
        self.icon.show()
        self.timer.start()
        self.check()

    def check(self):
        executable = getattr(self.window, "core_executable", None)
        directories = [item["path"] for item in getattr(self.window, "recent_projects", [])]
        if not executable or not directories:
            self._report({"running": 0, "unavailable": 0, "projects": len(directories)})
            return
        self.jobs.submit(lambda: run_pass(executable, directories), self._report, self._failed, self)

    def _report(self, result):
        self.keep_awake.set_active(result["running"] > 0)
        if result["running"]:
            text = trf("Сканов по расписанию сейчас: {n}", n=result["running"])
        else:
            text = tr("Расписание: активных сканов нет")
        self.icon.setToolTip(text)

    def _failed(self, _text):
        self.keep_awake.set_active(False)
        self.icon.setToolTip(tr("Расписание: ядро не ответило"))

    def _activated(self, reason):
        if reason == QSystemTrayIcon.Trigger:
            self.show_window()

    def show_window(self):
        self.window.showNormal()
        self.window.raise_()
        self.window.activateWindow()

    def quit(self):
        self.timer.stop()
        self.keep_awake.stop()
        self.icon.hide()
        self.window.close()
        QApplication.quit()


def install(window):
    """Start tray mode when the platform has a system tray; returns None (and the window stays normal) when it has none."""
    if not QSystemTrayIcon.isSystemTrayAvailable():
        return None
    tray = TrayMode(window)
    tray.start()
    window._tray = tray  # keep the controller alive for the application's lifetime
    return tray
