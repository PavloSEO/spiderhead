"""Shared setup for the Work / Simple / Inbox screen tests: real core answers (tests/core_fixtures) fed through the window's own loaders.

The fixtures were captured from the core CLI on a copy of the QA project (qa-site, checklist initialised, two inbox
entries, one rejected by an agent identity, one goal accepted); nothing is invented here.
"""

import copy
import json
from pathlib import Path
from unittest.mock import patch

from PyQt5.QtWidgets import QApplication, QLabel, QPushButton, QToolButton, QWidget

DIRECTORY = "/project/qa"
FIXTURES = Path(__file__).resolve().parent / "core_fixtures"


def fixture(name):
    return copy.deepcopy(json.loads((FIXTURES / name).read_text(encoding="utf-8")))


def open_qa(window, tasks=True, inbox=True, scans=True):
    """Make ``window`` look like a project was opened and read: the loaders run, the commands to the core do not."""
    window.project_directory = DIRECTORY
    window.project_result = fixture("project_open.json")
    window.current_project_uuid = window.project_result["project"]["project_uuid"]
    window._project_loading = False
    window.screen_errors = {}
    window._work_progress = fixture("progress.json")
    with patch.object(window, "start_command"):
        if tasks:
            window.load_tasks(fixture("checklist_page.json"))
        if inbox:
            window.load_inbox(fixture("inbox_list.json"))
            window.load_unread(fixture("inbox_unread.json"))
    if scans:
        window.scan_model.replace([fixture("scan_row.json")])
    window.data_changed.emit("project")


def texts(root):
    """Every user-visible text and tooltip under ``root`` (labels, buttons, tabs)."""
    found = []
    for widget in root.findChildren(QWidget):
        found.append(widget.toolTip())
        if isinstance(widget, (QLabel, QPushButton, QToolButton)):
            found.append(widget.text())
    return [text for text in found if text]


def load_url_rows(window, scan="/project/qa/scans/one.sqlite"):
    """Feed the legacy URL page a core-shaped page of saved URLs (the page shows data only after a scan was read)."""
    window.project_directory = window.project_directory or DIRECTORY
    window.selected_scan_path = scan
    window.load_urls({"rows": [
        {"url": "https://shop.example.test/", "status_code": 200, "content_type": "text/html", "indexability": "Indexable", "title": "Shop"},
        {"url": "https://shop.example.test/catalog/chair/", "status_code": 200, "content_type": "text/html", "indexability": "Indexable", "title": "Chair"},
        {"url": "https://shop.example.test/missing/", "status_code": 404, "content_type": "text/html", "indexability": "Non-indexable", "title": None},
    ], "offset": 0, "has_more": False}, scan)
    QApplication.processEvents()
