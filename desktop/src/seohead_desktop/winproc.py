"""Windows: keep console children (the ``seohead`` CLI) from opening a console window per call.

A windowed Qt app that starts a console executable gets a visible console window for every
child unless the process is created with ``CREATE_NO_WINDOW``. The shell polls the core often, so
without this the screen fills with blank windows. POSIX needs nothing.
"""

from __future__ import annotations

import os
import subprocess

from PyQt5.QtCore import QProcess

CREATE_NO_WINDOW = 0x08000000
_patched = False


def hide_console_windows() -> None:
    """Make every ``subprocess.Popen`` of this app start without a console window."""
    global _patched
    if os.name != "nt" or _patched:
        return
    original = subprocess.Popen.__init__

    def init(self, *args, **kwargs):
        kwargs["creationflags"] = kwargs.get("creationflags", 0) | CREATE_NO_WINDOW
        original(self, *args, **kwargs)

    subprocess.Popen.__init__ = init
    _patched = True


def new_process(parent=None) -> QProcess:
    """A ``QProcess`` that starts console executables without a console window on Windows."""
    process = QProcess(parent)
    if os.name == "nt" and hasattr(process, "setCreateProcessArgumentsModifier"):

        def modify(arguments):
            arguments.flags = arguments.flags | CREATE_NO_WINDOW

        process.setCreateProcessArgumentsModifier(modify)
    return process
