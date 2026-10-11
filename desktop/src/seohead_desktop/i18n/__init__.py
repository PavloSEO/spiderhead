"""RU/EN switching on the fly.

Source strings are the Russian texts used in code (keys of ``en.json``). ``tr`` translates at call time;
``retranslate`` walks a widget tree and re-applies texts, tooltips, placeholders and accessible names, remembering the
source of every text it touched so the language can be switched back and forth without losing it.
"""

from __future__ import annotations

import json
from pathlib import Path

from PyQt5.QtCore import QObject, Qt, pyqtSignal
from PyQt5.QtWidgets import (
    QAbstractButton,
    QAction,
    QComboBox,
    QGroupBox,
    QLabel,
    QLineEdit,
    QListWidget,
    QMenu,
    QTabBar,
    QWidget,
)

LANGUAGES = ("ru", "en")
_DIR = Path(__file__).resolve().parent


class _Signals(QObject):
    changed = pyqtSignal(str)


signals = _Signals()
_language = "ru"
_en = None


def _dictionary():
    global _en
    if _en is None:
        _en = json.loads((_DIR / "en.json").read_text(encoding="utf-8"))
        for source, english in list(_en.items()):  # overline labels show the upper-case form of a source text
            if "<" not in source and "{" not in source:
                _en.setdefault(source.upper(), english.upper())
    return _en


def language():
    return _language


def set_language(lang):
    global _language
    if lang not in LANGUAGES:
        raise ValueError(f"unknown language: {lang}")
    if lang != _language:
        _language = lang
        signals.changed.emit(lang)


def tr(text):
    """Translate one source string; unknown strings come back unchanged (never an empty or guessed text)."""
    if _language == "en" and isinstance(text, str):
        return _dictionary().get(text, text)
    return text


class Num:
    """A number formatted when a template is rendered: decimal comma in Russian, point in English."""

    def __init__(self, value, digits=0):
        self.value, self.digits = value, digits

    def __str__(self):
        text = f"{self.value:.{self.digits}f}"
        return text.replace(".", ",") if _language == "ru" else text


_RENDERED = {}  # rendered text -> (template, values); lets a finished label be re-rendered in the other language
_REVERSE = None


def _reverse():
    global _REVERSE
    if _REVERSE is None:
        _REVERSE = {}
        for source, english in _dictionary().items():
            _REVERSE.setdefault(english, source)
    return _REVERSE


def _source(text):
    """The Russian key of an English text (the text itself when it is not a translation)."""
    return _reverse().get(text, text)


_ACTIVE = set()  # texts being re-rendered right now: a value that names itself is plain text


def _value(value):
    if isinstance(value, str):
        entry = None if value in _ACTIVE else _RENDERED.get(value)
        if entry is None:
            return tr(_source(value))
        _ACTIVE.add(value)
        try:
            return _render(*entry)
        finally:
            _ACTIVE.discard(value)
    return str(value) if isinstance(value, Num) else value


def _render(template, values):
    text = tr(template).format(**{name: _value(value) for name, value in values.items()})
    if text not in values.values():  # an identity rendering would only point at itself
        if len(_RENDERED) > 4000:
            _RENDERED.pop(next(iter(_RENDERED)))
        _RENDERED[text] = (template, values)
    return text


def trf(template, **values):
    """Translate a template (the whole template is the dictionary key) and fill it in.

    String values that are source texts are translated too; the result is remembered, so a label that shows it is
    re-rendered, not left in the old language, when the language changes.
    """
    return _render(template, values)


def joined(separator, parts):
    """``separator.join(parts)`` that stays translatable: every part is translated and the result can be re-rendered."""
    parts = list(parts)
    template = separator.join(f"{{p{index}}}" for index in range(len(parts)))
    return _render(template, {f"p{index}": part for index, part in enumerate(parts)})


def known(text):
    return text in _dictionary()


def convert(text):
    """``text`` (in either language, or a rendered template) in the current language."""
    entry = _RENDERED.get(text)
    return _render(*entry) if entry else tr(_source(text))


# ---- tree pass ---------------------------------------------------------------------------------------------------

_MEMORY = {}  # (id(owner), slot) -> (source, applied)


def _apply(key, current, setter):
    """Re-apply one text. Text changed by the code since the last pass is a new source, never overwritten."""
    source, applied = _MEMORY.get(key, (None, None))
    if source is None or current != applied:
        source = current
    wanted = convert(source) if source else source
    if wanted != current:
        setter(wanted)
    _MEMORY[key] = (source, wanted)


def _widget(widget):
    ident = id(widget)
    _apply((ident, "tip"), widget.toolTip(), widget.setToolTip)
    _apply((ident, "acc"), widget.accessibleName(), widget.setAccessibleName)
    if isinstance(widget, QLabel):
        if not widget.pixmap():
            _apply((ident, "text"), widget.text(), widget.setText)
    elif isinstance(widget, QAbstractButton):
        _apply((ident, "text"), widget.text(), widget.setText)
    elif isinstance(widget, QLineEdit):
        _apply((ident, "ph"), widget.placeholderText(), widget.setPlaceholderText)
    elif isinstance(widget, QGroupBox):
        _apply((ident, "text"), widget.title(), widget.setTitle)
    elif isinstance(widget, QTabBar):
        for index in range(widget.count()):
            _apply((ident, "text", index), widget.tabText(index), lambda t, i=index: widget.setTabText(i, t))
            _apply((ident, "tip", index), widget.tabToolTip(index), lambda t, i=index: widget.setTabToolTip(i, t))
    elif isinstance(widget, QComboBox):
        for index in range(widget.count()):
            _apply((ident, "text", index), widget.itemText(index), lambda t, i=index: widget.setItemText(i, t))
    elif isinstance(widget, QListWidget):
        for row in range(widget.count()):
            item = widget.item(row)
            _apply((ident, "text", row), item.text(), item.setText)
            _apply((ident, "tip", row), item.toolTip(), item.setToolTip)
            spoken = item.data(Qt.AccessibleTextRole)
            if isinstance(spoken, str):
                _apply((ident, "acc", row), spoken, lambda t, i=item: i.setData(Qt.AccessibleTextRole, t))


def _action(action):
    ident = id(action)
    _apply((ident, "text"), action.text(), action.setText)
    _apply((ident, "tip"), action.toolTip(), action.setToolTip)
    if action.menu() is not None:
        _menu(action.menu())


def _menu(menu):
    _apply((id(menu), "title"), menu.title(), menu.setTitle)
    for action in menu.actions():
        _action(action)


def retranslate(root):
    """Apply the current language to ``root`` and everything under it (widgets, menus, actions)."""
    if isinstance(root, QWidget):
        _widget(root)
        if root.isWindow():
            _apply((id(root), "title"), root.windowTitle(), root.setWindowTitle)
        for child in root.findChildren(QWidget):
            _widget(child)
        for menu in root.findChildren(QMenu):
            _menu(menu)
    for action in root.findChildren(QAction):
        _action(action)
