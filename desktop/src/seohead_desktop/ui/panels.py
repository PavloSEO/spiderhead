"""Composable three-pane audit workspace and SEOHEAD project panels."""

from itertools import islice
from string import Template

from PyQt5.QtCore import Qt, pyqtSignal
from PyQt5.QtWidgets import (
    QComboBox,
    QFormLayout,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPlainTextEdit,
    QPushButton,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .comparison_summary import ComparisonSummary
from .components import PAGE_LIMIT, TabDeck, TablePanel, display_value, material_icon
from .kit import waiting_badge
from .presentation import WorkspaceSplitter, content_spacing
from .tabcatalogue import DETAIL_TABS, MAIN_TABS, PROJECT_TABS, RIGHT_TABS, TAB_BY_ID


def component_stylesheet(tokens):
    """Consume the existing canonical theme; no duplicate color constants."""
    return Template("""
QLabel#panelMessage { background: transparent; color: $on_surface_variant; padding: 24px; border: none; }
QWidget#evidencePreview { background: $surface_container_lowest; border: 1px solid $outline_variant; border-radius: ${radius_panel}px; }
QLabel#snippetTitle { color: $primary; font-size: 18px; }
QLabel#snippetURL { color: $on_surface_variant; }
QLabel#componentHeading { font-size: 20px; font-weight: 500; }
QListWidget#tabNavigation { background: $surface_container_low; border: none; padding: 8px; }
QListWidget#tabNavigation::item { min-height: 30px; padding: 6px 10px; border-radius: ${radius_control}px; }
QListWidget#tabNavigation::item:selected { background: $primary_container; color: $on_primary_container; }
QListWidget#tabNavigation::item:hover { background: $surface_container; }
QPlainTextEdit#noteDraft { border: 1px solid $outline_variant; border-radius: ${radius_control}px; }
QPlainTextEdit#noteDraft:focus { border: 2px solid $primary; }
QComboBox QAbstractItemView { background: $surface_container_lowest; selection-background-color: $primary_container; selection-color: $on_primary_container; }
QMenu { background: $surface_container_lowest; border: none; border-radius: ${radius_popup}px; padding: 8px; }
QMenu::item { padding: 8px 16px; border-radius: ${radius_control}px; }
QMenu::item:selected { background: $primary_container; color: $on_primary_container; }
QTabBar QToolButton { border: none; border-radius: 0; padding: 0; width: 22px; }
QFrame[kv_row="true"] { border-bottom: 1px solid $outline_variant; }
""").substitute(
        {
            **tokens["colors"],
            **{key: tokens[key] for key in ("radius_control", "radius_panel", "radius_popup")},
        }
    )


class SourcePanel(TablePanel):
    """Retained HTML is plain text; never creates a browser or executes scripts."""

    TEXT_LIMIT = 65536

    def __init__(self, spec, parent=None):
        super().__init__(spec, parent)
        self.viewer = QPlainTextEdit()
        self.viewer.setReadOnly(True)
        self.viewer.setAccessibleName("Сохранённый исходный HTML")
        self.viewer.setLineWrapMode(QPlainTextEdit.NoWrap)
        self.stack.addWidget(self.viewer)
        self.search.setPlaceholderText("Найти в сохранённом тексте · Enter")
        self.search.returnPressed.connect(self.find_text)

    def set_page(self, items, **kwargs):
        page = list(islice(iter(items), PAGE_LIMIT + 1))
        super().set_page(page, **kwargs)
        if not hasattr(self, "viewer"):
            return
        self.viewer.clear()
        if self.state == "ready" and page and page[0].get("source") is not None:
            source = str(page[0]["source"])
            self.viewer.setPlainText(source[: self.TEXT_LIMIT])
            self.stack.setCurrentWidget(self.viewer)
            if len(source) > self.TEXT_LIMIT:
                self.count_label.setText(
                    f"Показаны первые {self.TEXT_LIMIT:,} символов сохранённого текста"
                )

    def find_text(self):
        if not self.viewer.find(self.search.text()):
            cursor = self.viewer.textCursor()
            cursor.movePosition(cursor.Start)
            self.viewer.setTextCursor(cursor)
            self.viewer.find(self.search.text())


class SerpPreviewPanel(TablePanel):
    def __init__(self, spec, parent=None):
        super().__init__(spec, parent)
        self.original = {}
        self.preview = QWidget()
        self.preview.setObjectName("evidencePreview")
        layout = QVBoxLayout(self.preview)
        fields = QFormLayout()
        self.title_edit = QLineEdit()
        self.title_edit.setAccessibleName("Title для локального предпросмотра")
        self.description_edit = QLineEdit()
        self.description_edit.setAccessibleName("Description для локального предпросмотра")
        fields.addRow("Title", self.title_edit)
        fields.addRow("Description", self.description_edit)
        layout.addLayout(fields)
        self.preview_url = QLabel()
        self.preview_url.setObjectName("snippetURL")
        self.preview_title = QLabel()
        self.preview_title.setObjectName("snippetTitle")
        self.preview_description = QLabel()
        for label in (self.preview_url, self.preview_title, self.preview_description):
            label.setTextFormat(Qt.PlainText)
            label.setWordWrap(True)
            layout.addWidget(label)
        reset = QPushButton("Вернуть сохранённые значения")
        reset.clicked.connect(self.reset_values)
        layout.addWidget(reset, 0, Qt.AlignLeft)
        note = QLabel("Локальный предпросмотр. Поисковая выдача и её оформление не измерены.")
        note.setWordWrap(True)
        note.setObjectName("muted")
        layout.addWidget(note)
        layout.addStretch()
        self.stack.addWidget(self.preview)
        self.title_edit.textChanged.connect(self.preview_title.setText)
        self.description_edit.textChanged.connect(self.preview_description.setText)

    def set_page(self, items, **kwargs):
        page = list(islice(iter(items), PAGE_LIMIT + 1))
        super().set_page(page, **kwargs)
        if not hasattr(self, "preview"):
            return
        self.original = dict(page[0]) if self.state == "ready" and page else {}
        self.reset_values()
        if self.original:
            self.stack.setCurrentWidget(self.preview)

    def reset_values(self):
        self.title_edit.setText(display_value(self.original.get("title")))
        self.description_edit.setText(display_value(self.original.get("description")))
        self.preview_url.setText(display_value(self.original.get("url")))


class InboxPanel(TablePanel):
    def __init__(self, spec, parent=None):
        super().__init__(spec, parent)
        self.submission_enabled = False
        self.note = QPlainTextEdit()
        self.note.setObjectName("noteDraft")
        self.note.setAccessibleName("Новая заметка проекта")
        self.note.setPlaceholderText("Заметка или вопрос по проекту")
        self.note.setMaximumHeight(100)
        self.note.setTabChangesFocus(True)
        self.layout().addWidget(self.note)
        controls = QHBoxLayout()
        self.kind = QComboBox()
        self.kind.setAccessibleName("Тип заметки")
        self.kind.addItem("Заметка", "note")
        self.kind.addItem("Вопрос · недоступно", "question")
        self.kind.model().item(1).setEnabled(False)
        self.kind.model().item(1).setToolTip(
            "Ядро поддерживает note и proposed_goal; вопрос можно записать как текст заметки."
        )
        self.kind.addItem("Предложенная цель", "proposed_goal")
        self.kind.currentIndexChanged.connect(self._validate_note)
        controls.addWidget(self.kind)
        self.note_status = QLabel("Отправка не подключена")
        self.note_status.setObjectName("muted")
        self.note_status.setWordWrap(True)
        controls.addWidget(self.note_status, 1)
        self.submit_button = QPushButton("Сохранить заметку")
        self.submit_button.setProperty("role", "primary")
        self.submit_button.setEnabled(False)
        self.submit_button.clicked.connect(self.submit)
        controls.addWidget(self.submit_button)
        self.layout().addLayout(controls)
        self.note.textChanged.connect(self._validate_note)

    def set_submission_enabled(self, enabled, reason=""):
        self.submission_enabled = bool(enabled)
        self.note_status.setText(
            reason or ("Отправка явным действием" if enabled else "Отправка не подключена")
        )
        self._validate_note()

    def _validate_note(self):
        length = len(self.note.toPlainText().strip())
        self.submit_button.setEnabled(
            self.submission_enabled
            and 0 < length <= 4000
            and self.kind.currentData() in {"note", "proposed_goal"}
        )
        if length > 4000:
            self.note_status.setText("Максимум 4 000 символов; текст сохранён в форме")

    def submit(self):
        text = self.note.toPlainText().strip()
        if (
            self.submission_enabled
            and 0 < len(text) <= 4000
            and self.kind.currentData() in {"note", "proposed_goal"}
        ):
            self.intent_requested.emit(
                "submit_note", {"text": text, "kind": self.kind.currentData()}
            )

    def submission_succeeded(self):
        self.note.clear()
        self.note_status.setText("Заметка сохранена ядром")

    def set_page(self, items, **kwargs):
        super().set_page(items, **kwargs)
        if hasattr(self, "note") and self.state == "unavailable":
            self.set_submission_enabled(False)


class ComparePanel(TablePanel):
    """Retained-pair selectors and separate raw-change/verified-page summaries."""

    pair_changed = pyqtSignal()

    def __init__(self, spec, parent=None):
        super().__init__(spec, parent)
        self.idle_when_cleared = True
        self.before = QComboBox()
        self.before.setAccessibleName("До: исходный сохранённый скан")
        self.after = QComboBox()
        self.after.setAccessibleName("После: новый сохранённый скан")
        for combo in (self.before, self.after):
            combo.setMinimumContentsLength(22)
            combo.setSizeAdjustPolicy(QComboBox.AdjustToMinimumContentsLengthWithIcon)
        header = QWidget()
        header.setObjectName("comparisonHeader")
        header_layout = QVBoxLayout(header)
        header_layout.setContentsMargins(0, 0, 0, 16)
        header_layout.setSpacing(16)
        title = QLabel("Сравнение сканов")
        title.setObjectName("sectionTitle")
        heading_row = QHBoxLayout()
        heading_row.addWidget(title, 1)
        header_layout.addLayout(heading_row)
        caption = QLabel(
            "Выберите два сохранённых наблюдения. Исправление подтверждается отдельной проверкой ядра."
        )
        caption.setObjectName("metadata")
        caption.setWordWrap(True)
        caption.hide()
        title.setToolTip(caption.text())
        self.pair_toggle = QToolButton()
        self.pair_toggle.setCheckable(True)
        self.pair_toggle.setText("Выбрать пару")
        self.pair_toggle.setToolTip("Раскрыть выбор пары и повторное сравнение")
        self.pair_toggle.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
        self.pair_toggle.setIcon(material_icon("tune"))
        self.pair_toggle.setAccessibleName("Раскрыть выбор пары и повторное сравнение")
        self.pair_toggle.hide()
        heading_row.addWidget(self.pair_toggle)
        self.pair_controls = QWidget()
        pair_layout = QVBoxLayout(self.pair_controls)
        pair_layout.setContentsMargins(0, 0, 0, 0)
        pair_layout.setSpacing(4)
        header_layout.addWidget(self.pair_controls)
        self.pair_toggle.toggled.connect(self.sync_pair_controls)
        chooser = QGridLayout()
        chooser.setColumnStretch(0, 1)
        chooser.setColumnStretch(2, 1)
        for column, text, combo in ((0, "Было", self.before), (2, "Стало", self.after)):
            label = QLabel(text)
            label.setObjectName("comparisonStep")
            chooser.addWidget(label, 0, column)
            chooser.addWidget(combo, 1, column)
        self.swap = QToolButton()
        self.swap.setIcon(material_icon("swap_horiz"))
        self.swap.setToolTip("Поменять местами: было и стало")
        self.swap.setAccessibleName("Поменять местами: было и стало")
        self.swap.clicked.connect(self._swap_pair)
        chooser.addWidget(self.swap, 1, 1)
        pair_layout.addLayout(chooser)
        controls = QHBoxLayout()
        self.comparison_status = QLabel("Выберите два разных скана")
        self.comparison_status.setObjectName("metadata")
        self.comparison_status.setWordWrap(True)
        controls.addWidget(self.comparison_status, 1)
        self.compare_button = QPushButton("Сравнить сохранённые сканы")
        self.compare_button.setProperty("role", "primary")
        self.compare_button.setEnabled(False)
        controls.addWidget(self.compare_button)
        pair_layout.addLayout(controls)
        self.comparison_summary = ComparisonSummary()
        self.warning_toggle = self.comparison_summary.details_toggle
        self.warning_text = self.comparison_summary.details
        self.comparison_summary.filterRequested.connect(self.apply_status_filter)
        header_layout.addWidget(self.comparison_summary)
        gaps = QHBoxLayout()
        gap_label = QLabel("По сегментам сайта и KPI до/после")
        gap_label.setObjectName("metadata")
        gaps.addWidget(gap_label, 1)
        gaps.addWidget(waiting_badge(938))
        header_layout.addLayout(gaps)
        self.filter.currentIndexChanged.disconnect()
        self.filter.clear()
        for label, state in (
            ("Все на странице", "all"),
            ("Исправлено", "resolved"),
            ("Сохранилось", "persisting"),
            ("Изменилось", "changed"),
            ("Нельзя подтвердить", "not_verifiable"),
            ("Новые", "new"),
        ):
            self.filter.addItem(label, state)
        self.filter.currentIndexChanged.connect(
            lambda: self.apply_status_filter(self.filter.currentData())
        )
        self.layout().insertWidget(0, header)
        self.before.currentIndexChanged.connect(self._valid_pair)
        self.after.currentIndexChanged.connect(self._valid_pair)
        self.before.currentIndexChanged.connect(lambda: self.pair_changed.emit())
        self.after.currentIndexChanged.connect(lambda: self.pair_changed.emit())
        self.compare_button.clicked.connect(self.preview_comparison)
        self.refresh_button.clicked.disconnect()
        self.refresh_button.clicked.connect(lambda: self.request_page(self.offset))
        self.refresh_button.setEnabled(False)
        self.source_label.hide()
        self.table.setColumnWidth(0, 280)
        self.table.setColumnWidth(1, 180)
        self.table.setColumnWidth(2, 190)
        for column in range(3, self.model.columnCount()):
            self.table.setColumnWidth(column, 240)

    def _swap_pair(self):
        """Exchange before/after; both choosers list the same retained scans, so indexes map 1:1."""
        before, after = self.before.currentIndex(), self.after.currentIndex()
        self.before.setCurrentIndex(after)
        self.after.setCurrentIndex(before)

    def sync_pair_controls(self, *_args):
        if not hasattr(self, "pair_controls"):
            return
        compact = self.state == "ready"
        self.pair_toggle.setVisible(compact)
        if hasattr(self, "comparison_summary"):
            self.comparison_summary.set_compact(compact)
        self.pair_controls.setVisible(not compact or self.pair_toggle.isChecked())

    def resizeEvent(self, event):
        super().resizeEvent(event)
        margin, spacing = content_spacing(self.width())
        self.layout().setContentsMargins(margin, spacing, margin, spacing)
        self.sync_pair_controls()

    def set_scans(self, items):
        from .presentation import field_text, short_run_id, state_text

        scans = list(islice(iter(items), PAGE_LIMIT + 1))
        if len(scans) > PAGE_LIMIT:
            raise ValueError("Scan chooser is bounded to one page")
        fresh = self.before.currentData() is None and self.after.currentData() is None
        latest = sorted(
            scans,
            key=lambda scan: scan.get("finished_at") or scan.get("created_at") or "",
            reverse=True,
        )[:2]
        for combo in (self.before, self.after):
            previous = combo.currentData()
            selected_uuid = previous.get("uuid") if isinstance(previous, dict) else None
            if (
                selected_uuid is None and fresh and len(latest) == 2
            ):  # first open: the two newest saved scans, before = older
                selected_uuid = latest[1 if combo is self.before else 0].get("uuid")
            combo.blockSignals(True)
            combo.clear()
            combo.addItem("Выберите сохранённый скан", None)
            for scan in scans:
                label = f"{field_text('finished_at', scan.get('finished_at') or scan.get('created_at'))} · {short_run_id(scan.get('uuid')) or 'ID неизвестен'} · {state_text('partial' if scan.get('crawl_partial') or scan.get('corpus_partial') else scan.get('lifecycle'))}"
                combo.addItem(label, dict(scan))
                combo.setItemData(
                    combo.count() - 1, scan.get("path") or scan.get("uuid"), Qt.ToolTipRole
                )
                if scan.get("uuid") == selected_uuid:
                    combo.setCurrentIndex(combo.count() - 1)
            combo.blockSignals(False)
        self._valid_pair()
        if fresh and len(latest) == 2 and self.compare_button.isEnabled():
            self.comparison_status.setText(
                "Выбраны два последних скана: проверьте пару и нажмите «Сравнить сохранённые сканы»"
            )

    def _valid_pair(self):
        before, after = self.before.currentData(), self.after.currentData()
        valid = bool(
            before
            and after
            and before.get("uuid")
            and after.get("uuid")
            and before["uuid"] != after["uuid"]
        )
        self.compare_button.setEnabled(valid and self.state != "loading")
        if not valid:
            self.comparison_status.setText("Нужны два разных сохранённых скана")

    def set_summary(self, payload):
        state = payload.get("state")
        if state in {"unavailable", "idle"}:
            self._valid_pair()
        self.comparison_status.setText(
            payload.get("reason")
            or (
                "Сравнение сохранено · результаты ниже"
                if state == "ready"
                else "Выберите два сохранённых скана"
            )
        )
        self.comparison_summary.set_payload(payload)
        self.apply_status_filter("all")
        self.comparison_summary.setToolTip(payload.get("package") or "")
        if payload.get("source"):
            before, after = self.before.currentText(), self.after.currentText()
            self.source_label.setText(f"До: {before} → после: {after}")
            self.source_label.setToolTip(payload["source"])

    def apply_status_filter(self, state):
        allowed = {"all", "resolved", "persisting", "changed", "not_verifiable", "new"}
        if state not in allowed:
            return
        self.filter.blockSignals(True)
        self.filter.setCurrentIndex(self.filter.findData(state))
        self.filter.blockSignals(False)
        self.proxy.set_state_filter(state)
        self.comparison_summary.set_active_filter(state)
        self._search_page(self.search.text())

    def preview_comparison(self):
        if self.compare_button.isEnabled():
            self.intent_requested.emit(
                "preview_compare",
                {"before": self.before.currentData(), "after": self.after.currentData()},
            )

    def set_page(self, items, **kwargs):
        kwargs["available_filters"] = (
            "all",
            "resolved",
            "persisting",
            "changed",
            "not_verifiable",
            "new",
        )
        super().set_page(items, **kwargs)
        self.sync_pair_controls()
        if hasattr(self, "compare_button"):
            self.refresh_button.setEnabled(self.state == "ready")
            self._valid_pair()
            if self.state in {"unavailable", "loading"}:
                self.compare_button.setEnabled(False)


def detail_factory(spec, parent):
    if spec.id == "view_source":
        return SourcePanel(spec, parent)
    if spec.id == "serp_snippet":
        return SerpPreviewPanel(spec, parent)
    return TablePanel(spec, parent)


class AuditWorkspace(QWidget):
    """SF-like organization with SEOHEAD native styling and explicit adapters."""

    intent_requested = pyqtSignal(str, object)

    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        controls = QHBoxLayout()
        controls.setContentsMargins(12, 6, 12, 6)
        heading = QLabel("Аудит сохранённых данных")
        heading.setObjectName("sectionCaption")
        controls.addWidget(heading, 1)
        self.detail_toggle = QToolButton()
        self.detail_toggle.setText("Детали")
        self.detail_toggle.setProperty("role", "panelToggle")
        self.detail_toggle.setToolTip("Скрыть или показать детали URL")
        self.detail_toggle.setAccessibleName("Скрыть или показать детали URL")
        self.detail_toggle.clicked.connect(lambda: self.detail.setVisible(self.detail.isHidden()))
        controls.addWidget(self.detail_toggle)
        self.right_toggle = QToolButton()
        self.right_toggle.setText("Сводка")
        self.right_toggle.setProperty("role", "panelToggle")
        self.right_toggle.setToolTip("Скрыть или показать сводку аудита")
        self.right_toggle.setAccessibleName("Скрыть или показать сводку аудита")
        self.right_toggle.clicked.connect(lambda: self.right.setVisible(self.right.isHidden()))
        controls.addWidget(self.right_toggle)
        layout.addLayout(controls)
        self.horizontal = WorkspaceSplitter(Qt.Horizontal)
        self.vertical = WorkspaceSplitter(Qt.Vertical)
        self.main = TabDeck(MAIN_TABS)
        self.detail = TabDeck(DETAIL_TABS, detail_factory)
        self.right = TabDeck(RIGHT_TABS)
        self.vertical.addWidget(self.main)
        self.vertical.addWidget(self.detail)
        self.horizontal.addWidget(self.vertical)
        self.horizontal.addWidget(self.right)
        self.horizontal.setChildrenCollapsible(True)
        self.vertical.setChildrenCollapsible(True)
        self.vertical.setStretchFactor(0, 1)
        self.vertical.setStretchFactor(1, 1)
        self.horizontal.setStretchFactor(0, 3)
        self.horizontal.setStretchFactor(1, 1)
        layout.addWidget(self.horizontal)
        for deck in (self.main, self.detail, self.right):
            deck.intent_requested.connect(self._relay)
            deck.current_changed.connect(
                lambda id: self.intent_requested.emit("open_panel", {"tab_id": id})
            )
        self.main.current_changed.connect(self._main_changed)
        self._shown = False
        self.restore_panels()

    def panel(self, id):
        pane = TAB_BY_ID[id].pane
        return {"main": self.main, "detail": self.detail, "right": self.right}[pane].panel(id)

    def set_page(self, tab_id, items, **kwargs):
        if TAB_BY_ID[tab_id].pane == "main" and tab_id == self.main.current_id:
            self.detail.clear("Выберите URL в обновлённой странице.")
        self.panel(tab_id).set_page(items, **kwargs)

    def clear(self, reason="Выберите сохранённый источник."):
        for deck in (self.main, self.detail, self.right):
            deck.clear(reason)

    def restore_panels(self):
        self.main.show()
        self.detail.show()
        self.right.show()
        self.horizontal.setSizes([940, 330])
        self.vertical.setSizes([520, 300])

    def showEvent(self, event):
        if not self._shown:
            self.vertical.setSizes([520, 300])
            self.horizontal.setSizes([940, 330])
            self._shown = True
        super().showEvent(event)

    def _relay(self, intent, payload):
        if intent == "select_url":
            self.detail.clear("Выбран другой URL. Загрузите его сохранённые измерения.")
            source = self.panel(payload["tab_id"]).source_label.text()
            row = payload["row"]
            fields = [
                {"name": label, "value": row.get(key)}
                for key, label in TAB_BY_ID[payload["tab_id"]].columns
            ]
            self.set_page("url_details", fields, total=len(fields), source=source)
        self.intent_requested.emit(intent, payload)

    def _main_changed(self, id):
        panel = self.main.panel(id)
        current = panel.table.currentIndex()
        if panel.state == "ready" and current.isValid():
            panel._selected(current, None)
        else:
            self.detail.clear("Выберите URL в текущем разделе.")


class ProjectPanels(TabDeck):
    """Project views use plain projections and semantic signals only."""

    refresh = pyqtSignal()
    select_task = pyqtSignal(str)
    select_scan = pyqtSignal(dict)
    submit_note = pyqtSignal(str, str)

    def __init__(self, parent=None):
        super().__init__(PROJECT_TABS, self._factory, parent)
        self.intent_requested.connect(self._semantic_intent)
        self.current_changed.connect(
            lambda id: self.intent_requested.emit("open_panel", {"tab_id": id})
        )

    @staticmethod
    def _factory(spec, parent):
        return {"inbox": InboxPanel, "compare": ComparePanel}.get(spec.id, TablePanel)(spec, parent)

    def set_page(self, tab_id, items, **kwargs):
        self.panel(tab_id).set_page(items, **kwargs)

    def _semantic_intent(self, intent, payload):
        if intent == "refresh":
            self.refresh.emit()
        elif intent == "select_task":
            id = payload["row"].get("id")
            if id is not None:
                self.select_task.emit(str(id))
        elif intent == "select_scan":
            self.select_scan.emit(dict(payload["row"]))
        elif intent == "submit_note":
            self.submit_note.emit(payload["text"], payload["kind"])
