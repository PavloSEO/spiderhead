"""The one place that creates the QApplication and ends Qt processes.

Python's interpreter finalisation tears down PyQt objects (QtCore cleanup_on_exit, dealloc_QApplication) and crashes with
SIGSEGV on exit. Every process that creates the application therefore leaves through ``exit_now`` (or the hooks installed
by ``app()``), which flushes the streams and calls ``os._exit`` without finalisation.
"""

import atexit
import os
import sys

from PyQt5.QtWidgets import QApplication

_app = None
_failed = False
_hooks_installed = False


def exit_now(code=0):
    """Flush the standard streams and leave the process immediately (no interpreter finalisation)."""
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.flush()
        except (OSError, ValueError, AttributeError):
            pass
    os._exit(code if isinstance(code, int) else 0)


def _code_of(status):
    if status is None:
        return 0
    if isinstance(status, int):
        return status
    print(status, file=sys.stderr)
    return 1


def _install_hooks():
    """Make ``sys.exit`` / a normal end of the script / an uncaught exception leave through ``exit_now``."""
    global _hooks_installed
    if _hooks_installed:
        return
    _hooks_installed = True
    original_hook = sys.excepthook

    def excepthook(kind, value, trace):
        global _failed
        _failed = True
        original_hook(kind, value, trace)

    sys.excepthook = excepthook
    sys.exit = lambda status=None: exit_now(_code_of(status))  # unittest.main(), argparse, scripts
    atexit.register(lambda: exit_now(1 if _failed else 0))


def app(argv=None):
    """The process-wide QApplication: the existing instance or a single new one (kept referenced for the whole run)."""
    global _app
    instance = QApplication.instance()
    if instance is None:
        _app = instance = QApplication(list(argv) if argv is not None else [])
    _install_hooks()
    return instance
