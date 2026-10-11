"""Design-v2 dialog frame: icon tile, title and subtitle, body, and a footer of hint · buttons.

One frame for every confirmation or form dialog. Footer buttons are added left to right, so the
order is the caller's: text or secondary actions first, the primary action last (it becomes the default).
"""

from __future__ import annotations

from PyQt5.QtCore import QSize, Qt
from PyQt5.QtWidgets import (
    QDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QSizePolicy,
    QToolButton,
    QVBoxLayout,
)

from .. import theming
from ..i18n import tr
from .icons import MaterialIconLabel, material_icon

# Tone -> badge pair from the design tokens (the same pairs as the status badges).
TONES = {
    "primary": "info",
    "agent": "goal",
    "warning": "warn",
    "error": "err",
    "neutral": "mut",
    "success": "ok",
}
BUTTON_ROLES = ("text", "secondary", "primary", "danger", "destructive")


class ModalDialog(QDialog):
    def __init__(self, title, subtitle="", icon="info", tone="primary", width=560, parent=None):
        super().__init__(parent)
        self.setObjectName("modalDialog")
        self.setWindowTitle(tr(title))
        self.setModal(True)
        self.setMinimumWidth(width)
        self.setMaximumWidth(width)

        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)

        header = QHBoxLayout()
        header.setContentsMargins(24, 18, 16, 14)
        header.setSpacing(12)
        header.addWidget(self._icon_tile(icon, tone), 0, Qt.AlignTop)
        texts = QVBoxLayout()
        texts.setSpacing(2)
        self.title = QLabel(tr(title))
        self.title.setProperty("text_style", "dialog")
        self.title.setWordWrap(True)
        self.subtitle = QLabel(tr(subtitle))
        self.subtitle.setProperty("text_style", "meta")
        self.subtitle.setWordWrap(True)
        self.subtitle.setVisible(bool(subtitle))
        texts.addWidget(self.title)
        texts.addWidget(self.subtitle)
        header.addLayout(texts, 1)
        close = QToolButton()
        close.setProperty("role", "icon")
        close.setIcon(material_icon("close"))
        close.setAccessibleName(tr("Закрыть"))
        close.setToolTip(tr("Закрыть"))
        close.setFocusPolicy(
            Qt.NoFocus
        )  # Esc closes too; the focus ring should land on the action, not the close icon
        close.clicked.connect(self.reject)
        header.addWidget(close, 0, Qt.AlignTop)
        root.addLayout(header)

        self.body = QVBoxLayout()
        self.body.setContentsMargins(24, 4, 24, 16)
        self.body.setSpacing(14)
        root.addLayout(self.body)

        footer = QFrame()
        footer.setObjectName("modalFooter")
        self.footer = QHBoxLayout(footer)
        self.footer.setContentsMargins(24, 14, 24, 14)
        self.footer.setSpacing(8)
        self.hint = QLabel()
        self.hint.setProperty("text_style", "meta")
        self.hint.setWordWrap(True)
        self.hint.setVisible(False)
        self.footer.addWidget(self.hint, 1)
        root.addWidget(footer)

    @staticmethod
    def _icon_tile(icon, tone):
        tile = QFrame()
        tile.setObjectName("modalIcon")
        tile.setProperty("tone", tone)
        tile.setFixedSize(40, 40)
        layout = QVBoxLayout(tile)
        layout.setContentsMargins(0, 0, 0, 0)
        kind = TONES.get(tone, "info")
        ink = theming.theme()["badges"][kind][1]
        layout.addWidget(MaterialIconLabel(icon, 20, color=ink), 0, Qt.AlignCenter)
        return tile

    def set_hint(self, text):
        """Left side of the footer: a short note, or the reason the primary action is unavailable."""
        self.hint.setText(tr(text))
        self.hint.setVisible(bool(text))

    def add_button(self, text, role="text", icon=None, on_click=None):
        """Append a footer button (right side). ``role`` is one of text, secondary, primary, danger, destructive."""
        if role not in BUTTON_ROLES:
            raise ValueError(f"unknown modal button role: {role}")
        button = QPushButton(tr(text))
        button.setSizePolicy(QSizePolicy.Maximum, QSizePolicy.Fixed)
        button.setProperty("size", "lg")
        if role != "secondary":
            button.setProperty("role", role)
        if icon:
            filled = role in ("primary", "destructive")
            button.setIcon(
                material_icon(icon, color="role:on_primary" if filled else "role:primary")
            )
            button.setIconSize(QSize(18, 18))
        if role == "primary":
            button.setDefault(True)
        if on_click is not None:
            button.clicked.connect(on_click)
        self.footer.addWidget(button)
        return button
