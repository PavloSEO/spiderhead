"""Stand-in for MainWindow in screen tests: the state and actions the screens read, with every action recorded."""

from PyQt5.QtCore import QObject, pyqtSignal
from PyQt5.QtWidgets import QPushButton


class Navigation:
    def __init__(self):
        self.sections = []

    def select_section(self, name):
        self.sections.append(name)
        return True


class FakeManager:
    max_parallel = 3
    active_count = 0


class FakeHost(QObject):
    data_changed = pyqtSignal(str)

    def __init__(self, project="/project/qa"):
        super().__init__()
        self.project_directory = project
        self._project_loading = False
        self.scan_model = type("Model", (), {"rows": []})()
        self.observed_runs, self.observed_at = [], None
        self.owned = []
        self.selected_scan_path = None
        self.scan_manager = None
        self.navigation = Navigation()
        self.resume_scan_button = QPushButton()
        self.calls = []

    def owned_runs_for_project(self):
        return list(self.owned)

    def set_state(self, runs=(), scans=(), owned=(), observed_at=None, kind="observer"):
        self.observed_runs, self.owned = list(runs), list(owned)
        self.scan_model.rows = list(scans)
        self.observed_at = observed_at
        self.data_changed.emit(kind)

    def _record(name):
        def call(self, *args):
            self.calls.append((name, *args))
        return call

    scan_preview = _record("scan_preview")
    choose_project = _record("choose_project")
    cancel_active_work = _record("cancel_active_work")
    resume_selected_scan = _record("resume_selected_scan")
    open_monitor_window = _record("open_monitor_window")
    refresh_project = _record("refresh_project")
    choose_owned_run = _record("choose_owned_run")

    def select_project_scan(self, scan):
        self.calls.append(("select_project_scan", scan.get("uuid")))
        self.selected_scan_path = scan.get("path")
