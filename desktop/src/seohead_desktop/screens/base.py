"""Base of design-v2 screens.

A screen renders from state the main window already holds (project result, scan rows, observer runs, tasks, inbox)
and asks the window for actions; it never calls the core, SQL or the network itself. ``refresh`` must be cheap and
bounded: tables are models with delegates, never a widget per cell.
"""

from __future__ import annotations

from PyQt5.QtWidgets import QWidget

# page slot -> index in MainWindow.pages (the order the legacy pages were added in)
SLOTS = {"work": 0, "url": 1, "issues": 2, "project": 3, "tasks": 4, "scans": 5, "inbox": 6, "reports": 7, "journal": 8,
         "content_search": 9}


class Screen(QWidget):
    slot = ""            # key of SLOTS this screen occupies
    watches = ()         # MainWindow.data_changed kinds that trigger refresh()

    def __init__(self, host):
        super().__init__()
        self.host = host
        self.setProperty("spaciousPage", False)
        host.data_changed.connect(self._data_changed)

    def _data_changed(self, kind):
        if kind in self.watches:
            self.refresh()

    def refresh(self):  # pragma: no cover - implemented by screens
        raise NotImplementedError

    # Convenience readers (None when nothing is open / loaded); screens must not invent values.
    @property
    def project_open(self):
        return bool(self.host.project_directory)
