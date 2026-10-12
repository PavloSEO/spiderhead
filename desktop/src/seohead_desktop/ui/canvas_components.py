"""Reusable native components for the shared application canvas."""

from PyQt5.QtCore import QRectF, QSize, Qt, pyqtSignal
from PyQt5.QtGui import QColor, QFont, QFontMetrics, QPainter, QPen
from PyQt5.QtWidgets import (
    QBoxLayout,
    QFrame,
    QHBoxLayout,
    QLabel,
    QSizePolicy,
    QTabBar,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import theming
from ..i18n import tr
from .canvas_tokens import metric
from .controls import Segmented, Switch
from .icons import MaterialIconLabel, material_icon


def polish(root):
    """Resolve component selectors after native editors have their final parents."""
    for widget in (root, *root.findChildren(QWidget)):
        widget.style().unpolish(widget)
        widget.style().polish(widget)
        widget.updateGeometry()


class CanvasField(QFrame):
    """A leading symbol and an existing native editor in a 44 px field."""

    def __init__(self, editor, icon, color="role:text_2"):
        super().__init__()
        self.setProperty("canvas_component", "field")
        self.setFixedHeight(metric("field_height"))
        box = QHBoxLayout(self)
        box.setContentsMargins(12, 0, 12, 0)
        box.setSpacing(10)
        box.addWidget(MaterialIconLabel(icon, 20, color=color))
        editor.setProperty("canvas_editor", True)
        editor.setMinimumHeight(0)
        box.addWidget(editor, 1)
        polish(self)


class CanvasSegmented(Segmented):
    """Outlined pill with an explicit selected check, retaining native input."""

    def __init__(self, options, value, accessible_name):
        super().__init__(options, value, accessible_name)
        self.setProperty("canvas_component", "segment")
        self.setFixedHeight(metric("segment_height"))
        self.layout().setContentsMargins(0, 0, 0, 0)
        self.layout().setSpacing(0)
        for index, button in enumerate(self._buttons.values()):
            button.setProperty(
                "schedule_segment",
                "first" if index == 0 else "last" if index == len(self._buttons) - 1 else "middle",
            )
            button.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
            button.setIconSize(QSize(16, 16))
            button.toggled.connect(lambda _selected: self.update_widths())
            button.toggled.connect(
                lambda selected, b=button: b.setIcon(
                    material_icon("check", "role:canvas_segment_fg")
                    if selected
                    else material_icon("")
                )
            )
        self.setValue(value)
        polish(self)
        self.update_widths()

    def update_widths(self):
        font = QFont("Roboto")
        font.setPixelSize(metric("day_font"))
        font.setWeight(QFont.Medium)
        fm = QFontMetrics(font)
        for button in self._buttons.values():
            button.setFixedWidth(
                fm.horizontalAdvance(button.text())
                + 2 * metric("segment_padding")
                + (22 if button.isChecked() else 0)
                + 1
            )

    def setValue(self, value):
        super().setValue(value)
        for button in self._buttons.values():
            button.setIcon(
                material_icon("check", "role:canvas_segment_fg")
                if button.isChecked()
                else material_icon("")
            )


class CanvasPreset(QToolButton):
    """32 px preset chip without the platform tool-button padding."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setProperty("canvas_component", "preset")

    def sizeHint(self):
        font = QFont("Roboto")
        font.setPixelSize(metric("day_font"))
        font.setWeight(QFont.Medium)
        width = QFontMetrics(font).horizontalAdvance(self.text()) + 26
        return QSize(width + (22 if not self.icon().isNull() else 0), metric("preset_height"))


class Columns(QWidget):
    """Two panels at wide widths, the same panels stacked at compact widths."""

    def __init__(self, *panels, ratio=(31, 20)):
        super().__init__()
        self.setObjectName("scheduleColumns")
        self.box = QBoxLayout(QBoxLayout.LeftToRight, self)
        self.box.setContentsMargins(0, 0, 0, 0)
        self.box.setSpacing(metric("gap"))
        for index, panel in enumerate(panels):
            panel.setSizePolicy(QSizePolicy.Ignored, QSizePolicy.Preferred)
            self.box.addWidget(panel, ratio[index], Qt.AlignTop)

    def resizeEvent(self, event):
        self.box.setDirection(
            QBoxLayout.TopToBottom if self.width() < metric("compact") else QBoxLayout.LeftToRight
        )
        super().resizeEvent(event)


class CalendarDay(QToolButton):
    """Native checkable day with the circular calendar treatment."""

    def __init__(self, text, parent=None):
        super().__init__(parent)
        self.setText(text)
        self.setCheckable(True)
        self.setProperty("canvas_component", "day")
        self.setFixedSize(metric("day_size"), metric("day_size"))
        self.setCursor(Qt.PointingHandCursor)

    def paintEvent(self, event):
        painter = QPainter(self)
        painter.setRenderHint(QPainter.Antialiasing)
        roles = theming.roles()
        selected = self.isChecked()
        painter.setPen(QPen(QColor(roles["primary"] if selected else roles["outline"]), 1))
        painter.setBrush(
            QColor(
                roles["primary"]
                if selected
                else roles["raised"]
                if self.property("weekend")
                else roles["base"]
            )
        )
        painter.drawEllipse(QRectF(0.5, 0.5, 39, 39))
        painter.setPen(QColor(roles["on_primary"] if selected else roles["text_2"]))
        painter.drawText(self.rect(), Qt.AlignCenter, self.text())
        if self.hasFocus():
            painter.setPen(QPen(QColor(roles["ring"]), 2))
            painter.setBrush(Qt.NoBrush)
            painter.drawEllipse(QRectF(3, 3, 34, 34))


class WindowPreview(QWidget):
    """Draw the selected clock interval; calendar decisions remain in the core."""

    clicked = pyqtSignal()

    def __init__(self, start, end):
        super().__init__()
        self.start, self.end = start, end
        self.setMinimumHeight(metric("timeline_height") + metric("timeline_ticks"))
        self.setCursor(Qt.PointingHandCursor)
        self.setFocusPolicy(Qt.StrongFocus)
        self.setAccessibleName(tr("Change night window"))
        for edit in (start, end):
            edit.timeChanged.connect(self.update)

    def mouseReleaseEvent(self, event):
        if event.button() == Qt.LeftButton:
            self.clicked.emit()
        super().mouseReleaseEvent(event)

    def keyPressEvent(self, event):
        if event.key() in {Qt.Key_Space, Qt.Key_Return}:
            self.clicked.emit()
        else:
            super().keyPressEvent(event)

    def paintEvent(self, event):
        painter = QPainter(self)
        painter.setRenderHint(QPainter.Antialiasing)
        roles = theming.roles()
        width = self.width()
        painter.setPen(QPen(QColor(roles["divider"]), 1))
        painter.setBrush(QColor(roles["raised"]))
        painter.drawRoundedRect(
            QRectF(0, 0, width, metric("timeline_height")),
            metric("timeline_radius"),
            metric("timeline_radius"),
        )
        start, end = (
            edit.time().msecsSinceStartOfDay() / 86400000 for edit in (self.start, self.end)
        )
        painter.setPen(Qt.NoPen)
        painter.setBrush(QColor(roles["primary"]))
        if end > start:
            painter.drawRoundedRect(QRectF(start * width, 7, (end - start) * width, 32), 7, 7)
        elif end < start:
            painter.drawRect(QRectF(start * width, 7, (1 - start) * width, 30))
            painter.drawRect(QRectF(0, 7, end * width, 30))
        painter.setPen(QColor(roles["canvas_segment_fg"]))
        font = QFont("Roboto")
        font.setPixelSize(13)
        font.setWeight(QFont.Medium)
        painter.setFont(font)
        text = f"{self.start.time().toString('HH:mm')} – {self.end.time().toString('HH:mm')}"
        label_x = min(
            width - painter.fontMetrics().horizontalAdvance(text) - 10, max(12, end * width + 12)
        )
        painter.drawText(
            QRectF(label_x, 0, width - label_x, 46), Qt.AlignVCenter | Qt.AlignLeft, text
        )
        painter.setPen(QColor(roles["text_3"]))
        font = QFont("Roboto Mono")
        font.setPixelSize(10)
        painter.setFont(font)
        for hour in (0, 6, 12, 18, 24):
            x = min(width - 16, max(0, hour / 24 * width - 8))
            painter.drawText(QRectF(x, 48, 20, 18), Qt.AlignLeft, f"{hour:02d}")


def card(title, icon=None, tone="info", subtitle=None):
    frame = QFrame()
    frame.setProperty("card", "panel")
    frame.setProperty("canvas_component", "panel")
    layout = QVBoxLayout(frame)
    layout.setContentsMargins(
        metric("panel_padding_x"),
        metric("panel_padding_y"),
        metric("panel_padding_x"),
        metric("panel_padding_y"),
    )
    layout.setSpacing(metric("panel_gap"))
    heading = QHBoxLayout()
    heading.setSpacing(12)
    if icon:
        tile = QFrame()
        tile.setProperty("schedule_icon", tone)
        tile.setFixedSize(metric("day_size"), metric("day_size"))
        tile_layout = QVBoxLayout(tile)
        tile_layout.setContentsMargins(0, 0, 0, 0)
        ink = {
            "info": "on_selected",
            "warning": "canvas_icon_amber_fg",
            "ok": "canvas_icon_green_fg",
        }[tone]
        tile_layout.addWidget(
            MaterialIconLabel(icon, 22, color="role:" + ink, filled=True), 0, Qt.AlignCenter
        )
        heading.addWidget(tile)
    texts = QVBoxLayout()
    texts.setContentsMargins(0, 0, 0, 0)
    texts.setSpacing(0)
    caption = QLabel(tr(title))
    caption.setProperty("text_style", "section")
    caption.setMinimumHeight(26)
    texts.addWidget(caption)
    if subtitle is not None:
        subtitle.setMinimumHeight(20)
        texts.addWidget(subtitle)
    heading.addLayout(texts, 1)
    layout.addLayout(heading)
    frame.heading = heading
    return frame, layout


class CanvasSwitch(Switch):
    """44 by 26 switch using the shared component tokens and native checked state."""

    def __init__(self, accessible_name, checked=False, parent=None):
        super().__init__(accessible_name, checked, parent)
        self.setFixedSize(metric("switch_width"), metric("switch_height"))

    def sizeHint(self):
        return QSize(metric("switch_width"), metric("switch_height"))

    def paintEvent(self, event):
        p = QPainter(self)
        p.setRenderHint(QPainter.Antialiasing)
        r = theming.roles()
        w, h = self.width(), self.height()
        p.setPen(Qt.NoPen)
        if self.isChecked():
            p.setBrush(QColor(r["primary"]))
            p.drawRoundedRect(QRectF(0, 0, w, h), h / 2, h / 2)
            p.setBrush(QColor(r["on_primary"]))
            thumb = metric("switch_thumb")
            p.drawEllipse(QRectF(w - thumb - 3, 3, thumb, thumb))
        else:
            p.setPen(QPen(QColor(r["outline_hover"]), 2))
            p.setBrush(QColor(r["canvas_switch_off"]))
            p.drawRoundedRect(QRectF(1, 1, w - 2, h - 2), h / 2, h / 2)
            p.setPen(Qt.NoPen)
            p.setBrush(QColor(r["outline_hover"]))
            thumb = metric("switch_thumb_off")
            p.drawEllipse(QRectF(4, (h - thumb) / 2, thumb, thumb))
        if self.hasFocus():
            p.setPen(QPen(QColor(r["ring"]), 2))
            p.setBrush(Qt.NoBrush)
            p.drawRoundedRect(QRectF(1, 1, w - 2, h - 2), h / 2, h / 2)


class CodeCard(QFrame):
    """Plain retained text in the shared dark code surface."""

    def __init__(self, text, caption=None, parent=None, variant="cli"):
        super().__init__(parent)
        self.setProperty("canvas_component", "code")
        self.setProperty("code_variant", variant)
        box = QVBoxLayout(self)
        x, y = (
            (14, 12) if variant == "agent" else (metric("code_padding_x"), metric("code_padding_y"))
        )
        box.setContentsMargins(x, y, x, y)
        box.setSpacing(6 if variant == "agent" else 0)
        if caption:
            label = QLabel(caption)
            label.setProperty("code_meta", True)
            label.setFixedHeight(18 if variant == "agent" else 21)
            box.addWidget(label)
        self.text = QLabel(text)
        self.text.setTextFormat(Qt.PlainText)
        self.text.setWordWrap(True)
        self.text.setTextInteractionFlags(Qt.TextSelectableByMouse)
        if variant == "cli" and "\n" in text:
            self.text.hide()
            lines = QVBoxLayout()
            lines.setContentsMargins(0, 0, 0, 0)
            lines.setSpacing(0)
            for line in text.splitlines():
                label = QLabel(line)
                label.setTextFormat(Qt.PlainText)
                label.setFixedHeight(21)
                label.setTextInteractionFlags(Qt.TextSelectableByMouse)
                lines.addWidget(label)
            box.addLayout(lines)
        else:
            self.text.setMinimumHeight(20)
            box.addWidget(self.text)


class ModeCard(QToolButton):
    """Native exclusive choice with a tiled icon, title and description."""

    def __init__(self, title, description, icon, tone="info", parent=None):
        super().__init__(parent)
        self.setCheckable(True)
        self.setProperty("canvas_component", "mode")
        self.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
        self.setAccessibleName(title)
        self.box = QHBoxLayout(self)
        self.box.setContentsMargins(16, 16, 16, 16)
        self.box.setSpacing(12)
        tile = QFrame()
        self.tile = tile
        tile.setProperty("schedule_icon", tone)
        tile.setFixedSize(metric("icon_box"), metric("icon_box"))
        inside = QVBoxLayout(tile)
        inside.setContentsMargins(0, 0, 0, 0)
        self.tone = tone
        self.icon_label = MaterialIconLabel(
            icon, metric("icon_glyph"), color="role:text", filled=True
        )
        inside.addWidget(self.icon_label, 0, Qt.AlignCenter)
        self.box.addWidget(tile)
        texts = QVBoxLayout()
        texts.setSpacing(0)
        self.title = QLabel(title)
        self.title.setProperty("text_style", "section")
        self.title.setFixedHeight(20)
        self.description = QLabel(description)
        self.description.setProperty("text_style", "meta")
        self.description.setWordWrap(True)
        self.description.setFixedHeight(14)
        texts.addWidget(self.title)
        texts.addWidget(self.description)
        self.box.addLayout(texts, 1)
        for widget in (tile, self.title, self.description):
            widget.setAttribute(Qt.WA_TransparentForMouseEvents)
        self.toggled.connect(self._selection_changed)

    def _selection_changed(self, selected):
        self.update_margins()
        self.tile.setProperty("selected", selected)
        self.tile.style().unpolish(self.tile)
        self.tile.style().polish(self.tile)
        ink = {
            "info": "on_selected",
            "warning": "canvas_icon_amber_fg",
            "ok": "canvas_icon_green_fg",
        }[self.tone]
        self.icon_label.set_material_icon(
            self.icon_label.name, "role:on_primary" if selected else "role:" + ink
        )

    def update_margins(self):
        compact = bool(self.property("compact"))
        self.box.setContentsMargins(15, 13, 15, 13) if compact else self.box.setContentsMargins(
            17, 17, 17, 17
        )
        self.box.setAlignment(Qt.AlignTop)


class ChipFlow(QWidget):
    """Wrap native preset buttons by their measured widths."""

    def __init__(self, buttons, parent=None):
        super().__init__(parent)
        self.buttons = list(buttons)
        for button in self.buttons:
            button.setParent(self)
        policy = QSizePolicy(QSizePolicy.Preferred, QSizePolicy.Preferred)
        policy.setHeightForWidth(True)
        self.setSizePolicy(policy)
        self._arrange()

    def _positions(self, width):
        x = y = 0
        for button in self.buttons:
            size = button.sizeHint()
            if x and x + size.width() > width:
                y += metric("preset_height") + 8
                x = 0
            yield button, x, y, size.width()
            x += size.width() + 8

    def heightForWidth(self, width):
        return max(
            (y + metric("preset_height") for _, _, y, _ in self._positions(width)), default=0
        )

    def sizeHint(self):
        return QSize(0, self.heightForWidth(self.width()))

    def minimumSizeHint(self):
        return QSize(0, metric("preset_height"))

    def _arrange(self):
        for button, x, y, width in self._positions(self.width()):
            button.setGeometry(x, y, width, metric("preset_height"))
            button.show()
        self.setFixedHeight(self.heightForWidth(self.width()))

    def resizeEvent(self, event):
        self._arrange()
        super().resizeEvent(event)


class CanvasTabs(QTabBar):
    """Native tab input with the canvas icon/text spacing and underline."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.names = []
        self.setExpanding(False)
        self.setDrawBase(False)
        self.setFixedHeight(metric("subtabs") + 1)

    def add_canvas_tab(self, name, text):
        self.names.append(name)
        return self.addTab(text)

    def tabSizeHint(self, index):
        font = QFont("Roboto")
        font.setPixelSize(14)
        font.setWeight(QFont.Medium)
        text = QFontMetrics(font).horizontalAdvance(self.tabText(index))
        return QSize(
            24
            + metric("icon_glyph")
            + 24
            + (8 + text if text else 0)
            + 4
            + (16 if index == 0 else 0),
            metric("subtabs"),
        )

    def paintEvent(self, event):
        from .icons import material_icon

        painter = QPainter(self)
        r = theming.roles()
        painter.fillRect(self.rect(), QColor(r["base"]))
        painter.setPen(QColor(r["divider"]))
        painter.drawLine(0, self.height() - 1, self.width(), self.height() - 1)
        font = QFont("Roboto")
        font.setPixelSize(14)
        painter.setFont(font)
        for index, name in enumerate(self.names):
            if not self.isTabVisible(index):
                continue
            rect = self.tabRect(index)
            if index == 0:
                rect = rect.adjusted(16, 0, 0, 0)
            selected = index == self.currentIndex()
            ink = "primary" if selected else "text_2"
            glyph = metric("icon_glyph")
            font.setWeight(QFont.Medium if selected else QFont.Normal)
            painter.setFont(font)
            material_icon(name, "role:" + ink).paint(
                painter,
                rect.adjusted(
                    24,
                    (rect.height() - glyph) // 2,
                    -rect.width() + 24 + glyph,
                    -(rect.height() - glyph) // 2,
                ),
            )
            painter.setPen(QColor(r[ink]))
            painter.drawText(
                rect.adjusted(12 + glyph + 24 + 8, 0, -12, 0),
                Qt.AlignVCenter | Qt.AlignLeft,
                self.tabText(index),
            )
            if selected:
                painter.fillRect(
                    rect.x(), self.height() - 2, rect.width() - 4, 2, QColor(r["primary"])
                )
