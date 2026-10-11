"""Small lifetime-safe worker bridge for integration dialogs."""
from PyQt5 import sip
from PyQt5.QtCore import QObject, QRunnable, Qt, pyqtSignal


class Signals(QObject):
    done = pyqtSignal(object)


class Job(QRunnable):
    def __init__(self, function, signals):
        super().__init__()
        self.function, self.signals = function, signals

    def run(self):
        try:
            result = self.function()
        except Exception as exc:
            result = exc
        try:
            self.signals.done.emit(result)
        except RuntimeError:
            pass


def run_background(pool, function, callback, owner):
    signals = Signals(owner)
    def deliver(value):
        if sip.isdeleted(owner) or sip.isdeleted(signals):
            return
        signals.deleteLater()
        callback(value)
    signals.done.connect(deliver, Qt.QueuedConnection)
    pool.start(Job(function, signals))
