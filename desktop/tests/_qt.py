"""Shared Qt test helper: drop top-level windows leaked by earlier tests.

An application-wide theme change re-polishes every live widget, so leaked windows make each later
theme test slower (minutes in total). Sweeping them first keeps the cost proportional to the test.
"""

from PyQt5.QtCore import QCoreApplication, QEvent
from PyQt5.QtWidgets import QApplication


def sweep_widgets():
    from seohead_desktop import i18n

    i18n.set_language("ru")  # Isolate legacy translation fixtures from English application windows.
    app = QApplication.instance()
    if app is None:
        return
    app.processEvents()  # let zero-delay timers that reference windows run before deleting them
    for widget in QApplication.topLevelWidgets():
        widget.close()
        widget.deleteLater()
    QCoreApplication.sendPostedEvents(None, QEvent.DeferredDelete)
    app.processEvents()


def ui(text):
    """Expected UI copy in the fixture's language; application windows always choose English."""
    from seohead_desktop import i18n

    return i18n.tr(text)
