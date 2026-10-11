"""Round native Qt popup surfaces without replacing their input behavior."""

from PyQt5.QtCore import QEvent, QObject, QRectF, Qt
from PyQt5.QtGui import QPainterPath, QRegion
from PyQt5.QtWidgets import QComboBox, QFrame, QMenu, QStyledItemDelegate, QWidget


class RoundedPopupStyle(QObject):
    def __init__(self, application, radius=12):
        super().__init__(application)
        self.radius = radius
        self.applying = False
        application.installEventFilter(self)

    @staticmethod
    def is_popup(widget):
        return isinstance(widget, QMenu) or (
            isinstance(widget, QWidget)
            and widget.metaObject().className() == "QComboBoxPrivateContainer"
        )

    def eventFilter(self, watched, event):
        if self.applying or event.type() not in (QEvent.Polish, QEvent.Show, QEvent.Resize):
            return False
        if event.type() == QEvent.Polish and isinstance(watched, QComboBox):
            # The standard item-view delegate obeys rounded item QSS; the native
            # menu delegate paints its own square focus frame inside the popup.
            self.applying = True
            try:
                if watched.itemDelegate().metaObject().className() == "QComboMenuDelegate":
                    watched.setItemDelegate(QStyledItemDelegate(watched))
                    watched.view().setFrameShape(QFrame.NoFrame)
            finally:
                self.applying = False
            return False
        if not self.is_popup(watched):
            return False
        if event.type() in (QEvent.Polish, QEvent.Show, QEvent.Resize):
            self.applying = True
            try:
                if not watched.property("seoheadRoundedPopup"):
                    watched.setProperty("seoheadRoundedPopup", True)
                    watched.setAttribute(Qt.WA_TranslucentBackground)
                    watched.setAutoFillBackground(False)
                    if isinstance(watched, QFrame):
                        watched.setFrameShape(QFrame.NoFrame)
                path = QPainterPath()
                path.addRoundedRect(QRectF(watched.rect()), self.radius, self.radius)
                watched.setMask(QRegion(path.toFillPolygon().toPolygon()))
            finally:
                self.applying = False
        return False


def install_popup_style(application, radius):
    policy = getattr(application, "_seohead_popup_style", None)
    if policy is None:
        application._seohead_popup_style = RoundedPopupStyle(application, radius)
    else:
        policy.radius = radius
