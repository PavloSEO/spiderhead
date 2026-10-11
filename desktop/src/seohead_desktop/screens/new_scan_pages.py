"""Widgets and the eight pages of the scan settings window (ScSpeed … ScProfiles).

A page binds controls to a ScanDraft. A field the core has no setting for is drawn disabled with its reason and the
number of the core issue it waits for; it holds no value and never reaches the command.
"""

from __future__ import annotations

import re
import shutil
from pathlib import Path

from PyQt5.QtCore import QAbstractTableModel, QPoint, QPointF, QRect, QRectF, QSize, Qt, pyqtSignal
from PyQt5.QtGui import QColor, QPainter
from PyQt5.QtWidgets import (
    QCheckBox,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLayout,
    QLineEdit,
    QPlainTextEdit,
    QPushButton,
    QSizePolicy,
    QTableView,
    QTableWidget,
    QTableWidgetItem,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import theming
from ..i18n import tr, trf
from ..ui.controls import Note, Segmented, SettingRow, Switch, polish
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import style_table, waiting_badge
from ..ui.presentation import ElidedLabel
from .new_scan_draft import (
    FILE_GROUPS,
    GOOGLEBOT_UA,
    ISSUE_EXTRACT,
    ISSUE_OTHER,
    ISSUE_PROFILES,
    ISSUE_RENDER,
    ISSUE_SETTINGS,
    MOBILE_UA,
    RPS_SAFE,
    YANDEX_UA,
    grouped,
)

NOT_DECLARED = "Ядро не объявляет эту настройку"


def clear_segmented(segmented):
    group = segmented._group
    group.setExclusive(False)
    for button in segmented._buttons.values():
        button.setChecked(False)
    group.setExclusive(True)


class FlowLayout(QLayout):
    """Left-to-right layout that wraps onto new rows, so a row of badges never forces the dialog wider."""

    def __init__(self, parent=None, spacing=8):
        super().__init__(parent)
        self._items = []
        self._gap = spacing
        self.setContentsMargins(0, 0, 0, 0)

    def addItem(self, item):
        self._items.append(item)

    def count(self):
        return len(self._items)

    def itemAt(self, index):
        return self._items[index] if 0 <= index < len(self._items) else None

    def takeAt(self, index):
        return self._items.pop(index) if 0 <= index < len(self._items) else None

    def hasHeightForWidth(self):
        return True

    def heightForWidth(self, width):
        return self._arrange(QRect(0, 0, width, 0), False)

    def setGeometry(self, rect):
        super().setGeometry(rect)
        self._arrange(rect, True)

    def sizeHint(self):
        return self.minimumSize()

    def minimumSize(self):
        size = QSize()
        for item in self._items:
            size = size.expandedTo(item.minimumSize())
        margins = self.contentsMargins()
        return size + QSize(margins.left() + margins.right(), margins.top() + margins.bottom())

    def _arrange(self, rect, apply):
        x, y, row = rect.x(), rect.y(), 0
        for item in self._items:
            if item.widget() is not None and item.widget().isHidden():
                continue
            hint = item.sizeHint()
            if x + hint.width() > rect.right() + 1 and row:
                x, y, row = rect.x(), y + row + self._gap, 0
            if apply:
                item.setGeometry(QRect(QPoint(x, y), hint))
            x += hint.width() + self._gap
            row = max(row, hint.height())
        return y + row - rect.y()


def flow(*widgets, spacing=8):
    box = QWidget()
    layout = FlowLayout(box, spacing)
    for widget in widgets:
        layout.addWidget(widget)
    return box


class ChoiceCard(QToolButton):
    """Choice card (design «Что сканировать»): icon, name, one line of text; a card button has no layout of its own."""

    def __init__(self, key, glyph, name, text, object_name="", tag="", compact=False):
        super().__init__()
        self.key = key
        self.compact = compact
        self.setObjectName(object_name)
        self.setCheckable(True)
        self.setAutoExclusive(True)
        self.setProperty("card", "choice")
        self.setAccessibleName(tr(name))
        self.setMinimumHeight(40 if compact else 86)
        self.setCursor(Qt.PointingHandCursor)
        if compact:
            # one 40 px row: icon and name; the sentence moves into the tooltip (design ScanDialog «.src»)
            self.setProperty("compact", True)
            self.setToolTip(tr(text))
            row_layout = QHBoxLayout(self)
            row_layout.setContentsMargins(10, 0, 10, 0)
            row_layout.setSpacing(8)
            if glyph:
                row_layout.addWidget(MaterialIconLabel(glyph, 18, color="role:primary"))
            title = QLabel(tr(name))
            title.setProperty("text_style", "control")
            row_layout.addWidget(title, 1)
            self.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
            self.setFixedHeight(40)
            for child in self.findChildren(QWidget):
                child.setAttribute(Qt.WA_TransparentForMouseEvents)
            return
        layout = QVBoxLayout(self)
        layout.setContentsMargins(12, 12, 12, 12)
        layout.setSpacing(4)
        head = QHBoxLayout()
        head.setSpacing(8)
        if glyph:
            head.addWidget(MaterialIconLabel(glyph, 20, color="role:primary"))
        title = QLabel(tr(name))
        title.setProperty("text_style", "control")
        head.addWidget(title, 1)
        layout.addLayout(head)
        body = QLabel(tr(text))
        body.setProperty("text_style", "meta")
        body.setWordWrap(True)
        layout.addWidget(body)
        if tag:
            badge = QLabel(tr(tag))
            badge.setProperty("badge", "mut")
            layout.addWidget(badge, 0, Qt.AlignLeft)
        layout.addStretch(1)
        self.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Preferred)
        for child in self.findChildren(QWidget):
            child.setAttribute(Qt.WA_TransparentForMouseEvents)

    def sizeHint(self):
        hint = self.layout().sizeHint()
        if self.compact:
            return QSize(max(hint.width(), 96), 40)
        return QSize(max(hint.width(), 120), max(hint.height(), 86))

    def minimumSizeHint(self):
        return QSize(96, self.sizeHint().height())

    def hasHeightForWidth(self):
        return not self.compact and self.layout().hasHeightForWidth()

    def heightForWidth(self, width):
        return 40 if self.compact else max(self.layout().heightForWidth(width), 86)


class HelpIcon(MaterialIconLabel):
    """The «?» of the dense layout: the explanation lives in the tooltip, not in a line of text (design `.q`)."""

    def __init__(self, text, parent=None):
        super().__init__("help", 16, parent, color="role:text_muted")
        self.setAttribute(Qt.WA_TransparentForMouseEvents, False)
        self.setCursor(Qt.WhatsThisCursor)
        self.setToolTip(text)
        self.setAccessibleName(tr("Подсказка"))


