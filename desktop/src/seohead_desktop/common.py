"""Small helpers shared by the main-window modules."""

from __future__ import annotations

import hashlib
from pathlib import Path

from PyQt5.QtWidgets import QAbstractItemView, QHeaderView, QPlainTextEdit

from .ui.presentation import theme_tokens

ROOT = Path(__file__).resolve().parent
CONSUMER_ID = "desktop/gui"
PAGE_LIMIT = 50
TASK_PAGE_LIMIT = 100  # the core's largest checklist page; the Work list reads every page


def plain(text=""):
    view = QPlainTextEdit(text)
    view.setReadOnly(True)
    view.setMaximumBlockCount(1000)
    return view


def scan_request_key(project_uuid, scan_path, operation):
    digest = hashlib.sha256(str(scan_path).encode()).hexdigest()[:12]
    return f"{operation}:{project_uuid or 'unknown'}:{digest}"


def configure_table(table):
    table.setAlternatingRowColors(True)
    table.setSelectionBehavior(QAbstractItemView.SelectRows)
    table.setSelectionMode(QAbstractItemView.SingleSelection)
    table.setEditTriggers(QAbstractItemView.NoEditTriggers)
    table.verticalHeader().setVisible(False)
    table.verticalHeader().setDefaultSectionSize(theme_tokens()["table_row_height"])
    table.setShowGrid(False)
    table.setWordWrap(False)
    table.setHorizontalScrollMode(QAbstractItemView.ScrollPerPixel)
    table.setVerticalScrollMode(QAbstractItemView.ScrollPerPixel)
    table.horizontalHeader().setSectionResizeMode(QHeaderView.Interactive)
    table.horizontalHeader().setStretchLastSection(True)


def emits(kind):
    """Decorator for MainWindow loaders: after the legacy handler ran, tell the screens that ``kind`` of data changed."""

    def wrap(method):
        def run(self, *args, **kwargs):
            result = method(self, *args, **kwargs)
            self.data_changed.emit(kind)
            return result

        run.__name__ = method.__name__
        run.__doc__ = method.__doc__
        return run

    return wrap
