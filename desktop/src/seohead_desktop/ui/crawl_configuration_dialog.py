"""Native typed crawl settings editor; explicit drafts and contained JSON profiles."""

from __future__ import annotations

import json
import os
from copy import deepcopy
from pathlib import Path

from PyQt5.QtCore import Qt, pyqtSignal
from PyQt5.QtWidgets import (
    QAbstractItemView,
    QCheckBox,
    QComboBox,
    QDialog,
    QDialogButtonBox,
    QFileDialog,
    QFormLayout,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QPlainTextEdit,
    QPushButton,
    QStackedWidget,
    QTableView,
    QVBoxLayout,
)

from ..crawl_configuration import describe_controls, validate_overrides
from .components import PageModel

PROFILE_SCHEMA = "seohead.desktop.crawl-profile.v1"
MAX_PROFILE_BYTES = 256 * 1024


def _unique_object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("Profile contains a duplicate JSON key")
        value[key] = item
    return value


class CrawlConfigurationDialog(QDialog):
    """Return copied overrides only after Apply; opening the dialog performs no I/O."""

    overrides_accepted = pyqtSignal(dict)

    def __init__(
        self,
        descriptor,
        parent=None,
        *,
        project_directory=None,
        overrides=None,
        base_values=None,
        input_mode="site",
        storage_backend="sqlite",
    ):
        super().__init__(parent)
        self.descriptor = deepcopy(descriptor)
        self.context = {
            "base_values": deepcopy(base_values),
            "input_mode": input_mode,
            "storage_backend": storage_backend,
        }
        rows = describe_controls(
            descriptor, input_mode=input_mode, storage_backend=storage_backend
        )
        self.controls = {row["path"]: row for row in rows}
        initial = validate_overrides(descriptor, overrides or {}, **self.context)
        self._working = deepcopy(initial)
        self._accepted_overrides = deepcopy(initial)
        self.project_directory = Path(project_directory) if project_directory else None
        self._active_path = None
        self._dirty = False
        self._loading = False
        self.setWindowTitle("Параметры нового скана")
        self.resize(900, 700)
        self.setMinimumSize(640, 550)
        layout = QVBoxLayout(self)
        intro = QLabel(
            "Измените нужные параметры и нажмите «Применить». Окончательная проверка ядра выполняется перед явным запуском скана."
        )
        intro.setWordWrap(True)
        layout.addWidget(intro)
        chooser = QFormLayout()
        self.group = QComboBox()
        self.group.setObjectName("configurationGroup")
        self.group.setAccessibleName("Группа параметров")
        self.group.addItem("Все группы", None)
        for group in sorted({row["group"] for row in rows}):
            self.group.addItem(group, group)
        chooser.addRow("Группа", self.group)
        self.search = QLineEdit()
        self.search.setObjectName("configurationSearch")
        self.search.setAccessibleName("Найти параметр")
        self.search.setPlaceholderText("Название или описание параметра")
        self.search.setClearButtonEnabled(True)
        chooser.addRow("Поиск", self.search)
        self.path = QComboBox()
        self.path.setObjectName("configurationPath")
        self.path.setAccessibleName("Параметр сканирования")
        self.path.setSizeAdjustPolicy(QComboBox.AdjustToMinimumContentsLengthWithIcon)
        self.path.setMinimumContentsLength(24)
        chooser.addRow("Параметр", self.path)
        layout.addLayout(chooser)
        self.description = QLabel()
        self.description.setWordWrap(True)
        self.description.setTextFormat(Qt.PlainText)
        layout.addWidget(self.description)
        self.availability = QLabel()
        self.availability.setWordWrap(True)
        self.availability.setTextFormat(Qt.PlainText)
        layout.addWidget(self.availability)
        self.editor_stack = QStackedWidget()
        self.bool_editor = QCheckBox("Включено")
        self.choice_editor = QComboBox()
        self.text_editor = QLineEdit()
        self.text_editor.setMaxLength(4096)
        self.int_editor = QLineEdit()
        self.int_editor.setMaxLength(32)
        self.int_editor.setPlaceholderText("Целое число")
        self.float_editor = QLineEdit()
        self.float_editor.setMaxLength(64)
        self.float_editor.setPlaceholderText("Число, например 0.5")
        self.list_editor = QPlainTextEdit()
        self.list_editor.setPlaceholderText("По одному значению на строку")
        self.list_editor.setMaximumHeight(120)
        self._editors = {
            "bool": self.bool_editor,
            "choice": self.choice_editor,
            "str": self.text_editor,
            "int": self.int_editor,
            "float": self.float_editor,
            "list": self.list_editor,
        }
        for editor in self._editors.values():
            self.editor_stack.addWidget(editor)
        layout.addWidget(self.editor_stack)
        self.stage_button = QPushButton("Добавить в изменения")
        self.stage_button.setObjectName("configurationStageButton")
        self.stage_button.setDefault(True)
        self.stage_button.clicked.connect(self.stage_current)
        layout.addWidget(self.stage_button, 0, Qt.AlignLeft)
        self.override_caption = QLabel()
        layout.addWidget(self.override_caption)
        self.override_model = PageModel(
            (("path", "Параметр"), ("value", "Новое значение")), self
        )
        self.override_table = QTableView()
        self.override_table.setObjectName("configurationOverrides")
        self.override_table.setAccessibleName("Изменения параметров")
        self.override_table.setModel(self.override_model)
        self.override_table.setSelectionBehavior(QAbstractItemView.SelectRows)
        self.override_table.setSelectionMode(QAbstractItemView.SingleSelection)
        self.override_table.setEditTriggers(QAbstractItemView.NoEditTriggers)
        self.override_table.verticalHeader().hide()
        self.override_table.horizontalHeader().setSectionResizeMode(QHeaderView.Stretch)
        self.override_table.doubleClicked.connect(
            lambda index: self.select_path(
                self.override_model.rows[index.row()]["path"]
            )
        )
        layout.addWidget(self.override_table, 1)
        actions = QHBoxLayout()
        self.remove_button = QPushButton("Убрать выбранное изменение")
        self.remove_button.clicked.connect(self.remove_selected)
        actions.addWidget(self.remove_button)
        actions.addStretch()
        self.load_button = QPushButton("Загрузить профиль…")
        self.save_button = QPushButton("Сохранить новый профиль…")
        self.load_button.setEnabled(self.project_directory is not None)
        self.save_button.setEnabled(self.project_directory is not None)
        self.load_button.clicked.connect(self.choose_load_profile)
        self.save_button.clicked.connect(self.choose_save_profile)
        actions.addWidget(self.load_button)
        actions.addWidget(self.save_button)
        layout.addLayout(actions)
        self.feedback = QLabel()
        self.feedback.setObjectName("configurationFeedback")
        self.feedback.setWordWrap(True)
        self.feedback.setTextFormat(Qt.PlainText)
        layout.addWidget(self.feedback)
        self.buttons = QDialogButtonBox(
            QDialogButtonBox.Apply | QDialogButtonBox.Cancel
        )
        self.apply_button = self.buttons.button(QDialogButtonBox.Apply)
        self.apply_button.setText("Применить")
        self.buttons.button(QDialogButtonBox.Cancel).setText("Отмена")
        self.apply_button.setObjectName("configurationApplyButton")
        self.apply_button.setAutoDefault(False)
        self.apply_button.clicked.connect(self.accept)
        self.buttons.rejected.connect(self.reject)
        layout.addWidget(self.buttons)
        self.group.currentIndexChanged.connect(self._refresh_paths)
        self.search.textChanged.connect(self._refresh_paths)
        self.path.currentIndexChanged.connect(self._load_control)
        self.bool_editor.toggled.connect(self._mark_dirty)
        self.choice_editor.currentIndexChanged.connect(self._mark_dirty)
        for editor in (self.text_editor, self.int_editor, self.float_editor):
            editor.textChanged.connect(self._mark_dirty)
        self.list_editor.textChanged.connect(self._mark_dirty)
        self._refresh_paths()
        self._refresh_overrides()

    def get_overrides(self):
        return deepcopy(self._accepted_overrides)

    def select_path(self, path):
        if path not in self.controls:
            raise ValueError("Unknown setting path")
        self.group.setCurrentIndex(0)
        self.search.clear()
        self.path.setCurrentIndex(self.path.findData(path))

    def _refresh_paths(self, *_args):
        selected = self.path.currentData()
        group, query = self.group.currentData(), self.search.text().casefold()
        self.path.blockSignals(True)
        self.path.clear()
        for path, row in self.controls.items():
            if group and row["group"] != group:
                continue
            if query and query not in (path + " " + row["description"]).casefold():
                continue
            self.path.addItem(
                path + (" · недоступно" if not row["editable"] else ""), path
            )
        index = self.path.findData(selected)
        self.path.setCurrentIndex(max(index, 0))
        self.path.blockSignals(False)
        self._load_control()

    def _load_control(self, *_args):
        self._active_path = self.path.currentData()
        self._dirty = False
        self._loading = True
        if self._active_path is None:
            self.description.setText("Параметры не найдены.")
            self.availability.clear()
            self.editor_stack.setEnabled(False)
            self.stage_button.setEnabled(False)
            self._loading = False
            self._refresh_validation()
            return
        row = self.controls[self._active_path]
        value = self._working.get(
            self._active_path,
            (self.context["base_values"] or {}).get(self._active_path, row["default"]),
        )
        kind = "choice" if "choices" in row else row["type"]
        editor = self._editors.get(kind, self.text_editor)
        editor.setAccessibleName("Значение " + self._active_path)
        self.editor_stack.setCurrentWidget(editor)
        self.editor_stack.setEnabled(row["editable"])
        self.stage_button.setEnabled(row["editable"])
        self.description.setText(row["description"])
        if row["editable"]:
            origin = (
                "Изменение в черновике"
                if self._active_path in self._working
                else "Исходное значение; не добавлено в изменения"
            )
            self.availability.setText(
                origin + (" · влияет на результаты" if row["results_affecting"] else "")
            )
        else:
            self.availability.setText("Недоступно: " + row["unavailable_reason"])
        if kind == "bool":
            self.bool_editor.setChecked(value is True)
        elif kind == "choice":
            self.choice_editor.clear()
            self.choice_editor.addItems(row["choices"])
            self.choice_editor.setCurrentText(str(value))
        elif kind == "list":
            self.list_editor.setPlainText(
                "\n".join(str(item) for item in value)
                if isinstance(value, list)
                else "Значение скрыто"
            )
        elif kind in {"str", "int", "float"}:
            editor.setText("Значение скрыто" if value is None else str(value))
        else:
            self.text_editor.setText("Нет подключённого редактора")
        self._loading = False
        self._refresh_validation()

    def _mark_dirty(self, *_args):
        if not self._loading:
            self._dirty = True
            self._refresh_validation()

    def _editor_value(self):
        row = self.controls.get(self._active_path)
        if not row or not row["editable"]:
            raise ValueError("Этот параметр недоступен для изменения")
        if "choices" in row:
            return self.choice_editor.currentText()
        if row["type"] == "bool":
            return self.bool_editor.isChecked()
        if row["type"] == "int":
            try:
                return int(self.int_editor.text().strip())
            except ValueError:
                raise ValueError("Введите целое число") from None
        if row["type"] == "float":
            try:
                return float(self.float_editor.text().strip().replace(",", "."))
            except ValueError:
                raise ValueError("Введите число") from None
        if row["type"] == "list":
            text = self.list_editor.toPlainText()
            return [line for line in text.splitlines() if line.strip()]
        return self.text_editor.text()

    def _candidate(self):
        result = deepcopy(self._working)
        if self._dirty:
            result[self._active_path] = self._editor_value()
        return result

    def _validated_candidate(self):
        return validate_overrides(self.descriptor, self._candidate(), **self.context)

    def _refresh_validation(self):
        try:
            self._validated_candidate()
        except (ValueError, TypeError, RecursionError) as exc:
            self.feedback.setText(str(exc))
            self.apply_button.setEnabled(False)
            return False
        self.feedback.setText(
            "Черновик проверен. Применение вернёт параметры в план нового скана."
        )
        self.apply_button.setEnabled(True)
        return True

    def _refresh_overrides(self):
        self.override_model.replace(
            [
                {"path": path, "value": value}
                for path, value in sorted(self._working.items())
            ]
        )
        self.override_caption.setText(f"Явные изменения: {len(self._working)}")
        self._refresh_validation()

    def stage_current(self):
        try:
            value = self._editor_value()
        except (ValueError, TypeError) as exc:
            self.feedback.setText(str(exc))
            return False
        # Permit an incomplete cross-field draft (e.g. width before height).
        # Apply and profile saving still require the entire draft to validate.
        self._working[self._active_path] = value
        self._dirty = False
        self._refresh_overrides()
        self._load_control()
        return True

    def remove_selected(self):
        index = self.override_table.currentIndex()
        if index.isValid():
            self._working.pop(self.override_model.rows[index.row()]["path"], None)
            self._refresh_overrides()
            self._load_control()

    def accept(self):
        try:
            result = self._validated_candidate()
        except (ValueError, TypeError, RecursionError) as exc:
            self.feedback.setText(str(exc))
            return
        self._accepted_overrides = deepcopy(result)
        self.overrides_accepted.emit(deepcopy(result))
        super().accept()

    def _profile_directory(self):
        if self.project_directory is None:
            raise ValueError("Сначала выберите локальный проект")
        root = self.project_directory.resolve(strict=True)
        if not root.is_dir() or not (root / "project.json").is_file():
            raise ValueError("Выбранный проект недоступен")
        folder = root / "profiles"
        if folder.is_symlink() or (folder.exists() and not folder.is_dir()):
            raise ValueError("Каталог profiles должен находиться внутри проекта")
        return folder

    def _profile_path(self, filename):
        folder = self._profile_directory()
        path = Path(filename)
        if ".." in path.parts:
            raise ValueError("Профиль должен находиться в project/profiles")
        if not path.is_absolute():
            path = folder / path
        if (
            path.parent.resolve() != folder.resolve()
            or path.suffix.lower() != ".json"
            or path.is_symlink()
        ):
            raise ValueError("Выберите JSON-файл непосредственно в project/profiles")
        return path

    def save_profile(self, filename):
        values = self._validated_candidate()
        path = self._profile_path(filename)
        payload = {
            "schema": PROFILE_SCHEMA,
            "input_mode": self.context["input_mode"],
            "storage_backend": self.context["storage_backend"],
            "overrides": values,
        }
        encoded = (
            json.dumps(payload, ensure_ascii=False, indent=2, allow_nan=False) + "\n"
        ).encode()
        if len(encoded) > MAX_PROFILE_BYTES:
            raise ValueError("Профиль превышает допустимый размер 256 KiB")
        path.parent.mkdir(exist_ok=True)
        try:
            descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            raise ValueError(
                "Профиль уже существует. Выберите новое имя для новой версии."
            ) from None
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(encoded)
        self._working = deepcopy(values)
        self._dirty = False
        self._refresh_overrides()
        self._load_control()
        self.feedback.setText("Новый профиль сохранён: " + path.name)
        return path

    def load_profile(self, filename):
        path = self._profile_path(filename)
        flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
        with os.fdopen(os.open(path, flags), "rb") as handle:
            raw = handle.read(MAX_PROFILE_BYTES + 1)
        if len(raw) > MAX_PROFILE_BYTES:
            raise ValueError("Профиль превышает допустимый размер 256 KiB")
        try:
            payload = json.loads(raw, object_pairs_hook=_unique_object)
        except (ValueError, UnicodeError, RecursionError):
            raise ValueError("Неверный JSON профиля") from None
        if not isinstance(payload, dict) or set(payload) != {
            "schema",
            "input_mode",
            "storage_backend",
            "overrides",
        }:
            raise ValueError("Неизвестная структура профиля")
        if payload["schema"] != PROFILE_SCHEMA:
            raise ValueError("Неподдерживаемая версия профиля")
        if any(
            payload[key] != self.context[key]
            for key in ("input_mode", "storage_backend")
        ):
            raise ValueError("Профиль предназначен для другого режима или хранилища")
        values = validate_overrides(
            self.descriptor, payload["overrides"], **self.context
        )
        self._working = deepcopy(values)
        self._dirty = False
        self._refresh_overrides()
        self._load_control()
        self.feedback.setText(
            "Профиль загружен в черновик. Для возврата настроек нажмите «Применить»."
        )

    def choose_save_profile(self):
        try:
            folder = self._profile_directory()
            filename, _ = QFileDialog.getSaveFileName(
                self,
                "Сохранить новый профиль",
                str(folder / "crawl-profile.json"),
                "JSON (*.json)",
                options=QFileDialog.DontConfirmOverwrite,
            )
            if filename:
                self.save_profile(filename)
        except (OSError, RuntimeError, ValueError, TypeError) as exc:
            self.feedback.setText(str(exc))

    def choose_load_profile(self):
        try:
            folder = self._profile_directory()
            filename, _ = QFileDialog.getOpenFileName(
                self, "Загрузить профиль", str(folder), "JSON (*.json)"
            )
            if filename:
                self.load_profile(filename)
        except (OSError, RuntimeError, ValueError, TypeError) as exc:
            self.feedback.setText(str(exc))