class Stepper(QFrame):
    """[−] value [+] in one 32 px field (design `.stp`); ``edit`` is the real line edit bound to the draft."""

    def __init__(self, edit, on_minus, on_plus, parent=None):
        super().__init__(parent)
        self.setProperty("stepper", True)
        self.edit = edit
        edit.setAlignment(Qt.AlignCenter)
        edit.setMinimumWidth(0)
        edit.setMaximumWidth(16777215)
        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        self.minus, self.plus = QToolButton(), QToolButton()
        for button, glyph, name, step in (
            (self.minus, "remove", "Меньше", on_minus),
            (self.plus, "add", "Больше", on_plus),
        ):
            button.setProperty("stepper_button", True)
            button.setIcon(material_icon(glyph))
            button.setAccessibleName(tr(name))
            button.setToolTip(tr(name))
            button.setFocusPolicy(Qt.NoFocus)
            button.clicked.connect(lambda _c=False, f=step: f())
        layout.addWidget(self.minus)
        layout.addWidget(edit, 1)
        layout.addWidget(self.plus)
        self.setFixedHeight(32)
        self.setMinimumWidth(110)

    def set_invalid(self, invalid):
        self.setProperty("invalid", bool(invalid))
        polish(self)

    def set_enabled_value(self, enabled):
        self.edit.setEnabled(enabled)
        self.minus.setEnabled(enabled)
        self.plus.setEnabled(enabled)
        self.setProperty("off", not enabled)
        polish(self)


class FormRow(QWidget):
    """One dense form row: caption column (132) and the control (design `.fr`); an error line appears under the control."""

    LABEL = 132

    def __init__(
        self,
        title,
        control,
        help_text="",
        label_widget=None,
        hint="",
        field=None,
        label_width=None,
        parent=None,
    ):
        super().__init__(parent)
        self.control = control
        self.edit = field or control
        self.hint_text = hint
        grid = QGridLayout(self)
        grid.setContentsMargins(0, 0, 0, 0)
        grid.setHorizontalSpacing(10)
        grid.setVerticalSpacing(2)
        head = QWidget()
        head_layout = QHBoxLayout(head)
        head_layout.setContentsMargins(0, 0, 0, 0)
        head_layout.setSpacing(4)
        if label_widget is not None:
            head_layout.addWidget(label_widget)
        self.caption = QLabel(title)
        self.caption.setProperty("text_style", "control")
        head_layout.addWidget(self.caption)
        self.help = HelpIcon(help_text) if help_text else None
        if self.help is not None:
            head_layout.addWidget(self.help)
        head_layout.addStretch(1)
        self.label_width = label_width or self.LABEL
        head.setFixedWidth(self.label_width)
        head.setMinimumHeight(32)
        self.head = head
        grid.addWidget(head, 0, 0)
        grid.addWidget(control, 0, 1)
        grid.setColumnStretch(1, 1)
        self.note = QLabel(hint)
        self.note.setProperty("text_style", "meta")
        self.note.setWordWrap(True)
        self.note.setVisible(bool(hint))
        grid.addWidget(self.note, 1, 1)
        self.setMinimumHeight(32)

    def set_error(self, message):
        target = self.control if isinstance(self.control, Stepper) else self.edit
        if isinstance(self.control, Stepper):
            self.control.set_invalid(bool(message))
        self.edit.setProperty("invalid", bool(message))
        polish(self.edit)
        self.note.setProperty("field_error", bool(message))
        polish(self.note)
        self.note.setText(message or self.hint_text)
        self.note.setVisible(bool(message or self.hint_text))
        target.setToolTip(message or "")

    def set_narrow(self, narrow):
        """In a narrow dialog the caption goes above the control instead of beside it."""
        grid = self.layout()
        grid.removeWidget(self.head)
        grid.removeWidget(self.control)
        grid.removeWidget(self.note)
        if narrow:
            self.head.setMinimumWidth(0)
            self.head.setMaximumWidth(16777215)
            self.head.setMinimumHeight(20)
            grid.addWidget(self.head, 0, 0)
            grid.addWidget(self.control, 1, 0)
            grid.addWidget(self.note, 2, 0)
        else:
            self.head.setFixedWidth(self.label_width)
            self.head.setMinimumHeight(32)
            grid.addWidget(self.head, 0, 0)
            grid.addWidget(self.control, 0, 1)
            grid.addWidget(self.note, 1, 1)
        grid.setColumnStretch(0, 1 if narrow else 0)
        grid.setColumnStretch(1, 0 if narrow else 1)


class FieldBox(QWidget):
    """Caption, one input and a line under it that is a hint, or the error in red when the value is not accepted."""

    def __init__(self, title, edit, hint="", field=None, parent=None, trailing=None):
        super().__init__(parent)
        self.edit = field or edit
        self.hint_text = hint
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(4)
        self.caption = QLabel(title)
        self.caption.setProperty("text_style", "control")
        if trailing is None:
            layout.addWidget(self.caption)
        else:  # a switch at the right end of the caption line
            head = QHBoxLayout()
            head.setContentsMargins(0, 0, 0, 0)
            head.addWidget(self.caption, 1)
            head.addWidget(trailing)
            layout.addLayout(head)
        layout.addWidget(edit)
        self.note = QLabel(hint)
        self.note.setProperty("text_style", "meta")
        self.note.setWordWrap(True)
        self.note.setVisible(bool(hint))
        layout.addWidget(self.note)

    def set_error(self, message):
        self.edit.setProperty("invalid", bool(message))
        polish(self.edit)
        self.note.setProperty("field_error", bool(message))
        polish(self.note)
        self.note.setText(message or self.hint_text)
        self.note.setVisible(bool(message or self.hint_text))
        self.edit.setToolTip(message or "")


def number_edit(draft, key, accessible, width=96, name=None):
    edit = QLineEdit()
    edit.setObjectName(name or f"scan_{key}")
    edit.setAccessibleName(accessible)
    edit.setFixedWidth(width) if width else None
    edit.setAlignment(Qt.AlignRight | Qt.AlignVCenter)
    edit.setText(draft.texts.get(key, ""))
    edit.textEdited.connect(lambda text: draft.set_text(key, text))

    def sync(problems=None):
        text = draft.texts.get(key, "")
        if (edit.text() != text and not edit.hasFocus()) or (
            edit.text() != text and edit.hasFocus() and key not in draft.errors
        ):
            edit.setText(text)

    return edit, sync


class Page(QWidget):
    """One settings page: title, hint and rows; ``sync`` refreshes every bound control from the draft."""

    def __init__(self, title, hint, parent=None):
        super().__init__(parent)
        self.binders = []
        self.layouts = []
        self.layout_ = QVBoxLayout(self)
        self.layout_.setContentsMargins(0, 0, 0, 0)
        self.layout_.setSpacing(0)
        head = QLabel(title)
        head.setProperty("text_style", "title")
        sub = QLabel(hint)
        sub.setProperty("text_style", "meta")
        sub.setWordWrap(True)
        self.layout_.addWidget(head)
        self.layout_.addWidget(sub)
        self.layout_.addSpacing(8)
        self.body = QVBoxLayout()
        self.body.setSpacing(0)
        self.layout_.addLayout(self.body)
        self.layout_.addStretch(1)

    def add(self, widget):
        self.body.addWidget(widget)
        return widget

    def caption(self, text):
        label = QLabel(text)
        label.setProperty("text_style", "overline")
        label.setContentsMargins(0, 18, 0, 6)
        self.body.addWidget(label)
        return label

    def bind(self, fn):
        self.binders.append(fn)

    def responsive(self, wide):
        """Hook for pages that lay their blocks out in columns when the window is wide."""
        for fn in getattr(self, "layouts", ()):
            fn(wide)

    def sync(self, problems):
        for fn in self.binders:
            fn(problems)


