"""Shared motion: short ease-out fades, a busy indicator and one reduce-motion switch.

Durations come from ``tokens.json`` (``motion.duration_ms``) and stay within 120-200 ms. With reduce-motion on
(the system setting or ``view.reduce_motion``) every helper applies its end state at once.
"""

from PyQt5.QtCore import (
    QAbstractAnimation,
    QEasingCurve,
    QEvent,
    QObject,
    QPropertyAnimation,
    QRectF,
    QSize,
    Qt,
    QVariantAnimation,
    pyqtSignal,
)
from PyQt5.QtGui import QColor, QPainter, QPen
from PyQt5.QtWidgets import QDialog, QGraphicsOpacityEffect, QHBoxLayout, QWidget

from .. import theming
from .presentation import theme_tokens

_reduced = False


def set_reduced(enabled):
    global _reduced
    _reduced = bool(enabled)


def reduced():
    return _reduced


def duration(scale=1.0):
    base = theme_tokens()["motion"]["duration_ms"]
    return max(120, min(200, round(base * scale)))


def fade_in(widget, scale=1.0):
    """Fade a widget from transparent to opaque; it is never left behind a graphics effect."""
    if _reduced or widget is None or widget.graphicsEffect() is not None:
        return
    effect = QGraphicsOpacityEffect(widget)
    effect.setOpacity(0.0)
    widget.setGraphicsEffect(effect)
    animation = QPropertyAnimation(effect, b"opacity", effect)
    animation.setDuration(duration(scale))
    animation.setStartValue(0.0)
    animation.setEndValue(1.0)
    animation.setEasingCurve(QEasingCurve.OutCubic)

    def finish():
        if widget.graphicsEffect() is effect:
            widget.setGraphicsEffect(None)

    animation.finished.connect(finish)
    animation.start()


def fade_stack(stack):
    """Cross-fade a QStackedWidget: the page that becomes current fades in over the old one's last frame."""
    stack.currentChanged.connect(lambda _index: fade_in(stack.currentWidget()))


def pulse(widget):
    """Looping opacity pulse for placeholders; static when motion is reduced."""
    if _reduced:
        return
    effect = QGraphicsOpacityEffect(widget)
    widget.setGraphicsEffect(effect)
    animation = QVariantAnimation(effect)
    animation.setDuration(duration(6))
    animation.setStartValue(1.0)
    animation.setKeyValueAt(0.5, 0.45)
    animation.setEndValue(1.0)
    animation.setLoopCount(-1)
    animation.setEasingCurve(QEasingCurve.InOutSine)
    animation.valueChanged.connect(effect.setOpacity)
    animation.start()


class BusyIndicator(QWidget):
    """Indeterminate ring; spins only while visible, shows a static arc under reduce-motion."""

    def __init__(self, size=20, parent=None):
        super().__init__(parent)
        self._size = size
        self._angle = 0
        self.setFixedSize(size, size)
        self.setAccessibleName("Loading")
        self._spin = QVariantAnimation(self)
        self._spin.setDuration(900)
        self._spin.setStartValue(0)
        self._spin.setEndValue(360)
        self._spin.setLoopCount(-1)
        self._spin.valueChanged.connect(self._turn)

    def sizeHint(self):
        return QSize(self._size, self._size)

    def _turn(self, value):
        self._angle = int(value)
        self.update()

    def showEvent(self, event):
        super().showEvent(event)
        if not _reduced:
            self._spin.start()

    def hideEvent(self, event):
        super().hideEvent(event)
        self._spin.stop()

    def paintEvent(self, _event):
        color = QColor(theming.roles()["primary"])
        painter = QPainter(self)
        painter.setRenderHint(QPainter.Antialiasing)
        width = max(2.0, self._size / 9)
        inset = width / 2 + 1
        rect = QRectF(inset, inset, self._size - 2 * inset, self._size - 2 * inset)
        track = QColor(color)
        track.setAlpha(50)
        painter.setPen(QPen(track, width))
        painter.drawEllipse(rect)
        painter.setPen(QPen(color, width, Qt.SolidLine, Qt.RoundCap))
        painter.drawArc(rect, -self._angle * 16, -100 * 16)


def status_row(label, *jobs):
    """A status label with a busy indicator in front of it that follows the given jobs' ``busyChanged``."""
    row = QWidget()
    layout = QHBoxLayout(row)
    layout.setContentsMargins(0, 0, 0, 0)
    spinner = BusyIndicator(14)
    spinner.hide()
    layout.addWidget(spinner)
    layout.addWidget(label, 1)
    running = set()

    def changed(job, busy):
        running.add(job) if busy else running.discard(job)
        spinner.setVisible(bool(running))

    for job in jobs:
        job.busyChanged.connect(lambda busy, job=job: changed(job, busy))
    return row


class Activity(QObject):
    """Counts running core jobs so one status-bar indicator can show that something is in progress."""

    changed = pyqtSignal(int)

    def __init__(self):
        super().__init__()
        self.count = 0

    def begin(self):
        self.count += 1
        self.changed.emit(self.count)

    def end(self):
        self.count = max(0, self.count - 1)
        self.changed.emit(self.count)


activity = Activity()


class _DialogFade(QObject):
    def eventFilter(self, watched, event):
        if event.type() == QEvent.Show and isinstance(watched, QDialog) and not _reduced:
            watched.setWindowOpacity(0.0)
            animation = QPropertyAnimation(watched, b"windowOpacity", watched)
            animation.setDuration(duration())
            animation.setStartValue(0.0)
            animation.setEndValue(1.0)
            animation.setEasingCurve(QEasingCurve.OutCubic)
            animation.start(QAbstractAnimation.DeleteWhenStopped)
        return False


def install(application):
    """Fade every dialog in when it opens."""
    fade = _DialogFade(application)
    application.installEventFilter(fade)
    return fade
