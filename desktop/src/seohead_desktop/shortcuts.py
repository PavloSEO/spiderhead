"""Keyboard shortcut registry: actions, defaults, validation, conflicts and display parts.

Bindings live in the settings store as ``keys.<action id>`` strings in ``QKeySequence.PortableText``
(empty string = unassigned). The shell applies them with ``sequences(store)`` to QShortcut objects.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass

from PyQt5.QtCore import Qt
from PyQt5.QtGui import QKeySequence

from .i18n import tr, trf
from .settings_store import Setting

GROUPS = ("Навигация", "Сканы", "Таблицы", "Справка")
INVALID = "Недопустимое сочетание"
NEEDS_MODIFIER = "Нужен модификатор (⌘, ⌃, ⌥) или клавиша F1–F12"


@dataclass(frozen=True)
class Action:
    id: str
    title: str
    group: str
    default: str
    fixed: tuple = ()   # not editable; ``default`` is only the label and ``fixed`` lists the sequences it occupies


ACTIONS = (
    Action("palette", "Действия и переходы", "Навигация", "Ctrl+K"),
    Action("new_scan", "Новый скан", "Навигация", "Ctrl+N"),
    Action("new_tab", "Новая вкладка", "Навигация", "Ctrl+T"),
    Action("close_tab", "Закрыть вкладку", "Навигация", "Ctrl+W"),
    Action("sections", "Разделы 1–4", "Навигация", "Ctrl+1…4", tuple(f"Ctrl+{n}" for n in range(1, 5))),  # Работа · Сканы · URL · Проблемы
    Action("settings", "Настройки", "Навигация", "Ctrl+,"),
    Action("stop_scan", "Остановить скан", "Сканы", "Ctrl+Backspace"),
    Action("pause_scan", "Приостановить / продолжить", "Сканы", "Ctrl+."),
    Action("recheck_url", "Перепроверить URL", "Сканы", "Ctrl+R"),
    Action("find_in_table", "Найти в таблице", "Таблицы", "Ctrl+F"),
    Action("expand_table", "Развернуть таблицу", "Таблицы", "Ctrl+Alt+Return"),
    Action("copy_tsv", "Копировать как TSV", "Таблицы", "Ctrl+Alt+C"),
    Action("columns", "Колонки", "Таблицы", ""),   # unassigned: the design's Ctrl+Shift+C is already copy_url's default
    Action("copy_url", "Копировать URL", "Таблицы", "Ctrl+Shift+C"),
    Action("url_history", "Назад / вперёд по URL", "Таблицы", "Ctrl+[ ]", ("Ctrl+[", "Ctrl+]")),
    Action("help", "Справка", "Справка", "F1"),
    Action("all_shortcuts", "Показать все сочетания", "Справка", "Ctrl+/"),
)
BY_ID = {a.id: a for a in ACTIONS}
_MODIFIERS = {Qt.Key_Control, Qt.Key_Shift, Qt.Key_Alt, Qt.Key_Meta, Qt.Key_AltGr, Qt.Key_unknown}


def key(action_id):
    return f"keys.{action_id}"


def normalise(text):
    """Return (portable single-chord text, '') or (None, error). Empty text means unassigned."""
    text = (text or "").strip()
    if not text:
        return "", ""
    sequence = QKeySequence(text, QKeySequence.PortableText)
    if sequence.count() != 1 or sequence[0] == 0:
        return None, INVALID
    chord = sequence[0]
    base = chord & ~int(Qt.KeyboardModifierMask)
    has_modifier = chord & int(Qt.ControlModifier | Qt.AltModifier | Qt.MetaModifier)
    if base in _MODIFIERS or not (has_modifier or Qt.Key_F1 <= base <= Qt.Key_F35):
        return None, INVALID if base in _MODIFIERS else NEEDS_MODIFIER
    return sequence.toString(QKeySequence.PortableText), ""


class ShortcutSetting(Setting):
    """Store entry holding one chord; the error shown under the row is the specific one."""

    def validate(self, value):
        if not isinstance(value, str):
            return False, value
        normal, _error = normalise(value)
        return (normal is not None), (normal if normal is not None else value)


def schema():
    return tuple(ShortcutSetting(key(a.id), a.default, str, error=INVALID) for a in ACTIONS if not a.fixed)


def bindings(store):
    """action id -> portable sequence ('' when unassigned); fixed actions keep their label."""
    return {a.id: (a.default if a.fixed else store.get(key(a.id))) for a in ACTIONS}


def sequences(store):
    """action id -> QKeySequence for the shell to attach to QShortcut (fixed and unassigned skipped)."""
    return {a.id: QKeySequence(store.get(key(a.id)), QKeySequence.PortableText) for a in ACTIONS
            if not a.fixed and store.get(key(a.id))}


def conflict(store, action_id, portable):
    """Return the Action that already uses ``portable`` (normalised), or None."""
    if not portable:
        return None
    for action in ACTIONS:
        if action.id == action_id:
            continue
        taken = action.fixed or (store.get(key(action.id)),)
        if portable in taken:
            return action
    return None


def set_binding(store, action_id, portable, replace=False):
    """Store a binding. Return (None, '') on success, else (conflicting Action or None, message)."""
    normal, error = normalise(portable)
    if normal is None:
        return None, error
    other = conflict(store, action_id, normal)
    if other is not None:
        if not replace or other.fixed:
            return other, trf("{keys} уже занято: «{title}»", keys=display(normal), title=other.title)
        store.set(key(other.id), "")
    store.set(key(action_id), normal)
    return None, ""


_MAC = sys.platform == "darwin"
_MAC_MODIFIERS = (("Meta", "⌃"), ("Alt", "⌥"), ("Shift", "⇧"), ("Ctrl", "⌘"))
_KEYS = {"Left": "←", "Right": "→", "Up": "↑", "Down": "↓"}
_MAC_KEYS = {"Return": "↵", "Enter": "↵", "Backspace": "⌫", "Del": "⌦", "Tab": "⇥"}


def parts(portable, mac=_MAC):
    """Display parts for key caps, e.g. 'Ctrl+Shift+C' -> ['⌘', '⇧', 'C'] on macOS (system order)."""
    if not portable:
        return []
    head, _sep, last = portable.rpartition("+")
    if not last and head:  # the key itself is "+"
        mods, last = head[:-1].split("+") if len(head) > 1 else [], "+"
    else:
        mods = head.split("+") if head else []
    if mac:
        mods = [sym for name, sym in _MAC_MODIFIERS if name in mods]
        last = _MAC_KEYS.get(last, last)
    return [*mods, _KEYS.get(last, last)]


def display(portable, mac=_MAC):
    return ("" if mac else "+").join(parts(portable, mac))


def export_bindings(store):
    return {"version": 1, "bindings": {a.id: store.get(key(a.id)) for a in ACTIONS if not a.fixed}}


def import_bindings(store, data):
    """Apply an exported mapping atomically. Return '' on success or the Russian reason (nothing changed)."""
    incoming = data.get("bindings") if isinstance(data, dict) else None
    if not isinstance(incoming, dict):
        return tr("Файл не похож на экспорт сочетаний")
    merged = {a.id: store.get(key(a.id)) for a in ACTIONS if not a.fixed}
    for action_id, text in incoming.items():
        if action_id not in merged:
            continue
        normal, error = normalise(text) if isinstance(text, str) else (None, INVALID)
        if normal is None:
            return trf("«{title}»: {error}", title=BY_ID[action_id].title, error=error)
        merged[action_id] = normal
    taken = {}
    fixed = {seq: a for a in ACTIONS for seq in a.fixed}
    for action_id, sequence in merged.items():
        other = fixed.get(sequence) or (BY_ID[taken[sequence]] if sequence in taken else None)
        if sequence and other is not None:
            return trf("{keys} занято дважды: «{first}» и «{second}»", keys=display(sequence), first=BY_ID[action_id].title,
                       second=other.title)
        taken[sequence] = action_id
    for action_id, sequence in merged.items():
        store.set(key(action_id), sequence)
    return ""