def holder(*widgets, spacing=8):
    box = QWidget()
    layout = QHBoxLayout(box)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(spacing)
    for widget in widgets:
        layout.addWidget(widget) if isinstance(widget, QWidget) else layout.addStretch(1)
    return box


def dense_text():
    """A disabled multi-line sample field of the sheet (kept short so two columns stay level)."""
    edit = QPlainTextEdit()
    edit.setMaximumHeight(72)
    return edit


def dense(item):
    """Settings rows of the scan window are 48 px, not 60 (design `.sc .set-row{padding:10px 0}`)."""
    item.layout().setContentsMargins(0, 10, 0, 10)
    item.title.setWordWrap(True)
    return item


def row(page, title, description, control, key_tip=""):
    item = dense(SettingRow(tr(title), tr(description), control))
    if key_tip:
        item.title.setToolTip(key_tip)
    page.add(item)
    return item


def waiting_row(page, title, description, issue, reason, preview=None, into=None):
    """A field of the sheet that has no core setting: visible, disabled, with the reason and the neutral unavailable badge."""
    if preview is not None:
        preview.setEnabled(False)
    item = dense(
        SettingRow(
            tr(title),
            trf("{text} · {reason}", text=tr(description), reason=tr(reason))
            if description
            else tr(reason),
            preview,
        )
    )
    item.later_badge.setText(tr("Недоступно в этой версии ядра"))
    item.later_badge.setProperty("waiting_issue", issue)
    item.later_badge.setMinimumWidth(1)
    item.later_badge.setToolTip(tr("Появится") + ": " + tr(description or title))
    item.later_badge.show()
    item.setProperty("waiting", True)
    if into is None:
        page.add(item)
    else:
        if (
            preview is not None
        ):  # a narrow column: the disabled control goes under the text, not beside it
            item.layout().addWidget(preview, 1, 0)
        into.addWidget(item)
    return item


def segmented_row(page, draft, path, title, description, options, disabled=None, key_tip=None):
    if not draft.has(path):
        return waiting_row(
            page,
            title,
            description,
            ISSUE_SETTINGS,
            NOT_DECLARED,
            Segmented(options, None, tr(title)),
        )
    seg = Segmented(options, draft.value(path), tr(title))
    seg.setObjectName("seg_" + path)
    for value, issue in (disabled or {}).items():
        button = seg._buttons[value]
        button.setEnabled(False)
        button.setToolTip(tr("Недоступно в этой версии ядра"))
    seg.changed.connect(lambda value: draft.set_value(path, value))

    def sync(_problems):
        seg.setValue(draft.value(path))

    page.bind(sync)
    return row(page, title, description, seg, key_tip or path)


def switch_row(page, draft, path, title, description):
    if not draft.has(path):
        return waiting_row(
            page, title, description, ISSUE_SETTINGS, NOT_DECLARED, Switch(tr(title))
        )
    switch = Switch(tr(title), bool(draft.value(path)))
    switch.setObjectName("sw_" + path)
    switch.toggled.connect(lambda state: draft.set_value(path, bool(state)))

    def sync(_problems):
        if switch.isChecked() != bool(draft.value(path)):
            switch.blockSignals(True)
            switch.setChecked(bool(draft.value(path)))
            switch.blockSignals(False)

    page.bind(sync)
    return row(page, title, description, switch, path)


def number_row(page, draft, key, title, description, unit="", width=90, key_tip=""):
    from .new_scan_draft import NUMS

    if not draft.has(NUMS[key].path):
        return waiting_row(page, title, description, ISSUE_SETTINGS, NOT_DECLARED, QLineEdit())
    edit, sync_edit = number_edit(draft, key, tr(title), width)
    suffix = QLabel(tr(unit))
    suffix.setProperty("text_style", "meta")
    item = row(
        page, title, description, holder(edit, suffix) if unit else edit, key_tip or NUMS[key].path
    )
    item.control = edit if not unit else item.control

    def sync(problems):
        sync_edit()
        edit.setProperty("invalid", bool(problems.get(key)))
        polish(edit)
        item._error.setText(problems.get(key, ""))
        item._error_box.setVisible(bool(problems.get(key)))

    page.bind(sync)
    return item


class RateSlider(QWidget):
    """Design `.slider`: a 4 px track, a 16 px thumb, one position per preset; the thumb moves by drag or arrow keys."""

    changed = pyqtSignal(int)

    def __init__(self, steps, accessible_name="", parent=None):
        super().__init__(parent)
        self.steps, self.index = len(steps), 0
        self.setFixedSize(100, 16)
        self.setFocusPolicy(Qt.StrongFocus)
        self.setAccessibleName(accessible_name)

    def _x(self, index):
        return 8 + (self.width() - 16) * index / max(1, self.steps - 1)

    def set_index(self, index):
        self.index = index
        self.update()

    def paintEvent(self, _event):
        roles = theming.roles()
        painter = QPainter(self)
        painter.setRenderHint(QPainter.Antialiasing)
        painter.setPen(Qt.NoPen)
        mid, x = self.height() / 2, self._x(self.index)
        painter.setBrush(QColor(roles["selected"]))
        painter.drawRoundedRect(QRectF(8, mid - 2, self.width() - 16, 4), 2, 2)
        painter.setBrush(QColor(roles["primary"]))
        painter.drawRoundedRect(QRectF(8, mid - 2, x - 8, 4), 2, 2)
        painter.drawEllipse(QPointF(x, mid), 8, 8)
        painter.end()

    def _pick(self, x):
        steps = [self._x(i) for i in range(self.steps)]
        index = min(range(self.steps), key=lambda i: abs(steps[i] - x))
        if index != self.index:
            self.index = index
            self.update()
            self.changed.emit(index)

    def mousePressEvent(self, event):
        self._pick(event.x())

    def mouseMoveEvent(self, event):
        self._pick(event.x())

    def keyPressEvent(self, event):
        step = {Qt.Key_Left: -1, Qt.Key_Right: 1}.get(event.key(), 0)
        if step:
            self._pick(self._x(min(max(self.index + step, 0), self.steps - 1)))
        else:
            super().keyPressEvent(event)


def checkbox(text):
    box = QCheckBox(text)
    box.setAccessibleName(text)
    return box


