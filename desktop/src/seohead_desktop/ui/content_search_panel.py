"""Native controls over the core's retained content-search projection."""

from PyQt5.QtCore import Qt, pyqtSignal
from PyQt5.QtWidgets import (
    QAbstractItemView,
    QCheckBox,
    QComboBox,
    QFormLayout,
    QGridLayout,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QMenu,
    QPushButton,
    QTableView,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from ..content_search import SEARCH_PRESETS
from .components import PageModel, material_icon
from .presentation import (
    ElidedLabel,
    ProjectEmptyState,
    StateBadge,
    content_spacing,
    short_run_id,
    theme_tokens,
    value_text,
)


class _SearchModel(PageModel):
    def data(self, index, role=Qt.DisplayRole):
        if role == Qt.ToolTipRole and index.isValid():
            raw = self.rows[index.row()].get("_source", {})
            key = self.columns[index.column()][0]
            if key in raw:
                return value_text(raw[key])
        return super().data(index, role)


class ContentSearchPanel(QWidget):
    openProjectRequested = pyqtSignal()
    searchRequested = pyqtSignal(dict)
    pageRequested = pyqtSignal(int)
    cancelRequested = pyqtSignal()
    helpRequested = pyqtSignal()

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setObjectName("bodySearchPanel")
        self._available = self._selected = self._busy = False
        self._offset = 0
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        self.empty_project = ProjectEmptyState()
        self.empty_project.openRequested.connect(self.openProjectRequested)
        outer.addWidget(self.empty_project)
        self.content = QWidget()
        outer.addWidget(self.content, 1)
        layout = QVBoxLayout(self.content)
        layout.setContentsMargins(32, 24, 32, 24)
        layout.setSpacing(12)
        self.empty_project.hide()
        heading = QHBoxLayout()
        title = QLabel("Поиск в сохранённом HTML")
        title.setObjectName("sectionTitle")
        heading.addWidget(title, 1)
        self.state = StateBadge()
        heading.addWidget(self.state)
        layout.addLayout(heading)
        self.context = ElidedLabel("Выберите сохранённый скан")
        self.context.setObjectName("metadata")
        layout.addWidget(self.context)
        presets = QHBoxLayout()
        self.preset_buttons = []
        for preset in SEARCH_PRESETS:
            button = QPushButton(preset["label"])
            button.setProperty("role", "quiet")
            button.setIcon(material_icon("search"))
            button.setToolTip("Заполнить форму; поиск запускается отдельной кнопкой")
            button.clicked.connect(lambda checked=False, identifier=preset["id"]: self.set_preset(identifier))
            presets.addWidget(button)
            self.preset_buttons.append(button)
        presets.addStretch()
        layout.addLayout(presets)
        query_row = QHBoxLayout()
        self.query = QLineEdit()
        self.query.setObjectName("bodySearchQuery")
        self.query.setAccessibleName("Буквальный текст для поиска в сохранённом скане")
        self.query.setPlaceholderText("Буквальная строка: GTM-, фрагмент кода или текст…")
        self.query.setMaxLength(512)
        self.query.setClearButtonEnabled(True)
        self.start = QPushButton("Найти в скане")
        self.start.setObjectName("bodySearchStart")
        self.start.setProperty("role", "primary")
        self.start.setIcon(material_icon("search", theme_tokens()["colors"]["on_primary"]))
        self.cancel = QPushButton("Отменить поиск")
        self.cancel.setIcon(material_icon("stop"))
        self.cancel.clicked.connect(self.cancelRequested)
        self.options = QToolButton()
        self.options.setIcon(material_icon("tune"))
        self.options.setToolTip("Пресеты, регистр и фрагменты")
        self.options.setAccessibleName("Параметры поиска")
        self.options.setPopupMode(QToolButton.InstantPopup)
        self.options_menu = QMenu(self.options)
        for preset in SEARCH_PRESETS:
            self.options_menu.addAction(preset["label"], lambda checked=False, identifier=preset["id"]: self.set_preset(identifier))
        self.options_menu.addSeparator()
        self.options.setMenu(self.options_menu)
        query_row.addWidget(self.query, 1)
        query_row.addWidget(self.options)
        query_row.addWidget(self.start)
        query_row.addWidget(self.cancel)
        layout.addLayout(query_row)
        form = QFormLayout()
        form.setFieldGrowthPolicy(QFormLayout.AllNonFixedFieldsGrow)
        self.scope = self.combo("bodySearchScope", [("Код <head>", "head_markup"), ("Весь HTML", "raw_html"), ("Текст <body>", "body_text"), ("Код CSS-элемента", "selector_markup")])
        self.mode = self.combo("bodySearchMode", [("Содержит строку", "contains"), ("Не содержит строку", "not_contains")])
        self.representation = self.combo("bodySearchRepresentation", [("Исходный HTML", "static"), ("Сохранённый DOM после JavaScript", "rendered")])
        self.selector = QLineEdit()
        self.selector.setObjectName("bodySearchSelector")
        self.selector.setPlaceholderText("CSS-селектор, например head script")
        self.selector.setMaxLength(4096)
        options = QHBoxLayout()
        for widget in (self.scope, self.mode, self.representation):
            options.addWidget(widget, 1)
        form.addRow("Где и как", options)
        form.addRow("CSS-селектор", self.selector)
        self.selector_label = form.labelForField(self.selector)
        layout.addLayout(form)
        self.case_sensitive = QCheckBox("Учитывать регистр")
        self.snippets = QCheckBox("Показывать фрагменты содержимого")
        flags = QHBoxLayout()
        flags.addWidget(self.case_sensitive)
        flags.addWidget(self.snippets)
        flags.addStretch()
        layout.addLayout(flags)
        for checkbox in (self.case_sensitive, self.snippets):
            action = self.options_menu.addAction(checkbox.text())
            action.setCheckable(True)
            action.toggled.connect(checkbox.setChecked)
            checkbox.toggled.connect(action.setChecked)
        self.options_menu.addSeparator()
        self.options_menu.addAction("Как читать результаты…", self.helpRequested.emit)
        help_text = self.help_text = QLabel("Поиск читает сохранённые тела выбранного скана. Наличие маркера не подтверждает работу тега. Недоступный HTML или DOM не считается отсутствием строки.")
        help_text.setObjectName("metadata")
        help_text.setWordWrap(True)
        layout.addWidget(help_text)
        self.coverage = QLabel("Измерений пока нет")
        self.coverage.setObjectName("searchCoverage")
        self.coverage.setWordWrap(True)
        self.coverage.hide()
        self.coverage_fields = (("present_documents", "Содержат строку"), ("absent_documents", "Без строки"), ("unavailable_documents", "Не проверены"), ("non_html_documents", "Не HTML"))
        self.coverage_metrics = QWidget()
        metric_layout = QGridLayout(self.coverage_metrics)
        metric_layout.setContentsMargins(0, 12, 0, 12)
        metric_layout.setHorizontalSpacing(24)
        self.coverage_values = {}
        for column, (key, title) in enumerate(self.coverage_fields):
            label = QLabel(title)
            label.setObjectName("searchMetricLabel")
            value = QLabel("—")
            value.setObjectName("searchMetricValue")
            self.coverage_values[key] = value
            metric_layout.addWidget(label, 0, column)
            metric_layout.addWidget(value, 1, column)
            metric_layout.setColumnStretch(column, 1)
        layout.addWidget(self.coverage_metrics)
        self.result_context = ElidedLabel()
        self.result_context.setObjectName("metadata")
        layout.addWidget(self.result_context)
        self.message = QLabel()
        self.message.setObjectName("searchNotice")
        self.message.setWordWrap(True)
        self.message.setTextFormat(Qt.PlainText)
        layout.addWidget(self.message)
        layout.addStretch(0)
        self.empty_stretch_index = layout.count() - 1
        self.model = _SearchModel((("url", "URL"), ("presence_label", "Строка в документе"), ("capture_mode", "Источник"), ("reason", "Основание"), ("snippet", "Фрагмент")), self)
        self.table = QTableView()
        self.table.setObjectName("bodySearchResults")
        self.table.setModel(self.model)
        self.table.setSelectionBehavior(QAbstractItemView.SelectRows)
        self.table.setEditTriggers(QAbstractItemView.NoEditTriggers)
        self.table.setAlternatingRowColors(True)
        self.table.setShowGrid(False)
        self.table.setWordWrap(False)
        self.table.verticalHeader().hide()
        self.table.horizontalHeader().setSectionResizeMode(QHeaderView.Interactive)
        self.table.horizontalHeader().setStretchLastSection(True)
        for column, width in enumerate((340, 160, 130, 250, 250)):
            self.table.setColumnWidth(column, width)
        layout.addWidget(self.table, 1)
        pager = QHBoxLayout()
        self.count = QLabel("Страница не загружена")
        self.previous = QPushButton("Назад")
        self.next = QPushButton("Далее")
        self.previous.clicked.connect(lambda: self.pageRequested.emit(max(0, self._offset - 100)))
        self.next.clicked.connect(lambda: self.pageRequested.emit(self._offset + len(self.model.rows)))
        pager.addWidget(self.count, 1)
        pager.addWidget(self.previous)
        pager.addWidget(self.next)
        layout.addLayout(pager)
        self.query.textChanged.connect(self.update_controls)
        self.selector.textChanged.connect(self.update_controls)
        self.scope.currentIndexChanged.connect(self.update_controls)
        self.start.clicked.connect(self.request_search)
        self.query.returnPressed.connect(self.request_search)
        self.set_payload({"state": "unavailable", "reason": "Выберите сохранённый скан; возможность поиска проверяется у ядра"})
        self.sync_compact()

    def sync_compact(self):
        margin, spacing = content_spacing(self.width())
        self.content.layout().setContentsMargins(margin, spacing, margin, spacing)
        self.content.layout().setSpacing(12 if self.width() >= 900 else 8)
        self.empty_project.layout().setContentsMargins(margin, spacing, margin, spacing)
        self.options.show()
        self.options.setText("Параметры")
        self.options.setToolButtonStyle(Qt.ToolButtonTextBesideIcon if self.width() >= 900 else Qt.ToolButtonIconOnly)
        self.context.hide()
        self.help_text.hide()
        for widget in (*self.preset_buttons, self.case_sensitive, self.snippets):
            widget.hide()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self.sync_compact()

    @staticmethod
    def combo(name, entries):
        widget = QComboBox()
        widget.setObjectName(name)
        widget.setAccessibleName({"bodySearchScope": "Область сохранённого содержимого", "bodySearchMode": "Строка содержится или отсутствует", "bodySearchRepresentation": "Исходный HTML или сохранённый DOM"}.get(name, name))
        widget.setMinimumContentsLength(10)
        widget.setSizeAdjustPolicy(QComboBox.AdjustToMinimumContentsLengthWithIcon)
        for label, value in entries:
            widget.addItem(label, value)
        return widget

    def set_context(self, label, scan_uuid, available, project_open=True):
        self.empty_project.setVisible(not project_open)
        self.content.setVisible(project_open)
        self._selected, self._available = bool(scan_uuid), bool(available)
        self.context.setText(f"{label} · скан {scan_uuid}" if scan_uuid else "Выберите сохранённый скан в панели проекта")
        self.update_controls()

    def set_preset(self, identifier):
        preset = next((item for item in SEARCH_PRESETS if item["id"] == identifier), None)
        if preset:
            self.query.setText(preset["query"])
            self.scope.setCurrentIndex(self.scope.findData(preset["scope"]))
            self.mode.setCurrentIndex(self.mode.findData("contains"))
        self.query.setFocus()
        self.query.selectAll()

    def update_controls(self, *_args):
        selector_mode = self.scope.currentData() == "selector_markup"
        self.selector.setVisible(selector_mode)
        self.selector_label.setVisible(selector_mode)
        valid = bool(self.query.text().strip()) and "\x00" not in self.query.text() and (not selector_mode or bool(self.selector.text().strip()))
        self.start.setEnabled(self._available and self._selected and valid and not self._busy)
        self.start.setToolTip("Подключённое ядро не поддерживает поиск по телам" if not self._available else "Выберите сохранённый скан" if not self._selected else "Явный поиск по всему сохранённому скану")
        self.cancel.setEnabled(self._busy)
        self.cancel.setVisible(self._busy)

    def request_search(self):
        if self.start.isEnabled():
            self.searchRequested.emit({"query": self.query.text(), "scope": self.scope.currentData(), "mode": self.mode.currentData(), "representation": self.representation.currentData(), "selector": self.selector.text().strip() if self.scope.currentData() == "selector_markup" else None, "case_sensitive": self.case_sensitive.isChecked(), "include_snippets": self.snippets.isChecked()})

    def set_payload(self, payload):
        state = payload.get("state", "unavailable")
        self._busy = state == "loading"
        self._offset = payload.get("offset") or 0
        self.state.set_state("running" if self._busy else "partial" if payload.get("operation_status") in {"partial", "incomplete"} else "finished" if state == "ready" else state)
        reasons = {"body_absent/omitted_by_policy": "Тело не сохранено по политике источника", "body_absent": "Тело страницы не сохранено", "rendered_body_absent": "DOM после JavaScript не сохранён", "non_html": "Документ не является HTML"}
        representations = {"static": "Исходный HTML", "rendered": "Сохранённый DOM"}
        rows = [{**row, "_source": {"reason": row.get("reason"), "capture_mode": row.get("capture_mode")},
                 "reason": reasons.get(str(row.get("reason")), row.get("reason") or ""),
                 "capture_mode": representations.get(row.get("capture_mode"), row.get("capture_mode")),
                 "presence_label": "Найдена" if row.get("presence") is True else "Не найдена" if row.get("presence") is False else "Не проверено"} for row in payload.get("rows", [])]
        self.model.replace(rows)
        self.table.setVisible(bool(rows))
        self.content.layout().setStretch(self.empty_stretch_index, 0 if rows else 1)
        self.table.setColumnHidden(4, not any("snippet" in row for row in rows))
        coverage = payload.get("coverage") or {}
        self.coverage_metrics.setVisible(bool(coverage))
        for key, title in self.coverage_fields:
            value = coverage.get(key)
            text = str(value) if type(value) is int and value >= 0 else "—"
            self.coverage_values[key].setText(text)
            self.coverage_values[key].setAccessibleName(title + ": " + text)
            self.coverage_values[key].setToolTip("Весь сохранённый корпус; не только текущая страница")
        self.coverage.setText("  ·  ".join(f"{title}: {value_text(coverage.get(key))}" for key, title in (("present_documents", "Найдена"), ("absent_documents", "Не найдена"), ("unavailable_documents", "Недоступно"), ("non_html_documents", "Не HTML"))))
        self.coverage.setToolTip("Счётчики всего корпуса выбранного скана, не текущей страницы")
        source = payload.get("source") or {}
        query = payload.get("query")
        scopes = {"head_markup": "код <head>", "raw_html": "весь HTML", "body_text": "текст <body>", "selector_markup": "код CSS-элемента"}
        self.result_context.setText(f"Результат: {query} · {scopes.get(payload.get('scope'), 'область не указана')} · {representations.get(payload.get('representation'), 'источник не указан')} · скан {short_run_id(source.get('scan_uuid')) or '—'}" if query else "Результат появится после явного поиска")
        self.result_context.setToolTip(value_text(source))
        total = payload.get("total")
        self.count.setText(f"Строки {self._offset + 1}–{self._offset + len(rows)} из {total}" if rows else f"Нет строк на этой странице · всего {value_text(total)}")
        self.previous.setEnabled(state == "ready" and self._offset > 0)
        self.next.setEnabled(state == "ready" and bool(payload.get("has_more")) and bool(rows))
        reason = payload.get("reason") or ""
        if state == "ready" and coverage.get("unavailable_documents"):
            reason = "Часть документов недоступна. Отсутствие строки во всём скане не подтверждено." + ("\n" + reason if reason else "")
        self.message.setText(reason)
        self.message.setVisible(bool(reason))
        self.update_controls()
