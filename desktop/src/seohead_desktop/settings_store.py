"""Per-computer application preferences (never written into a project file).

Every setting is declared once with a default and a validator. Sections of the Settings dialog
declare their own ``SCHEMA`` (see ``ui/settings``); the store merges them. Without a QSettings
backend the store is in-memory (tests, ``--no-settings``).
"""

from __future__ import annotations

from dataclasses import dataclass

from PyQt5.QtCore import QObject, pyqtSignal


@dataclass(frozen=True)
class Setting:
    key: str
    default: object
    kind: type = str                # bool | int | float | str
    minimum: float | None = None
    maximum: float | None = None
    choices: tuple | None = None
    error: str = ""                 # Russian message shown under the field when validation fails

    def validate(self, value):
        """Return (ok, normalised_value)."""
        try:
            if self.kind is bool:
                if not isinstance(value, bool):
                    return False, value
                return True, value
            if self.kind in (int, float):
                if isinstance(value, bool):
                    return False, value
                number = self.kind(str(value).replace(" ", "").replace(",", ".")) if isinstance(value, str) else self.kind(value)
                if (self.minimum is not None and number < self.minimum) or (self.maximum is not None and number > self.maximum):
                    return False, value
                return True, number
            value = str(value)
            if self.choices is not None and value not in self.choices:
                return False, value
            return True, value
        except (TypeError, ValueError):
            return False, value


class AppSettings(QObject):
    changed = pyqtSignal(str)

    def __init__(self, backend=None, schema=()):
        super().__init__()
        self._backend = backend
        self._schema = {}
        self._memory = {}
        self.register(schema)

    def register(self, schema):
        for setting in schema:
            self._schema[setting.key] = setting

    def definition(self, key):
        return self._schema[key]

    def keys(self, prefix=""):
        return [k for k in self._schema if k.startswith(prefix)]

    def get(self, key):
        setting = self._schema[key]
        raw = self._backend.value(key, None) if self._backend is not None else self._memory.get(key)
        if raw is None:
            return setting.default
        if setting.kind is bool and isinstance(raw, str):  # QSettings may hand back "true"/"false"
            raw = raw.lower() == "true"
        ok, value = setting.validate(raw)
        return value if ok else setting.default

    def set(self, key, value):
        """Store a valid value and return None, or return the Russian error message (nothing stored)."""
        setting = self._schema[key]
        ok, normal = setting.validate(value)
        if not ok:
            return setting.error or "Недопустимое значение"
        if self.get(key) == normal and self.is_set(key):
            return None
        if self._backend is not None:
            self._backend.setValue(key, normal)
        else:
            self._memory[key] = normal
        self.changed.emit(key)
        return None

    def is_set(self, key):
        raw = self._backend.value(key, None) if self._backend is not None else self._memory.get(key)
        return raw is not None

    def reset(self, prefix):
        for key in self.keys(prefix):
            if self.is_set(key):
                if self._backend is not None:
                    self._backend.remove(key)
                else:
                    self._memory.pop(key, None)
                self.changed.emit(key)