# ---------------------------------------------------------------------------------------------------------------------
def speed_page(draft, host):
    page = Page(tr("Скорость и лимиты"), tr("Нагрузка на сайт и границы скана"))
    edit, sync_edit = number_edit(
        draft, "rps", tr("Запросов в секунду на хост"), 80, "scanRequestRate"
    )
    rates = (1, 2, 3, 5, 10)
    presets = Segmented([(n, str(n)) for n in rates], None, tr("Быстрые значения"))
    presets.changed.connect(lambda n: draft.set_text("rps", str(n)))
    # design `.slider`: the position of the preset, the same five values as the segmented control
    rate_slider = RateSlider(rates, tr("Запросов в секунду на хост"))
    rate_slider.setObjectName("scanRateSlider")
    rate_slider.changed.connect(lambda i: draft.set_text("rps", str(rates[i])))
    rps_row = row(
        page,
        "Запросов в секунду на хост",
        "Боевым сайтам — не больше 2. Выше — только для своего стенда",
        holder(edit, rate_slider, presets),
        "speed.min_delay_seconds = 1 / запросов в секунду",
    )
    warn = Note(
        "warn",
        tr("Выше безопасного для боевого сайта."),
        tr(
            "Разрешено, потому что адрес локальный: возможны 429/503 и блокировка IP на чужом сайте."
        ),
    )
    page.add(warn)

    def sync_rps(problems):
        sync_edit()
        rps_row._error.setText(problems.get("rps", ""))
        rps_row._error_box.setVisible("rps" in problems)
        edit.setProperty("invalid", "rps" in problems)
        polish(edit)
        rate = draft.rps()
        warn.setVisible(
            draft.local and rate is not None and rate > RPS_SAFE and "rps" not in problems
        )
        match = next((n for n in rates if rate is not None and abs(rate - n) < 1e-6), None)
        presets.setValue(match) if match is not None else clear_segmented(presets)
        if rate is not None:  # nearest preset when the value is not one of them
            rate_slider.set_index(min(range(len(rates)), key=lambda i: abs(rates[i] - rate)))

    page.bind(sync_rps)
    thread_edit, sync_threads = number_edit(
        draft, "threads", tr("Параллельных соединений"), 80, "scanConcurrency"
    )
    threads = Segmented([(n, str(n)) for n in (1, 2, 4, 8, 16)], None, tr("Быстрые значения"))
    threads.changed.connect(lambda n: draft.set_text("threads", str(n)))
    threads_row = row(
        page,
        "Параллельных соединений",
        "Общий пул на скан. Умолчание ядра — 1",
        holder(thread_edit, threads),
        "speed.concurrency",
    )

    def sync_pool(problems):
        sync_threads()
        threads_row._error.setText(problems.get("threads", ""))
        threads_row._error_box.setVisible("threads" in problems)
        value = draft.value("speed.concurrency")
        threads.setValue(value) if value in (1, 2, 4, 8, 16) else clear_segmented(threads)

    page.bind(sync_pool)
    switch_row(
        page,
        draft,
        "speed.adaptive",
        "Адаптивная скорость",
        "Снижает темп при 429/503 и росте времени ответа, возвращает при норме",
    )
    number_row(
        page,
        draft,
        "min_delay",
        "Минимальная пауза между запросами",
        "Связана с «запросов в секунду»: пауза = 1 / запросов в секунду",
        "с",
        90,
    )
    number_row(
        page,
        draft,
        "max_delay",
        "Максимальная пауза при замедлении",
        "Предел, до которого адаптивный режим замедляет обход",
        "с",
        90,
    )
    number_row(
        page,
        draft,
        "timeouts",
        "Остановка после тайм-аутов подряд",
        "Скан остановится после стольких тайм-аутов подряд",
        "",
        90,
    )
    page.caption(tr("Границы скана"))
    grid = QGridLayout()
    grid.setHorizontalSpacing(12)
    grid.setVerticalSpacing(10)
    boxes = {}
    limit_switch = Switch(tr("Лимит URL"), draft.limit_enabled)
    limit_switch.setObjectName("scanLimitEnabledSettings")
    limit_edit, sync_limit = number_edit(draft, "limit", tr("Лимит URL"), 0, "scanUrlLimitSettings")
    limit_switch.toggled.connect(draft.set_limit_enabled)
    limit_box = FieldBox(
        tr("Лимит URL"), limit_edit, tr("Верхняя граница числа URL"), trailing=limit_switch
    )
    boxes["limit"] = limit_box
    specs = (
        ("depth", "Глубина", "от стартовой страницы; −1 — без ограничения"),
        ("requests", "Лимит запросов", "с ресурсами и редиректами; 0 — без лимита"),
        ("minutes", "Время скана, мин", "0 — без лимита"),
    )
    for key, title, hint in specs:
        edit_, sync_ = number_edit(draft, key, tr(title), 0, f"scan_{key}_settings")
        boxes[key] = FieldBox(tr(title), edit_, tr(hint))
        boxes[key].sync = sync_
        page.bind(lambda _p, s=sync_: s())

    def arrange(wide):
        for item in (rps_row, threads_row):
            layout = item.layout()
            layout.removeWidget(item.control)
            layout.addWidget(
                item.control, 0 if wide else 1, 1 if wide else 0, Qt.AlignRight | Qt.AlignVCenter
            )
        columns = 4 if wide else 2
        for index, key in enumerate(("limit", "depth", "requests", "minutes")):
            grid.addWidget(
                boxes[key], index // columns, index % columns, Qt.AlignTop
            )  # captions and fields share one top line
        for column in range(4):
            grid.setColumnStretch(column, 1 if column < columns else 0)

    page.layouts.append(arrange)
    arrange(True)

    def sync_bounds(problems):
        sync_limit()
        if limit_switch.isChecked() != draft.limit_enabled:
            limit_switch.blockSignals(True)
            limit_switch.setChecked(draft.limit_enabled)
            limit_switch.blockSignals(False)
        limit_edit.setEnabled(draft.limit_enabled)
        limit_box.set_error(problems.get("limit", ""))
        for key in ("depth", "requests", "minutes"):
            boxes[key].set_error(problems.get(key, ""))

    page.bind(sync_bounds)
    holder_grid = QWidget()
    holder_grid.setLayout(grid)
    page.add(holder_grid)
    return page


# ---------------------------------------------------------------------------------------------------------------------
class RuleModel(QAbstractTableModel):
    """include/exclude regex rules of scope.*_patterns as one list (the core keeps two lists, order is not kept)."""

    def __init__(self, draft):
        super().__init__()
        self.draft = draft

    def rules(self):
        return [("include", p) for p in self.draft.value("scope.include_patterns", [])] + [
            ("exclude", p) for p in self.draft.value("scope.exclude_patterns", [])
        ]

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rules())

    def columnCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else 2

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if orientation == Qt.Horizontal and role == Qt.DisplayRole:
            return (tr("Тип"), tr("Шаблон"))[section]

    def data(self, index, role=Qt.DisplayRole):
        kind, pattern = self.rules()[index.row()]
        if role == Qt.DisplayRole:
            return (tr("включить") if kind == "include" else tr("исключить"), pattern)[
                index.column()
            ]
        if role == Qt.ToolTipRole:
            return pattern

    def reload(self):
        self.beginResetModel()
        self.endResetModel()


