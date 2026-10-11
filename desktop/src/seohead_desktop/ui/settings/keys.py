"""Settings → Горячие клавиши (sheet SetKeys). Registry, validation and conflicts live in ``shortcuts``."""

from __future__ import annotations

import json
from pathlib import Path

from PyQt5.QtCore import QEvent, Qt, pyqtSignal
from PyQt5.QtGui import QKeySequence
from PyQt5.QtWidgets import (
    QFileDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from ... import shortcuts, theming
from ...i18n import tr, trf
from ..controls import SettingRow, polish
from ..icons import MaterialIconLabel, material_icon
from .helpers import group_label, two_columns

ID, ICON, TITLE = "keys", "keyboard", "Горячие клавиши"
HINT = "Сочетания по группам; конфликты подсвечиваются"
SCHEMA = shortcuts.schema()

_MODIFIER_KEYS = {Qt.Key_Control, Qt.Key_Shift, Qt.Key_Alt, Qt.Key_Meta, Qt.Key_AltGr}


class KeyCapture(QFrame):
    """Key caps of one shortcut; click (or Enter/Space) starts recording, Esc cancels it."""

    captured = pyqtSignal(str)  # portable text of the recorded chord
    recording_changed = pyqtSignal(bool)

    def __init__(self, accessible_name, parent=None):
        self._recording = False
        super().__init__(parent)
        self.setProperty("keycap", "idle")
        self.setFocusPolicy(Qt.StrongFocus)
        self.setCursor(Qt.PointingHandCursor)
        self._title = accessible_name
        layout = QHBoxLayout(self)
        layout.setContentsMargins(4, 2, 4, 2)
        layout.setSpacing(4)
        self._caps = QHBoxLayout()
        self._caps.setSpacing(4)
        layout.addLayout(self._caps)
        self._prompt = QWidget()
        prompt_layout = QHBoxLayout(self._prompt)
        prompt_layout.setContentsMargins(6, 0, 6, 0)
        prompt_layout.setSpacing(6)
        self._prompt_icon = MaterialIconLabel(
            "radio_button_unchecked", 16, color=theming.roles()["on_selected"]
        )
        prompt_layout.addWidget(self._prompt_icon)
        prompt_text = QLabel("Нажмите сочетание…")
        prompt_text.setProperty("kbd_prompt", True)
        prompt_layout.addWidget(prompt_text)
        self._prompt.setVisible(False)
        layout.addWidget(self._prompt)
        theming.signals.changed.connect(self._retint)
        self.set_value("")

    def _retint(self, _name):
        self._prompt_icon.set_material_icon(
            "radio_button_unchecked", theming.roles()["on_selected"]
        )

    def set_value(self, portable, bad=False):
        while self._caps.count():
            old = self._caps.takeAt(0).widget()
            old.hide()
            old.deleteLater()
        texts = shortcuts.parts(portable) or [tr("Не задано")]
        for text in texts:
            cap = QLabel(text)
            if portable:
                cap.setProperty("kbd", "bad" if bad else "ok")
            else:
                cap.setProperty("na", True)
            self._caps.addWidget(cap)
            cap.setVisible(not self._recording)
        self.setAccessibleName(
            trf(
                "{title}: {keys}",
                title=self._title,
                keys=shortcuts.display(portable) or "не задано",
            )
        )

    def is_recording(self):
        return self._recording

    def _set_recording(self, on):
        if on == self._recording:
            return
        self._recording = on
        self.setProperty("keycap", "recording" if on else "idle")
        polish(self)
        for index in range(self._caps.count()):
            self._caps.itemAt(index).widget().setVisible(not on)
        self._prompt.setVisible(on)
        self.recording_changed.emit(on)

    def start(self):
        self.setFocus()
        self._set_recording(True)

    def stop(self):
        self._set_recording(False)

    def mousePressEvent(self, event):
        if self.isEnabled() and event.button() == Qt.LeftButton:
            self.start()

    def focusOutEvent(self, event):
        self.stop()
        super().focusOutEvent(event)

    def event(self, event):
        if self._recording and event.type() == QEvent.ShortcutOverride:  # let ⌘W, ⌘, … be recorded
            event.accept()
            return True
        return super().event(event)

    def keyPressEvent(self, event):
        if not self._recording:
            if event.key() in (Qt.Key_Return, Qt.Key_Enter, Qt.Key_Space):
                self.start()
            else:
                super().keyPressEvent(event)
            return
        if event.key() == Qt.Key_Escape:
            self.stop()
        elif event.key() not in _MODIFIER_KEYS:
            chord = (
                int(
                    event.modifiers()
                    & (Qt.ControlModifier | Qt.ShiftModifier | Qt.AltModifier | Qt.MetaModifier)
                )
                | event.key()
            )
            self.stop()
            self.captured.emit(QKeySequence(chord).toString(QKeySequence.PortableText))


class ShortcutRow(SettingRow):
    """One action: name on the left; reset, 'Заменить' and the key caps on the right."""

    def __init__(self, store, action):
        self.store, self.action = store, action
        self.capture = KeyCapture(action.title)
        self.reset_button = QToolButton()
        self.reset_button.setProperty("role", "icon")
        self.reset_button.setIcon(material_icon("restart_alt"))
        self.reset_button.setToolTip("Сбросить сочетание")
        self.reset_button.setAccessibleName(trf("Сбросить сочетание: {title}", title=action.title))
        self.replace_button = QPushButton("Заменить")
        self.replace_button.setProperty("role", "text")
        self.replace_button.setProperty("size", "pill")
        control = QWidget()
        layout = QHBoxLayout(control)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(6)
        for widget in (self.capture, self.replace_button, self.reset_button):
            layout.addWidget(widget)
        super().__init__(action.title, "", control)
        self.setProperty("setting_key", shortcuts.key(action.id))
        self.setProperty("shortcut", True)
        self.layout().setContentsMargins(0, 6, 0, 6)
        self._pending = None  # (portable, replaceable) of a recorded chord waiting for 'Заменить'
        if action.fixed:
            self.capture.setEnabled(False)
            self.capture.setToolTip("Сочетание фиксировано")
        self.capture.captured.connect(self._on_captured)
        self.capture.recording_changed.connect(self._on_recording)
        self.reset_button.clicked.connect(self.reset)
        self.replace_button.clicked.connect(self._replace)
        self.refresh()

    def value(self):
        return (
            self.action.default
            if self.action.fixed
            else self.store.get(shortcuts.key(self.action.id))
        )

    def refresh(self):
        pending = self._pending[0] if self._pending else None
        self.capture.set_value(pending or self.value(), bad=bool(pending))
        self.reset_button.setVisible(not self.action.fixed and self.value() != self.action.default)
        self.replace_button.setVisible(bool(self._pending and self._pending[1]))
        if bool(self._pending) != bool(self.property("conflict")):
            self.setProperty("conflict", bool(self._pending))
            for widget in [
                self,
                *self.findChildren(QWidget),
            ]:  # descendants do not restyle with the parent
                polish(widget)

    def _clear_state(self):
        self._pending = None
        self.set_error(None)

    def _on_recording(self, on):
        if on:
            self._clear_state()
            self.refresh()

    def _on_captured(self, portable):
        normal, error = shortcuts.normalise(portable)
        other = (
            shortcuts.conflict(self.store, self.action.id, normal) if normal is not None else None
        )
        if normal is None:
            self.set_error(error)
        elif other is not None:
            self._pending = (normal, not other.fixed)
            self.set_error(
                trf(
                    "{keys} уже занято: «{title}»",
                    keys=shortcuts.display(normal),
                    title=other.title,
                )
            )
        else:
            self.store.set(shortcuts.key(self.action.id), normal)
        self.refresh()

    def _replace(self):
        portable, _replaceable = self._pending
        self._clear_state()
        shortcuts.set_binding(self.store, self.action.id, portable, replace=True)
        self.refresh()

    def reset(self):
        self._clear_state()
        default = self.action.default
        other = shortcuts.conflict(self.store, self.action.id, default)
        if other is not None:  # the default is taken by a customised action: clear that one first
            self.store.set(shortcuts.key(other.id), "")
        self.store.set(shortcuts.key(self.action.id), default)
        self.refresh()


class KeysPage(QWidget):
    def __init__(self, store):
        super().__init__()
        self.store = store
        self.rows = {a.id: ShortcutRow(store, a) for a in shortcuts.ACTIONS}
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        hint = QLabel("Нажмите на сочетание, чтобы изменить · Esc — отмена записи")
        hint.setProperty("text_style", "meta")
        layout.addWidget(hint)
        columns = [[], []]
        for index, groups in enumerate((shortcuts.GROUPS[:2], shortcuts.GROUPS[2:])):
            for group in groups:
                columns[index].append(group_label(group))
                columns[index].extend(
                    r for a, r in zip(shortcuts.ACTIONS, self.rows.values()) if a.group == group
                )
        columns[1].append(self._transfer_buttons())
        self._columns = two_columns(*columns)
        layout.addWidget(self._columns)
        store.changed.connect(self._on_changed)

    def _transfer_buttons(self):
        box = QWidget()
        layout = QVBoxLayout(box)
        layout.setContentsMargins(0, 12, 0, 0)
        buttons = QHBoxLayout()
        buttons.setSpacing(8)
        self.import_button = QPushButton("Импорт")
        self.import_button.setIcon(material_icon("file_upload"))
        self.import_button.clicked.connect(self.import_file)
        self.export_button = QPushButton("Экспорт")
        self.export_button.setIcon(material_icon("file_download"))
        self.export_button.clicked.connect(self.export_file)
        buttons.addWidget(self.import_button)
        buttons.addWidget(self.export_button)
        buttons.addStretch(1)
        layout.addLayout(buttons)
        self.status = QLabel()
        self.status.setProperty("text_style", "meta")
        self.status.setWordWrap(True)
        self.status.setVisible(False)
        layout.addWidget(self.status)
        return box

    def _say(self, text):
        self.status.setText(text)
        self.status.setVisible(bool(text))

    def _on_changed(self, name):
        if name.startswith("keys."):
            for row in self.rows.values():
                row.refresh()

    def export_file(self):
        path, _filter = QFileDialog.getSaveFileName(
            self, tr("Экспорт сочетаний"), "seohead-shortcuts.json", "JSON (*.json)"
        )
        if path:
            try:
                Path(path).write_text(
                    json.dumps(shortcuts.export_bindings(self.store), ensure_ascii=False, indent=2),
                    encoding="utf-8",
                )
                self._say(tr("Сочетания сохранены в файл"))
            except OSError as exc:
                self._say(trf("Не удалось записать файл: {reason}", reason=exc.strerror or exc))

    def import_file(self):
        path, _filter = QFileDialog.getOpenFileName(
            self, tr("Импорт сочетаний"), "", "JSON (*.json)"
        )
        if path:
            try:
                error = shortcuts.import_bindings(
                    self.store, json.loads(Path(path).read_text(encoding="utf-8"))
                )
            except (OSError, ValueError) as exc:
                error = trf(
                    "Не удалось прочитать файл: {reason}",
                    reason=getattr(exc, "strerror", None) or exc,
                )
            self._say(
                trf("Не импортировано. {error}", error=error)
                if error
                else tr("Сочетания загружены из файла")
            )


def build_page(store, context):
    return KeysPage(store)
