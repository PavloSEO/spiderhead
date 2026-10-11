"""Tray mode: the window may stay hidden while the core runs the due scheduled scans of the remembered projects.

Each tick reads every remembered project's schedule (`schedule-show`) and runs `schedule-run-due` only where a run is
due now. The Mac is kept awake (caffeinate) from the start of such a run until the pass ends, because the core's
run-due call is the scan itself. Nothing is stored here: projects come from the window's recent list, schedules from
the project files. A tick does nothing while a previous pass is still going.
"""

from __future__ import annotations

from PyQt5.QtCore import QObject, QTimer
from PyQt5.QtWidgets import QAction, QApplication, QMenu, QStyle, QSystemTrayIcon

from .i18n import tr, trf
from .keep_awake import KeepAwake
from .schedule_core import ScheduleJobs, read_schedule, run_due

INTERVAL_MS = 60_000


def run_pass(executable, directories, keep_awake, read=read_schedule, run=run_due):
    """Run the due scans of each project. Runs on a worker thread; a project the core cannot answer for is counted."""
    ran = unavailable = 0
    try:
        for directory in directories:
            try:
                state = read(executable, directory)
            except ValueError:
                unavailable += 1
                continue
            if not state["due_now"]:
                continue
            keep_awake.set_active(True)
            try:
                if run(executable, directory)["ran"]:
                    ran += 1
            except ValueError:
                unavailable += 1
    finally:
        keep_awake.set_active(False)
    return {"ran": ran, "unavailable": unavailable, "projects": len(directories)}


class TrayMode(QObject):
    def __init__(self, window, keep_awake=None, parent=None):
        super().__init__(parent or window)
        self.window = window
        self.keep_awake = keep_awake or KeepAwake()
        self.jobs = ScheduleJobs(self)
        self.busy = False
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
        menu.addAction(show)
        menu.addAction(now)
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
        if self.busy:
            return
        if not executable or not directories:
            self.icon.setToolTip(tr("Расписание: нет проектов или ядра"))
            return
        self.busy = True
        self.jobs.submit(
            lambda: run_pass(executable, directories, self.keep_awake),
            self._report,
            self._failed,
            self,
        )

    def _report(self, result):
        self.busy = False
        text = (
            trf("Запусков по расписанию: {n}", n=result["ran"])
            if result["ran"]
            else tr("Расписание: запусков сейчас нет")
        )
        self.icon.setToolTip(text)

    def _failed(self, _text):
        self.busy = False
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