def scope_page(draft, host):
    page = Page(tr("Область"), tr("Что попадает в скан: хост, правила, типы файлов"))
    target = QLineEdit(draft.target or "")
    target.setReadOnly(True)
    target.setAccessibleName(tr("Стартовый URL"))
    target.setFixedWidth(320)
    row(page, "Стартовый URL", "Адрес берётся из проекта", target)
    segmented_row(
        page,
        draft,
        "scope.internal",
        "Какие хосты считать внутренними",
        "Только основной хост или весь домен со всеми поддоменами",
        [("host", tr("Только хост")), ("registrable_domain", tr("Домен и поддомены"))],
        key_tip="scope.internal",
    )
    waiting_row(
        page,
        "Основной хост и www",
        "Режим «хост + www»",
        ISSUE_SETTINGS,
        "в ядре только «хост» или «весь домен»",
        Segmented([("a", tr("Нет")), ("b", "www"), ("c", tr("Все"))], "a", tr("Поддомены")),
    )
    columns = QGridLayout()
    columns.setHorizontalSpacing(20)
    columns.setVerticalSpacing(0)
    left, right = QVBoxLayout(), QVBoxLayout()
    left.setSpacing(6)
    right.setSpacing(0)
    rules_caption = QLabel(tr("Правила regex"))
    rules_caption.setProperty("text_style", "overline")
    rules_caption.setContentsMargins(0, 14, 0, 0)
    left.addWidget(rules_caption)
    model = RuleModel(draft)
    table = QTableView()
    table.setObjectName("scanRulesTable")
    table.setAccessibleName(tr("Правила области"))
    table.setModel(model)
    style_table(table)
    table.setMinimumHeight(84)
    table.setMaximumHeight(120)
    left.addWidget(table)
    empty = QLabel(tr("Правил нет: сканируется весь хост"))
    empty.setProperty("text_style", "meta")
    left.addWidget(empty)
    page.bind(lambda _p: empty.setVisible(not model.rules()))
    entry = QLineEdit()
    entry.setPlaceholderText(tr("Регулярное выражение, например /(cart|checkout)/"))
    entry.setAccessibleName(tr("Новое правило"))
    include = QPushButton(tr("Включить"))
    exclude = QPushButton(tr("Исключить"))
    remove = QPushButton(tr("Удалить выбранное"))
    remove.setProperty("role", "text")
    problem = QLabel()
    problem.setProperty("field_error", True)
    problem.setProperty("text_style", "meta")
    left.addWidget(entry)
    left.addWidget(holder(include, exclude, remove, None))
    left.addWidget(problem)

    def add(kind):
        text = entry.text().strip()
        if not text:
            return
        try:
            re.compile(text)
        except re.error as exc:
            problem.setText(trf("Некорректное выражение: {error}", error=str(exc)))
            entry.setProperty("invalid", True)
            polish(entry)
            return
        path = f"scope.{kind}_patterns"
        if not draft.has(path):
            return
        problem.setText("")
        entry.setProperty("invalid", False)
        polish(entry)
        draft.set_value(path, [*draft.value(path, []), text])
        entry.clear()

    def delete():
        current = table.currentIndex()
        if not current.isValid():
            return
        kind, pattern = model.rules()[current.row()]
        path = f"scope.{kind}_patterns"
        values = list(draft.value(path, []))
        values.remove(pattern)
        draft.set_value(path, values)

    include.clicked.connect(lambda: add("include"))
    exclude.clicked.connect(lambda: add("exclude"))
    remove.clicked.connect(delete)
    page.bind(lambda _p: model.reload())
    note = QLabel(tr("Ядро хранит два списка и не учитывает порядок правил."))
    note.setProperty("text_style", "meta")
    left.addWidget(note)
    right_caption = QLabel(tr("Проверка правил"))
    right_caption.setProperty("text_style", "overline")
    right_caption.setContentsMargins(0, 14, 0, 0)
    right.addWidget(right_caption)
    waiting_row(
        page,
        "Какие из URL войдут",
        "",
        ISSUE_OTHER,
        "ядро не отвечает без запуска скана",
        dense_text(),
        into=right,
    )
    right.addStretch(1)
    left_box, right_box = QWidget(), QWidget()
    left_box.setLayout(left)
    right_box.setLayout(right)
    for box in (left_box, right_box):
        box.layout().setContentsMargins(0, 0, 0, 0)
    page.add(columns_holder := QWidget())
    columns_holder.setLayout(columns)

    def arrange_columns(wide):
        columns.addWidget(left_box, 0, 0)
        columns.addWidget(right_box, 0, 1) if wide else columns.addWidget(right_box, 1, 0)
        columns.setColumnStretch(0, 1)
        columns.setColumnStretch(1, 1 if wide else 0)

    page.layouts.append(arrange_columns)
    arrange_columns(True)
    waiting_row(
        page,
        "GET-параметры",
        "Оставлять / удалять отмеченные / игнорировать все",
        ISSUE_SETTINGS,
        "в ядре нет нормализации параметров",
        Segmented(
            [
                ("k", tr("Оставлять")),
                ("s", tr("Удалять отмеченные")),
                ("i", tr("Игнорировать все")),
            ],
            "k",
            tr("GET-параметры"),
        ),
    )
    page.caption(tr("Типы файлов"))
    boxes = {}
    flow = QGridLayout()
    flow.setHorizontalSpacing(24)
    for index, (group, (label, extensions)) in enumerate(FILE_GROUPS.items()):
        box = checkbox(tr(label))
        box.setObjectName("exclude_" + group)
        box.setToolTip(", ".join(extensions))
        boxes[group] = box
        flow.addWidget(box, index // 4, index % 4)

        def toggled(state, ext=extensions):
            current = [e for e in draft.value("scope.exclude_extensions", [])]
            current = [e for e in current if e not in ext]
            draft.set_value("scope.exclude_extensions", current + (list(ext) if state else []))

        box.clicked.connect(toggled)

    def sync_files(_problems):
        current = set(draft.value("scope.exclude_extensions", []))
        for group, (_label, extensions) in FILE_GROUPS.items():
            boxes[group].setChecked(set(extensions) <= current)

    if draft.has("scope.exclude_extensions"):
        page.bind(sync_files)
    else:
        for box in boxes.values():
            box.setEnabled(False)
    wrap = QWidget()
    wrap.setLayout(flow)
    page.add(wrap)
    return page


# ---------------------------------------------------------------------------------------------------------------------
USER_AGENTS = {"seohead": "", "google": GOOGLEBOT_UA, "yandex": YANDEX_UA, "mobile": MOBILE_UA}


def request_page(draft, host):
    page = Page(tr("Запрос"), tr("Как ядро представляется сайту"))
    labels = [
        ("seohead", "SEOHEAD"),
        ("google", "Googlebot"),
        ("yandex", "YandexBot"),
        ("mobile", tr("Мобильный")),
    ]
    if not draft.has("http.user_agent"):
        waiting_row(
            page,
            "User-Agent",
            "",
            ISSUE_SETTINGS,
            NOT_DECLARED,
            Segmented(labels, "seohead", "User-Agent"),
        )
    else:
        seg = Segmented(labels, "seohead", tr("User-Agent"))
        seg.setObjectName("seg_user_agent")
        line = QLineEdit()
        line.setReadOnly(True)
        line.setAccessibleName(tr("Строка User-Agent"))

        def pick(name):
            draft.set_value("http.user_agent", USER_AGENTS[name])
            if name == "google" and draft.has("robots.user_agent_token"):
                draft.set_value("robots.user_agent_token", "Googlebot")
            elif draft.has("robots.user_agent_token"):
                draft.set_value(
                    "robots.user_agent_token", draft.core.get("robots.user_agent_token", "")
                )

        seg.changed.connect(pick)

        def sync(_p):
            agent = draft.value("http.user_agent", "")
            name = next((n for n, ua in USER_AGENTS.items() if ua == agent), None)
            seg.setValue(name) if name else clear_segmented(seg)
            line.setText(agent or tr("По умолчанию ядра"))
            line.setProperty("mono", True)
            polish(line)

        page.bind(sync)
        row(
            page,
            "User-Agent",
            "Как ядро представляется сайту. Свой текст ждёт доработки.",
            seg,
            "http.user_agent",
        )
        page.add(line)
    reason = "заголовки, cookies, авторизация и прокси задаются отдельным доверенным способом"
    waiting_row(
        page,
        "Свой User-Agent",
        "",
        ISSUE_SETTINGS,
        "ядро принимает строку, но в приложении она не подключена",
        QLineEdit(),
    )
    page.caption(tr("Заголовки, доступ, прокси"))
    columns = QGridLayout()
    columns.setHorizontalSpacing(18)  # design .sc grid gap
    columns.setVerticalSpacing(0)
    left, right = QVBoxLayout(), QVBoxLayout()
    for side in (left, right):
        side.setContentsMargins(0, 0, 0, 0)
        side.setSpacing(0)
    waiting_row(
        page, "Заголовки запроса", "Имя и значение", ISSUE_SETTINGS, reason, QLineEdit(), into=left
    )
    waiting_row(
        page,
        "Cookies",
        "name=value; …",
        ISSUE_SETTINGS,
        "ядро принимает их только через переменные окружения для хоста",
        QLineEdit(),
        into=left,
    )
    waiting_row(
        page,
        "HTTP-авторизация",
        "Basic / Digest",
        ISSUE_SETTINGS,
        "в ядре нет логина и пароля, только готовый заголовок из окружения",
        Switch(tr("HTTP-авторизация")),
        into=right,
    )
    waiting_row(
        page,
        "Прокси",
        "Нет / системный / свой",
        ISSUE_SETTINGS,
        "адрес прокси задаётся отдельным доверенным способом",
        Segmented([("n", tr("Нет")), ("s", tr("Системный")), ("o", tr("Свой"))], "n", tr("Прокси")),
        into=right,
    )
    left.addStretch(1)
    right.addStretch(1)
    left_box, right_box = QWidget(), QWidget()
    left_box.setLayout(left)
    right_box.setLayout(right)
    holder_box = QWidget()
    holder_box.setLayout(columns)
    page.add(holder_box)

    def arrange(wide):
        columns.addWidget(left_box, 0, 0)
        columns.addWidget(right_box, 0, 1) if wide else columns.addWidget(right_box, 1, 0)
        columns.setColumnStretch(0, 1)
        columns.setColumnStretch(1, 1 if wide else 0)

    page.layouts.append(arrange)
    arrange(True)
    return page


# ---------------------------------------------------------------------------------------------------------------------
def robots_page(draft, host):
    page = Page(tr("robots.txt и директивы"), tr("Что делать с запретами и подсказками сайта"))
    segmented_row(
        page,
        draft,
        "robots.policy",
        "robots.txt",
        "«Отчёт» — сканировать всё, но пометить закрытые URL",
        [
            ("respect", tr("Соблюдать")),
            ("report_only", tr("Отчёт")),
            ("ignore", tr("Игнорировать")),
        ],
    )
    warn = Note(
        "warn",
        tr("Игнорировать robots.txt на чужом боевом сайте нельзя."),
        tr("Только для своего стенда."),
    )
    page.add(warn)
    page.bind(lambda p: warn.setVisible(draft.value("robots.policy") != "respect"))
    if draft.has("discovery.follow_nofollow"):
        follow = Segmented(
            [(True, tr("Ходить")), (False, tr("Не ходить"))],
            bool(draft.value("discovery.follow_nofollow")),
            tr("Ссылки с nofollow"),
        )
        follow.changed.connect(lambda value: draft.set_value("discovery.follow_nofollow", value))
        page.bind(lambda _p: follow.setValue(bool(draft.value("discovery.follow_nofollow"))))
        row(
            page,
            "Ссылки с nofollow",
            "Ходить по ним и помечать, или не ходить",
            follow,
            "discovery.follow_nofollow",
        )
    else:
        waiting_row(
            page,
            "Ссылки с nofollow",
            "",
            ISSUE_SETTINGS,
            NOT_DECLARED,
            Segmented([("a", tr("Ходить")), ("b", tr("Не ходить"))], "a", tr("Ссылки с nofollow")),
        )
    waiting_row(
        page,
        "Страницы с noindex",
        "Брать ссылки со страниц с noindex",
        ISSUE_OTHER,
        "в ядре нет такой настройки",
        Switch(tr("Брать ссылки с noindex")),
    )
    waiting_row(
        page,
        "Переходить по canonical",
        "Добавлять канонические URL в очередь",
        ISSUE_SETTINGS,
        "в ядре нет такой настройки для обхода сайта",
        Switch(tr("Canonical")),
    )
    waiting_row(
        page,
        "Переходить по hreflang",
        "Альтернативы на других языках и доменах",
        ISSUE_SETTINGS,
        "в ядре нет такой настройки",
        Switch(tr("hreflang")),
    )
    # Fixed behaviour of the core, not a setting: X-Robots-Tag and meta robots are merged, so the stricter directive wins.
    always_on = Switch(tr("X-Robots-Tag и meta robots"), checked=True)
    always_on.setEnabled(False)
    row(
        page,
        "X-Robots-Tag и meta robots",
        "Читать оба источника; при конфликте строже побеждает",
        always_on,
    )
    page.caption(tr("Sitemap"))
    switch_row(
        page,
        draft,
        "sitemaps.auto_discover",
        "Искать sitemap автоматически",
        "robots.txt и /sitemap.xml. Умолчание ядра — выключено",
    )
    waiting_row(
        page,
        "Список sitemap",
        "Автоматические и добавленные вручную с числом URL",
        ISSUE_OTHER,
        "ядро принимает один sitemap, число URL известно после чтения",
    )
    page.add(sitemap_list_table())
    return page


def sitemap_list_table():
    """The sitemap list has no core source yet: an empty, disabled table with the columns the list will have."""
    table = QTableWidget(1, 3)
    table.setObjectName("sitemapListTable")
    table.setHorizontalHeaderLabels([tr("Sitemap"), tr("Источник"), tr("URL")])
    style_table(table, "compact")
    table.setSpan(0, 0, 1, 3)
    table.setItem(0, 0, QTableWidgetItem(tr("Нет данных")))
    table.setFixedHeight(
        table.horizontalHeader().height() + 2 * table.verticalHeader().defaultSectionSize() + 6
    )
    table.setEnabled(False)
    return table


# ---------------------------------------------------------------------------------------------------------------------
def render_page(draft, host):
    page = Page(
        tr("JS-рендеринг"),
        tr("Исполнять JavaScript перед разбором страницы · браузер на этом компьютере"),
    )
    segmented_row(
        page,
        draft,
        "rendering.mode",
        "Режим",
        "«Авто» ждёт ядра: критерия «мало текста» в нём нет",
        [("raw", tr("Выкл")), ("auto", tr("Авто")), ("js", tr("Всегда"))],
        disabled={"auto": ISSUE_RENDER},
    )
    note = Note(
        "warn",
        tr("Рендер медленнее обычной загрузки и нагружает сайт."),
        tr("Точную оценку времени ядро не даёт (недоступно в этой версии ядра)."),
    )
    page.add(note)
    page.bind(lambda _p: note.setVisible(draft.value("rendering.mode") == "js"))
    segmented_row(
        page,
        draft,
        "rendering.browser.engine",
        "Движок",
        "Наличие браузера проверяется при запуске; проверка заранее недоступна в этой версии ядра",
        [("chromium", "Chromium"), ("firefox", "Firefox"), ("webkit", "WebKit")],
    )
    segmented_row(
        page,
        draft,
        "rendering.browser.viewport",
        "Окно просмотра",
        "Desktop или Mobile",
        [("desktop", "Desktop"), ("mobile", "Mobile")],
    )
    segmented_row(
        page,
        draft,
        "rendering.browser.wait_until",
        "Ждать готовности",
        "Сигнал, после которого DOM считается готовым",
        [
            ("load", "load"),
            ("domcontentloaded", "DOMContentLoaded"),
            ("networkidle", "networkidle"),
        ],
    )
    number_row(
        page,
        draft,
        "script_timeout",
        "Время работы JavaScript после загрузки",
        "Предел ожидания",
        "с",
        80,
    )
    waiting_row(
        page,
        "Дополнительная пауза",
        "После сигнала готовности, мс",
        ISSUE_RENDER,
        "в ядре нет отдельной паузы",
        QLineEdit(),
    )
    segmented_row(
        page,
        draft,
        "rendering.browser.page_concurrency",
        "Одновременных вкладок",
        "Отдельно от потоков HTTP",
        [(n, str(n)) for n in (1, 2, 4, 8, 16)],
    )
    blocked = [checkbox(tr(name)) for name in ("Изображения", "Шрифты", "Медиа", "Аналитика")]
    for box in blocked:
        box.setEnabled(False)
    waiting_row(
        page,
        "Блокировать ресурсы",
        "Изображения, шрифты, медиа, аналитика",
        ISSUE_RENDER,
        "в ядре нет блокировки ресурсов при рендере",
        holder(*blocked, spacing=16),
    )
    switch_row(
        page,
        draft,
        "rendering.artifacts.screenshots",
        "Скриншот страницы целиком",
        "Только при включённом JS; первый экран ядро не умеет",
    )
    note_raw = QLabel(tr("Настройки рендера уходят в команду только при режиме «Всегда»."))
    note_raw.setProperty("text_style", "meta")
    page.add(note_raw)
    return page


# ---------------------------------------------------------------------------------------------------------------------
class ExtractModel(QAbstractTableModel):
    """The empty column set of the extractor table (design ScExtract): the core cannot be given such rules yet."""

    HEADERS = ("Имя столбца", "Тип", "Выражение", "Вернуть", "Проверка на образце")

    def rowCount(self, parent=None):
        return 0

    def columnCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.HEADERS)

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if orientation == Qt.Horizontal and role == Qt.DisplayRole:
            return tr(self.HEADERS[section])


