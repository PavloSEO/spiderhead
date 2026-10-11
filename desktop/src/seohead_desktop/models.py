"""Bounded Qt presentation models for retained core projections."""

from PyQt5.QtCore import QAbstractTableModel, Qt
from PyQt5.QtGui import QColor

from .ui.presentation import field_text, state_text, theme_tokens, value_text


class UrlModel(QAbstractTableModel):
    columns = (
        ("URL", "url"),
        ("HTTP", "status"),
        ("Тип", "type"),
        ("Индексация", "indexability"),
        ("Title", "title"),
        ("Проблемы", "issues"),
    )

    def __init__(self, rows=(), parent=None):
        super().__init__(parent)
        self.rows = list(rows)

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return len(self.columns)

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid():
            return None
        value = self.rows[index.row()].get(self.columns[index.column()][1])
        if role in (Qt.DisplayRole, Qt.ToolTipRole):
            if self.columns[index.column()][1] == "indexability":
                return state_text(value)
            return value_text(value)
        if role == Qt.TextAlignmentRole and index.column() in (1, 5):
            return int(Qt.AlignRight | Qt.AlignVCenter)
        return None

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if role == Qt.DisplayRole and orientation == Qt.Horizontal:
            return self.columns[section][0]
        return None

    def replace(self, rows):
        self.beginResetModel()
        self.rows = list(rows)
        self.endResetModel()


class RecordModel(QAbstractTableModel):
    """A small typed projection of core JSON rows, never a backing store."""

    def __init__(self, columns, rows=(), parent=None):
        super().__init__(parent)
        self.columns = tuple(columns)
        self.rows = list(rows)

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return len(self.columns)

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid():
            return None
        key = self.columns[index.column()][1]
        value = self.rows[index.row()].get(key)
        if role == Qt.DisplayRole:
            if value is None:
                return "Не измерено"
            return field_text(key, value)
        if role == Qt.ToolTipRole:
            return self.rows[index.row()].get(key + "_tooltip", value_text(value))
        if role == Qt.TextAlignmentRole and isinstance(value, (int, float)) and not isinstance(value, bool):
            return int(Qt.AlignRight | Qt.AlignVCenter)
        if role == Qt.ForegroundRole and key in {"state", "lifecycle"}:
            color = "error" if value in {"failed", "error", "rejected"} else "primary" if value in {"running", "starting", "queued"} else "on_surface_variant"
            return QColor(theme_tokens()["colors"][color])
        return None

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        if role == Qt.DisplayRole and orientation == Qt.Horizontal:
            return self.columns[section][0]
        return None

    def replace(self, rows):
        self.beginResetModel()
        self.rows = list(rows)
        self.endResetModel()