def extract_page(draft, host):
    page = Page(
        tr("Извлечение"), tr("Свои поля из HTML: столбцы появятся в таблице скана и экспорте")
    )
    page.add(
        Note(
            "info",
            tr("Экстракторы пока не подключены"),
            tr(
                "Ядро принимает только data-only правила evidence.extraction_rules; XPath, Regex и проверка на образце в crawl-site недоступны. Редактор не подключён, в команду ничего не уходит."
            ),
        )
    )
    table = QTableView()
    table.setObjectName("scanExtractTable")
    table.setAccessibleName(tr("Экстракторы"))
    table.setModel(ExtractModel())
    style_table(table)
    table.setEnabled(False)
    table.setMinimumHeight(120)
    table.setMaximumHeight(150)
    page.add(table)
    waiting_row(
        page,
        "Список экстракторов",
        "Имя столбца, тип CSS/XPath/Regex, выражение",
        ISSUE_EXTRACT,
        "правила не редактируются в приложении",
        QPushButton(tr("Экстрактор")),
    )
    waiting_row(
        page,
        "Проверка на образце",
        "Один запрос к образцу с текущими настройками",
        ISSUE_EXTRACT,
        "ядро не проверяет правила без запуска скана",
        QLineEdit(),
    )
    return page


# ---------------------------------------------------------------------------------------------------------------------
def storage_page(draft, host):
    page = Page(tr("Хранение"), tr("Что ядро сохраняет на диск"))
    segmented_row(
        page,
        draft,
        "storage.body_mode",
        "Тело ответа",
        "HTML нужен для поиска по коду и сравнения текстов",
        [("off", tr("Не хранить")), ("captured_entity_bytes", tr("Сохранять тела"))],
    )
    waiting_row(
        page,
        "Все текстовые ресурсы",
        "Тела CSS, JS и других текстовых ответов",
        ISSUE_SETTINGS,
        "в ядре нет такого режима хранения",
        QLineEdit(),
    )
    number_row(
        page, draft, "body_mb", "Максимум на один ответ", "Больше — обрезается с пометкой", "МБ", 80
    )
    number_row(
        page,
        draft,
        "free_gb",
        "Остановить, если свободно меньше",
        "Скан встанет на паузу, данные сохранятся; продолжение — «Продолжить скан»",
        "ГБ",
        80,
    )
    waiting_row(
        page,
        "Хранить сканы проекта",
        "Сколько последних сканов оставлять",
        ISSUE_OTHER,
        "в ядре нет постоянной политики хранения, только разовая очистка",
        Segmented([("5", "5"), ("20", "20"), ("all", tr("Все"))], "20", tr("Хранить сканы")),
    )
    waiting_row(
        page,
        "Сжимать тела (zstd)",
        "",
        ISSUE_OTHER,
        "ядро хранит тела без zstd",
        Switch(tr("Сжатие")),
    )
    page.caption(tr("Место на диске"))
    card = QFrame()
    card.setProperty("card", "panel")
    box = QVBoxLayout(card)
    box.setContentsMargins(14, 12, 14, 12)
    free_line = QLabel()
    free_line.setObjectName("scanDiskFree")
    bar = QFrame()
    bar.setProperty("disk_bar", True)
    bar.setFixedHeight(10)
    fill = QFrame(bar)
    fill.setProperty("disk_fill", True)
    fill.setFixedHeight(10)
    tail = QFrame(bar)  # the last bytes before the pause threshold (design «порог паузы»)
    tail.setProperty("disk_pause", True)
    tail.setFixedHeight(10)
    legend = QHBoxLayout()
    legend.setSpacing(16)
    used_dot, pause_dot = QFrame(), QFrame()
    for dot, kind in ((used_dot, "used"), (pause_dot, "pause")):
        dot.setProperty("disk_legend", kind)
        dot.setFixedSize(8, 8)
    used_label, pause_label = QLabel(tr("занято")), QLabel()
    for label in (used_label, pause_label):
        label.setProperty("text_style", "meta")
    for dot, label in ((used_dot, used_label), (pause_dot, pause_label)):
        item = QHBoxLayout()
        item.setSpacing(6)
        item.addWidget(dot)
        item.addWidget(label)
        legend.addLayout(item)
    legend.addStretch(1)
    estimate = QLabel(tr("Размер этого скана: оценка недоступна в этой версии ядра"))
    estimate.setProperty("text_style", "meta")
    folder = ElidedLabel()
    folder.setProperty("text_style", "meta")
    pause = Note(
        "warn",
        tr("Свободного места меньше порога паузы."),
        tr("Скан остановится сразу после старта."),
    )
    box.addWidget(free_line)
    box.addWidget(bar)
    box.addLayout(legend)
    box.addWidget(estimate)
    box.addWidget(folder)
    page.add(card)
    page.add(pause)

    def sync(_p):
        directory = Path(host_directory(host) or ".")
        try:
            usage = shutil.disk_usage(directory if directory.exists() else Path.home())
        except OSError:
            free_line.setText(tr("Свободное место не измерено"))
            fill.setFixedWidth(0)
            tail.hide()
            pause.hide()
        else:
            free_line.setText(
                trf(
                    "Свободно на диске проекта: {free} ГБ из {total} ГБ",
                    free=grouped(usage.free // 1024**3),
                    total=grouped(usage.total // 1024**3),
                )
            )
            width = bar.width()
            used_px = max(2, int(width * (usage.used / usage.total))) if usage.total else 0
            fill.setFixedWidth(used_px)
            threshold = draft.effective("storage.min_free_bytes") or 0
            tail_px = (
                min(width - used_px, max(2, int(width * threshold / usage.total)))
                if threshold and usage.total
                else 0
            )
            tail.setVisible(tail_px > 0)
            tail.setGeometry(width - tail_px, 0, tail_px, 10)
            pause_dot.setVisible(bool(threshold))
            pause_label.setVisible(bool(threshold))
            pause_label.setText(trf("порог паузы {gb} ГБ", gb=grouped(threshold // 1024**3)))
            pause.setVisible(bool(threshold) and usage.free < threshold)
        folder.setText(trf("Папка: {path}", path=str(directory / "scans")))

    page.bind(sync)
    return page


def host_directory(host):
    return getattr(host, "project_directory", "") or ""


# ---------------------------------------------------------------------------------------------------------------------
class DiffModel(QAbstractTableModel):
    def __init__(self):
        super().__init__()
        self.rows = []

    def set_rows(self, rows):
        self.beginResetModel()
        self.rows = rows
        self.endResetModel()

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else 3

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if orientation == Qt.Horizontal and role == Qt.DisplayRole:
            return (tr("Параметр"), tr("Умолчание ядра"), tr("В профиле"))[section]

    def data(self, index, role=Qt.DisplayRole):
        if role in (Qt.DisplayRole, Qt.ToolTipRole):
            return str(self.rows[index.row()][index.column()])


def short(value):
    if isinstance(value, bool):
        return tr("вкл") if value else tr("выкл")
    text = str(value)
    return text if len(text) <= 60 else text[:57] + "…"


def profiles_page(draft, host):
    page = Page(tr("Профили"), tr("Наборы настроек скана"))
    state = QLabel()
    state.setObjectName("scanProfileState")
    state.setProperty("text_style", "meta")
    state.setWordWrap(True)
    cards = {}
    layout = QHBoxLayout()
    layout.setSpacing(10)
    definitions = {
        "core": ("Умолчания ядра", "встроенный", "Значения crawl-describe-settings без изменений"),
        "app": ("Умолчания приложения", "из настроек", "Настройки → Сканы по умолчанию"),
        "project": ("Профиль проекта", "из проекта", "policy.crawl_overrides проекта"),
    }
    for key, (title, tag, text) in definitions.items():
        card = ChoiceCard(key, None, title, text, "profile_" + key, tag)
        cards[key] = card
        layout.addWidget(card, 1)
    cards["core"].setChecked(True)
    strip = QWidget()
    strip.setLayout(layout)
    page.add(strip)
    page.add(state)
    model = DiffModel()
    table = QTableView()
    table.setObjectName("scanProfileDiff")
    table.setAccessibleName(tr("Отличия профиля от умолчаний ядра"))
    table.setModel(model)
    style_table(table)
    # design ScProfiles «.gt»: parameter 220 px, the two value columns share the rest
    header = table.horizontalHeader()
    header.setSectionResizeMode(0, QHeaderView.Fixed)
    header.resizeSection(0, 220)
    header.setSectionResizeMode(1, QHeaderView.Stretch)
    header.setSectionResizeMode(2, QHeaderView.Stretch)
    table.setMinimumHeight(160)
    page.caption(tr("Отличия от умолчаний ядра"))
    page.add(table)
    actions = QGridLayout()
    actions.setHorizontalSpacing(8)
    apply_button = QPushButton(tr("Применить к этому скану"))
    apply_button.setObjectName("profileApply")
    apply_button.setProperty("role", "tonal")
    actions.addWidget(apply_button, 0, 0, 1, 2)
    for index, text in enumerate(("Дублировать", "Удалить", "Импорт JSON…", "Экспорт JSON")):
        button = QPushButton(tr(text))
        button.setEnabled(False)
        button.setToolTip(tr("Недоступно в этой версии ядра"))
        actions.addWidget(button, 1, index)
    actions.addWidget(waiting_badge(ISSUE_PROFILES), 1, 4)
    actions.setColumnStretch(5, 1)
    strip2 = QWidget()
    strip2.setLayout(actions)
    page.add(strip2)
    note = QLabel(
        tr(
            "Ядро хранит один набор настроек на проект (policy.crawl_overrides). Создавать, переименовывать и удалять именованные профили оно не умеет."
        )
    )
    note.setProperty("text_style", "meta")
    note.setWordWrap(True)
    page.add(note)
    selected = {"key": "core"}

    def layer(key):
        return {"core": {}, "app": draft.app_layer, "project": draft.project_layer}[key]

    def show(key):
        selected["key"] = key
        rows = [
            (path, short(draft.core.get(path, "")), short(value))
            for path, value in sorted(layer(key).items())
            if draft.core.get(path) != value
        ]
        model.set_rows(rows)
        if key == "core":
            state.setText(tr("Встроенные значения ядра. Различий нет."))
        elif key == "project" and draft.policy_state != "ready":
            state.setText(
                tr("Профиль проекта загружается…")
                if draft.policy_state == "loading"
                else tr("Профиль проекта не прочитан.")
            )
        elif not rows:
            state.setText(tr("Профиль не задан: отличий от умолчаний ядра нет."))
        else:
            state.setText(trf("Отличий от умолчаний ядра: {n}", n=len(rows)))
        apply_button.setEnabled(
            key == "core" or key == "app" or (key == "project" and draft.policy_state == "ready")
        )

    for key, card in cards.items():
        card.clicked.connect(lambda _c, k=key: show(k))

    def apply():
        stacks = {"core": ("core",), "app": ("core", "app"), "project": ("core", "app", "project")}
        draft.reset(stacks[selected["key"]])

    apply_button.clicked.connect(apply)
    page.bind(lambda _p: show(selected["key"]))
    return page


PAGES = (
    ("speed", "Скорость и лимиты", "speed", speed_page),
    ("scope", "Область", "travel_explore", scope_page),
    ("request", "Запрос", "http", request_page),
    ("robots", "robots.txt и директивы", "smart_toy", robots_page),
    ("render", "JS-рендеринг", "javascript", render_page),
    ("extract", "Извлечение", "data_object", extract_page),
    ("storage", "Хранение", "database", storage_page),
    ("profiles", "Профили", "tune", profiles_page),
)
