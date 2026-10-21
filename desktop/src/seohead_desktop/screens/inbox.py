"""Screen «Входящие · заметки агенту» (sheet Inbox).

The list and stages come from the project inbox the core keeps (host.inbox_model.rows). Writing happens only through
host.submit_note on the explicit «Сохранить» button; the draft survives a refused write, a project switch and a tab
switch (host stash/restore_note_drafts). «Вопрос» and the agent's reply are not in the core yet (#944) and say so.
"""

from __future__ import annotations

from PyQt5.QtCore import QRectF, QSize, Qt, pyqtSignal
from PyQt5.QtGui import QColor, QFont, QPainter
from PyQt5.QtWidgets import (
    QButtonGroup,
    QDialog,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QMenu,
    QPlainTextEdit,
    QPushButton,
    QScrollArea,
    QShortcut,
    QSplitter,
    QStyle,
    QStyledItemDelegate,
    QTabBar,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import i18n, theming
from ..i18n import tr, trf
from ..local_owner_actions import LocalOwnerActions, accept_goal_arguments
from ..ui.controls import Note
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import BADGE_ROLE, StatePanel, no_project_panel, show_empty, waiting_badge
from ..ui.modal import ModalDialog
from .base import Screen
from .work import RowsModel, build_table, clear_layout, local_stamp, number, project_state

MAX_TEXT = 4000
KINDS = {"note": ("info", "Заметка"), "proposed_goal": ("goal", "Цель")}
OUTCOMES = {
    "task": "Задача создана",
    "goal": "Цель принята",
    "competitor": "Конкуренты найдены",
    "blocked": "Заблокировано",
    "rejected": "Отклонено",
}
REFERENCE_ICONS = {
    "scan": ("history", "Скан"),
    "task": ("checklist", "Задача"),
    "goal": ("flag", "Цель"),
    "finding": ("rule", "Находка"),
    "section": ("description", "Раздел"),
}
REFERENCE_SECTIONS = {
    "scan": "scans",
    "task": "work",
    "goal": "work",
    "finding": "issues",
    "section": "url",
}
FILTERS = (
    ("all", "Все", "Все"),
    ("unread", "Не прочитаны", "Новые"),
    ("goal", "Цели", "Цели"),
    ("done", "Обработаны", "Готово"),
)


def is_processed(entry):
    return (
        bool(entry.get("triage"))
        or bool(entry.get("acknowledged_at"))
        or entry.get("goal_state") in ("accepted", "completed")
    )


def outcome(entry):
    """Label of the last confirmed result of an entry, or None when the core recorded none."""
    triage = entry.get("triage") or []
    if triage:
        return OUTCOMES.get(triage[-1].get("kind"), triage[-1].get("kind"))
    state = entry.get("goal_state")
    return {"accepted": "Цель принята", "completed": "Цель выполнена"}.get(state)


def stages(entry):
    """The four stages of an entry as (done, icon, label); the second one is not measurable yet (#944)."""
    last = outcome(entry)
    return (
        (True, "check", "Сохранено"),
        (bool(entry.get("read_at")), "visibility", "Прочитано агентом"),
        (bool(entry.get("triage")), "sort", "Разобрано"),
        (last is not None, "task_alt", last or "Не подтверждено"),
    )


def first_line(text):
    return " ".join((text or "").split()) or tr("Нет данных")


def entry_columns():
    return (
        (
            "Тип",
            lambda e: tr(KINDS.get(e.get("kind"), ("mut", e.get("kind") or "—"))[1]),
            lambda e: (
                KINDS.get(e.get("kind"), ("mut", ""))[0],
                tr(KINDS.get(e.get("kind"), ("mut", e.get("kind") or "—"))[1]),
            ),
            None,
        ),
        ("Запись", lambda e: first_line(e.get("text")), None, lambda e: e.get("text") or ""),
        ("Стадия", lambda e: tr(outcome(e) or "Сохранено"), None, None),
    )


class EntryDelegate(QStyledItemDelegate):
    """One canvas message row: kind badge and title, the time on the right, the four stages below.

    Painted by the delegate, never a widget per row. Column 0 carries the kind (BADGE_ROLE); the title is column 1's text.
    """

    ROW_HEIGHT = 64

    def sizeHint(self, option, index):
        return QSize(0, self.ROW_HEIGHT)

    def paint(self, painter, option, index):
        palette = theming.roles()
        entry = index.model().rows[index.row()]
        painter.save()
        painter.setRenderHint(QPainter.Antialiasing)
        fill = (
            palette["selected"]
            if option.state & QStyle.State_Selected
            else palette["hover_row"]
            if option.state & QStyle.State_MouseOver
            else palette["base"]
        )
        painter.fillRect(option.rect, QColor(fill))
        painter.setPen(QColor(palette["divider_inner"]))
        painter.drawLine(option.rect.bottomLeft(), option.rect.bottomRight())
        rect = option.rect.adjusted(16, 10, -16, -8)

        kind, label = index.data(BADGE_ROLE) or ("mut", "")
        background, ink = theming.theme()["badges"].get(kind) or theming.theme()["badges"]["mut"]
        badge_font = QFont(option.font)
        badge_font.setPixelSize(12)
        badge_font.setWeight(QFont.Medium)
        painter.setFont(badge_font)
        badge = QRectF(
            rect.left(), rect.top(), painter.fontMetrics().horizontalAdvance(label) + 16, 20
        )
        painter.setPen(Qt.NoPen)
        painter.setBrush(QColor(background))
        painter.drawRoundedRect(badge, 6, 6)
        painter.setPen(QColor(ink))
        painter.drawText(badge, Qt.AlignCenter, label)

        stamp = local_stamp(entry.get("created_at"), True) or ""
        stamp_font = QFont(option.font)
        stamp_font.setPixelSize(12)
        painter.setFont(stamp_font)
        stamp_width = painter.fontMetrics().horizontalAdvance(stamp)
        painter.setPen(QColor(palette["text_3"]))
        painter.drawText(
            QRectF(rect.right() - stamp_width, rect.top(), stamp_width, 20),
            Qt.AlignVCenter | Qt.AlignRight,
            stamp,
        )

        title_font = QFont(option.font)
        title_font.setPixelSize(13)
        title_font.setWeight(QFont.Medium)
        painter.setFont(title_font)
        title_left = badge.right() + 8
        title = painter.fontMetrics().elidedText(
            index.model().index(index.row(), 1).data() or "",
            Qt.ElideRight,
            int(rect.right() - stamp_width - 8 - title_left),
        )
        painter.setPen(QColor(palette["text"]))
        painter.drawText(
            QRectF(title_left, rect.top(), rect.right() - title_left - stamp_width - 8, 20),
            Qt.AlignVCenter | Qt.AlignLeft,
            title,
        )

        stage_font = QFont(option.font)
        stage_font.setPixelSize(11)
        painter.setFont(stage_font)
        x = rect.left()
        y = rect.top() + 28
        for done, _icon, text in stages(entry):
            painter.setPen(QColor(palette["success"] if done else palette["text_muted"]))
            width = painter.fontMetrics().horizontalAdvance(tr(text))
            painter.drawText(QRectF(x, y, width, 16), Qt.AlignVCenter | Qt.AlignLeft, tr(text))
            x += width + 12
        painter.restore()


class ListPane(QWidget):
    """Left pane that reports its width changes (the filter labels shorten when it gets narrow)."""

    resized = pyqtSignal()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self.resized.emit()


class StageChain(QWidget):
    def __init__(self, entry, parent=None):
        super().__init__(parent)
        layout = QGridLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setHorizontalSpacing(16)
        layout.setVerticalSpacing(6)
        for index, (done, icon, label) in enumerate(stages(entry)):
            measurable = index != 1
            cell = QWidget()
            row = QHBoxLayout(cell)
            row.setContentsMargins(0, 0, 0, 0)
            row.setSpacing(4)
            row.addWidget(
                MaterialIconLabel(
                    icon if measurable or done else "help",
                    16,
                    color="role:success" if done else "role:text_muted",
                )
            )
            text = QLabel(tr(label))
            text.setProperty("text_style", "meta")
            text.setWordWrap(True)
            row.addWidget(text, 1)
            cell.setToolTip(
                tr("Стадия подтверждена ядром")
                if done
                else tr("Ядро не различает получателей записи: чтение агентом не измеряется")
                + " · "
                + tr("Недоступно в этой версии ядра")
                if not measurable
                else tr("Ядро не записало этой стадии")
            )
            layout.addWidget(cell, index // 2, index % 2)
        layout.setColumnStretch(0, 1)
        layout.setColumnStretch(1, 1)


class ContextChip(QFrame):
    def __init__(self, text, icon, removable=False, parent=None):
        super().__init__(parent)
        self.setProperty("chip", "filter")
        layout = QHBoxLayout(self)
        layout.setContentsMargins(8, 2, 4 if removable else 8, 2)
        layout.setSpacing(4)
        layout.addWidget(MaterialIconLabel(icon, 16, color="role:text_2"))
        self.label = QLabel(text)
        layout.addWidget(self.label)
        self.remove = None
        if removable:
            self.remove = QToolButton()
            self.remove.setIcon(material_icon("close"))
            self.remove.setAccessibleName(tr("Убрать контекст"))
            self.remove.setToolTip(tr("Убрать контекст"))
            layout.addWidget(self.remove)


def reference_label(reference):
    kind, _, ident = reference.partition(":")
    icon, name = REFERENCE_ICONS.get(kind, ("link", kind))
    return icon, f"{tr(name)} {ident[:36]}"


class Composer(QFrame):
    """Note composer: kind, context chips, text, explicit save. Holds the draft; the host stashes/restores it."""

    def __init__(self, screen, parent=None):
        super().__init__(parent)
        self.screen = screen
        self.setProperty("card", "panel")
        self.references = []
        layout = QVBoxLayout(self)
        layout.setContentsMargins(12, 12, 12, 12)
        layout.setSpacing(8)
        top = QHBoxLayout()
        top.setSpacing(4)
        self.kind_group = QButtonGroup(self)
        self.kind_buttons = {}
        for kind, icon, label in (
            ("note", "edit_note", "Заметка"),
            ("proposed_goal", "flag", "Цель"),
        ):
            button = QToolButton()
            button.setProperty("pill", "group")
            button.setCheckable(True)
            button.setText(tr(label))
            button.setIcon(material_icon(icon))
            button.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
            self.kind_group.addButton(button)
            self.kind_buttons[kind] = button
            button.setToolTip(
                tr("Предложить цель проекта") if kind == "proposed_goal" else tr("Заметка агенту")
            )
            button.clicked.connect(lambda _c=False: self._changed())
            top.addWidget(button)
        self.kind_buttons["note"].setChecked(True)
        self.question = QToolButton()
        self.question.setProperty("pill", "group")
        self.question.setText(tr("Вопрос"))
        self.question.setIcon(material_icon("help"))
        self.question.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
        self.question.setEnabled(False)
        self.question.setToolTip(
            tr("Ядро пока принимает только заметки и цели")
            + " · "
            + tr("Недоступно в этой версии ядра")
        )
        top.addWidget(self.question)
        top.addStretch(1)
        layout.addLayout(top)
        context_row = QHBoxLayout()
        context_row.setSpacing(4)
        self.context_button = QToolButton()
        self.context_button.setProperty("pill", "group")
        self.context_button.setText(tr("Контекст"))
        self.context_button.setIcon(material_icon("add_link"))
        self.context_button.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
        self.context_button.setPopupMode(QToolButton.InstantPopup)
        self.context_menu = QMenu(self.context_button)
        self.context_menu.aboutToShow.connect(self._fill_menu)
        self.context_button.setMenu(self.context_menu)
        context_row.addWidget(self.context_button)
        self.chips = QHBoxLayout()
        self.chips.setSpacing(4)
        context_row.addLayout(self.chips, 1)
        layout.addLayout(context_row)
        self.text = QPlainTextEdit()
        self.text.setPlaceholderText(tr("Заметка для текущего проекта"))
        self.text.setAccessibleName(tr("Текст заметки"))
        self.text.setFixedHeight(88)
        self.text.textChanged.connect(self._changed)
        layout.addWidget(self.text)
        self.error = QFrame()
        self.error.setProperty("note", "error")
        error_layout = QHBoxLayout(self.error)
        error_layout.setContentsMargins(12, 8, 12, 8)
        error_layout.addWidget(MaterialIconLabel("error", 20, color="role:error"), 0, Qt.AlignTop)
        self.error_text = QLabel()
        self.error_text.setWordWrap(True)
        self.error_text.setTextFormat(Qt.RichText)
        error_layout.addWidget(self.error_text, 1)
        self.error.hide()
        layout.addWidget(self.error)
        self.hint = QLabel(
            tr(
                "Черновик хранится, пока приложение открыто: ошибка записи и смена проекта его не стирают · Ctrl+Enter"
            )
        )
        self.hint.setProperty("text_style", "meta")
        self.hint.setWordWrap(True)
        self.counter = QLabel()
        self.counter.setProperty("text_style", "meta")
        self.send = QPushButton(tr("Сохранить во входящие"))
        self.send.setProperty("role", "primary")
        self.send.setIcon(material_icon("send", "role:on_primary"))
        self.send.setEnabled(False)
        self.send.clicked.connect(self.submit)
        layout.addWidget(self.hint)
        bottom = QHBoxLayout()
        bottom.addWidget(self.counter)
        bottom.addStretch(1)
        bottom.addWidget(self.send)
        layout.addLayout(bottom)
        self.destination = QLabel()
        self.destination.setProperty("text_style", "meta")
        self.destination.setWordWrap(True)
        layout.addWidget(self.destination)
        shortcut = QShortcut("Ctrl+Return", self.text)
        shortcut.setContext(Qt.WidgetWithChildrenShortcut)
        shortcut.activated.connect(self.submit)
        self.ready = False
        self.pending = False
        self._chips_changed()

    # ---- draft API used by the host ------------------------------------------------------------------------------
    def kind(self):
        return next(
            (kind for kind, button in self.kind_buttons.items() if button.isChecked()), "note"
        )

    def draft(self):
        return (self.text.toPlainText().strip(), self.kind(), tuple(self.references))

    def set_draft(self, text, kind, references=()):
        self.text.blockSignals(True)
        self.text.setPlainText(text)
        self.text.blockSignals(False)
        self.kind_buttons.get(kind, self.kind_buttons["note"]).setChecked(True)
        self.references = list(references)
        self._chips_changed()
        self._refresh_send()

    def clear_draft(self):
        self.set_draft("", self.kind(), ())

    def set_submission_state(self, ready, pending, reason):
        self.ready, self.pending = bool(ready), bool(pending)
        self.destination.setText(reason.rsplit(" · ", 1)[-1])
        self.destination.setToolTip(reason)
        self.send.setText(tr("Сохранение…") if pending else tr("Сохранить во входящие"))
        self._refresh_send()

    def show_error(self, text):
        self.error.setVisible(bool(text))
        if text:
            self.error_text.setText(
                f"<b>{tr('Не удалось сохранить заметку.')}</b> {text} {tr('Текст остаётся в форме.')}"
            )

    # ---- behaviour -----------------------------------------------------------------------------------------------
    def _refresh_send(self):
        length = len(self.text.toPlainText().strip())
        self.counter.setText(f"{length} / {MAX_TEXT}")
        self.send.setEnabled(self.ready and not self.pending and 0 < length <= MAX_TEXT)

    def _changed(self):
        self._refresh_send()
        self.screen.host.update_note_controls()

    def _fill_menu(self):
        self.context_menu.clear()
        host = self.screen.host
        scan = next(
            (r for r in host.scan_model.rows if r.get("path") == host.selected_scan_path), None
        )
        work = host.screens.get("work")
        task = getattr(work, "selected_id", None) or host.task_detail_requested
        options = []
        if scan and scan.get("uuid"):
            options.append((f"scan:{scan['uuid']}", tr("Выбранный скан")))
        if task:
            options.append((f"task:{task}", tr("Выбранная задача")))
        if not options:
            self.context_menu.addAction(tr("Нет скана или задачи для ссылки")).setEnabled(False)
        for reference, title in options:
            action = self.context_menu.addAction(title)
            action.setEnabled(reference not in self.references)
            action.triggered.connect(lambda _c=False, r=reference: self._add_reference(r))

    def _add_reference(self, reference):
        if reference not in self.references and len(self.references) < 20:
            self.references.append(reference)
            self._chips_changed()

    def _remove_reference(self, reference):
        self.references = [r for r in self.references if r != reference]
        self._chips_changed()

    def _chips_changed(self):
        clear_layout(self.chips)
        for reference in self.references:
            icon, label = reference_label(reference)
            chip = ContextChip(label, icon, removable=True)
            chip.remove.clicked.connect(lambda _c=False, r=reference: self._remove_reference(r))
            self.chips.addWidget(chip)
        self.chips.addStretch(1)

    def submit(self):
        text, kind, references = self.draft()
        if self.send.isEnabled() and 0 < len(text) <= MAX_TEXT:
            self.screen.host.submit_note(text, kind, source="screen", references=references)


class InboxScreen(Screen):
    slot = "inbox"
    watches = ("project", "inbox", "scans", "tasks")

    def __init__(self, host):
        super().__init__(host)
        self.filter = "all"
        self.selected_id = None
        self.all_rows = []
        self._goal_pending = False
        self.owner_actions = LocalOwnerActions(self)
        self.model = RowsModel(entry_columns())
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        self.empty_holder = QVBoxLayout()
        self.empty_holder.setContentsMargins(0, 0, 0, 0)
        root.addLayout(self.empty_holder)
        self.content = QSplitter(Qt.Horizontal)
        self.content.setChildrenCollapsible(False)
        root.addWidget(self.content, 1)
        self.content.addWidget(self._build_list())
        self.content.addWidget(self._build_right())
        self.content.setSizes([520, 700])
        self.content.setStretchFactor(1, 1)
        i18n.signals.changed.connect(self._language_changed)
        self.refresh()

    def _language_changed(self, _language):
        self.refresh()

    def _build_list(self):
        pane = ListPane()
        pane.setMinimumWidth(340)
        pane.resized.connect(self._label_tabs)
        layout = QVBoxLayout(pane)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        self.tabs = QTabBar()
        self.tabs.setUsesScrollButtons(
            True
        )  # labels keep their full text at narrow widths: the bar scrolls
        self.tabs.setElideMode(Qt.ElideNone)
        self.tabs.setProperty("tabs", "underline")
        self.tabs.setDrawBase(False)
        self.tabs.setExpanding(False)
        for _ in FILTERS:
            self.tabs.addTab("")
        self.tabs.currentChanged.connect(self._filter_changed)
        layout.addWidget(self.tabs)
        self.list_state = QVBoxLayout()
        layout.addLayout(self.list_state)
        self.table = build_table(self.model, badge_column=None, stretch=0)
        self.table.setItemDelegateForColumn(0, EntryDelegate(self.table))
        self.table.horizontalHeader().hide()
        self.table.setColumnHidden(1, True)
        self.table.setColumnHidden(2, True)
        self.table.verticalHeader().setSectionResizeMode(
            QHeaderView.ResizeToContents
        )  # the delegate's sizeHint sets the row; the density setting does not override it
        self.table.selectionModel().currentRowChanged.connect(self._row_changed)
        layout.addWidget(self.table, 1)
        self.status = QLabel()
        self.status.setProperty("text_style", "meta")
        self.status.setWordWrap(True)
        self.status.setContentsMargins(12, 4, 12, 4)
        layout.addWidget(self.status)
        return pane

    def _build_right(self):
        pane = QWidget()
        pane.setMinimumWidth(340)
        layout = QVBoxLayout(pane)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        self.scroll = QScrollArea()
        self.scroll.setWidgetResizable(True)
        self.scroll.setFrameShape(QFrame.NoFrame)
        self.scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        self.detail = QWidget()
        self.detail_layout = QVBoxLayout(self.detail)
        self.detail_layout.setContentsMargins(24, 18, 24, 12)
        self.detail_layout.setSpacing(12)
        self.scroll.setWidget(self.detail)
        layout.addWidget(self.scroll, 1)
        wrap = QWidget()
        wrap_layout = QVBoxLayout(wrap)
        wrap_layout.setContentsMargins(24, 8, 24, 16)
        self.accept_goal_button = QPushButton(tr("Принять цель проекта"))
        self.accept_goal_button.setProperty("role", "primary")
        self.accept_goal_button.setIcon(material_icon("check"))
        self.accept_goal_button.clicked.connect(self._accept_goal)
        self.accept_goal_button.hide()
        wrap_layout.addWidget(self.accept_goal_button)
        self.composer = Composer(self)
        wrap_layout.addWidget(self.composer)
        layout.addWidget(wrap)
        return pane

    # ---- composer bridge (called by host.inbox) -----------------------------------------------------------------
    def draft(self):
        return self.composer.draft()

    def set_draft(self, text, kind, references=()):
        self.composer.set_draft(text, kind, references)

    def clear_draft(self):
        self.composer.clear_draft()

    def set_submission_state(self, ready, pending, reason):
        self.composer.set_submission_state(ready, pending, reason)

    # ---- list ---------------------------------------------------------------------------------------------------
    def _matches(self, entry):
        return {
            "all": True,
            "unread": not entry.get("read_at"),
            "goal": entry.get("kind") == "proposed_goal",
            "done": is_processed(entry),
        }[self.filter]

    def _label_tabs(self):
        narrow = self.content.widget(0).width() < 440
        for index, (key, label, short) in enumerate(FILTERS):
            count = sum(
                1
                for e in self.all_rows
                if {
                    "all": True,
                    "unread": not e.get("read_at"),
                    "goal": e.get("kind") == "proposed_goal",
                    "done": is_processed(e),
                }[key]
            )
            self.tabs.setTabText(
                index, trf("{name} · {n}", name=tr(short if narrow else label), n=count)
            )
            self.tabs.setTabToolTip(index, tr(label))

    def _filter_changed(self, index):
        if 0 <= index < len(FILTERS) and FILTERS[index][0] != self.filter:
            self.filter = FILTERS[index][0]
            self._fill_rows()

    def _row_changed(self, current, _previous):
        if current.isValid() and current.row() < len(self.model.rows):
            self.selected_id = self.model.rows[current.row()].get("id")
            self._fill_detail()

    def _fill_rows(self):
        rows = sorted(
            (e for e in self.all_rows if self._matches(e)),
            key=lambda e: e.get("created_at") or "",
            reverse=True,
        )
        self.model.set_rows(rows)
        index = next(
            (i for i, r in enumerate(rows) if r.get("id") == self.selected_id), 0 if rows else None
        )
        if index is not None:
            self.table.selectRow(index)
            self.selected_id = rows[index].get("id")
        else:
            self.selected_id = None
        self._fill_detail()

    def refresh(self):
        host = self.host
        status, _text = project_state(host, errors=("observer",))
        if status == "none":
            show_empty(
                self.empty_holder,
                self.content,
                no_project_panel(host, "Откройте проект, чтобы увидеть входящие и писать заметки"),
            )
            return
        if status == "loading":
            show_empty(
                self.empty_holder,
                self.content,
                StatePanel(
                    "loading", "Загрузка проекта…", "Читаем сохранённые данные проекта из ядра"
                ),
            )
            return
        show_empty(self.empty_holder, self.content, None)
        self.all_rows = list(host.inbox_model.rows)
        self._label_tabs()
        clear_layout(self.list_state)
        error = host.screen_errors.get("observer")
        if error:
            self.list_state.addWidget(
                StatePanel(
                    "error",
                    "Не удалось получить входящие",
                    error,
                    action=("Повторить", host.refresh_project),
                )
            )
        elif host.inbox_revision is None:
            self.list_state.addWidget(StatePanel("loading", "Загрузка входящих…"))
        elif not self.all_rows:
            self.list_state.addWidget(
                StatePanel(
                    "empty",
                    "Во входящих пока пусто",
                    "Запись появится после явного нажатия «Сохранить во входящие»",
                )
            )
        shown = not error and host.inbox_revision is not None and bool(self.all_rows)
        self.table.setVisible(shown)
        self._fill_rows()
        unread = host.inbox_unread
        text = (
            trf("Не прочитано: {n}", n=number(unread))
            if unread is not None
            else tr("Непрочитанные не измерены")
        )
        if len(self.all_rows) >= 100:
            text += " · " + tr("показаны последние 100 записей")
        self.status.setText(text)
        self.status.setToolTip(
            tr("Считает ядро по получателю этого окна; чтение записи агентом оно пока не различает")
            + " · "
            + tr("Недоступно в этой версии ядра")
        )
        self.composer.show_error(host.screen_errors.get("inbox-submit"))

    # ---- detail -------------------------------------------------------------------------------------------------
    def _fill_detail(self):
        clear_layout(self.detail_layout)
        entry = next((e for e in self.model.rows if e.get("id") == self.selected_id), None)
        layout = self.detail_layout
        if entry is None:
            self.accept_goal_button.hide()
            layout.addWidget(
                StatePanel(
                    "empty", "Выберите запись", "Здесь появятся текст, стадии и разбор записи"
                )
            )
            layout.addStretch(1)
            return
        can_accept_goal = (
            entry.get("kind") == "proposed_goal"
            and entry.get("goal_state") == "proposed"
            and type(self.host.inbox_revision) is int
            and bool(self.host.core_executable)
        )
        self.accept_goal_button.setVisible(can_accept_goal)
        self.accept_goal_button.setEnabled(can_accept_goal and not self._goal_pending)
        kind, label = KINDS.get(entry.get("kind"), ("mut", entry.get("kind") or "—"))
        head = QHBoxLayout()
        badge = QLabel(tr(label))
        badge.setProperty("badge", kind)
        ident = QLabel(str(entry.get("id") or "").replace("inbox:", "")[:13])
        ident.setProperty("text_style", "mono")
        stamp = QLabel(local_stamp(entry.get("created_at"), True) or tr("Нет данных"))
        stamp.setProperty("text_style", "meta")
        head.addWidget(badge)
        head.addWidget(ident)
        head.addStretch(1)
        head.addWidget(stamp)
        layout.addLayout(head)
        body = QLabel(entry.get("text") or "")
        body.setWordWrap(True)
        body.setTextInteractionFlags(Qt.TextSelectableByMouse)
        layout.addWidget(body)
        refs = entry.get("references") or []
        if refs:
            chips = QHBoxLayout()
            for reference in refs[:6]:
                icon, text = reference_label(reference)
                button = QPushButton(text)
                button.setIcon(material_icon(icon))
                button.clicked.connect(
                    lambda _c=False, r=reference: self.host.navigation.select_section(
                        REFERENCE_SECTIONS.get(r.partition(":")[0], "work")
                    )
                )
                chips.addWidget(button)
            chips.addStretch(1)
            layout.addLayout(chips)
        layout.addWidget(StageChain(entry))
        section = QLabel(tr("Разбор").upper())
        section.setProperty("text_style", "overline")
        layout.addWidget(section)
        triage = entry.get("triage") or []
        if not triage:
            hint = QLabel(
                tr(
                    "Разбора пока нет: его записывает агент через MCP, окно показывает его без перезапуска."
                )
            )
            hint.setProperty("text_style", "meta")
            hint.setWordWrap(True)
            layout.addWidget(hint)
        for receipt in triage:
            row = QLabel(
                f"{local_stamp(receipt.get('recorded_at'), True) or ''} · {tr(OUTCOMES.get(receipt.get('kind'), receipt.get('kind') or ''))} · {receipt.get('actor') or ''}\n{receipt.get('reason') or ''}"
            )
            row.setWordWrap(True)
            row.setTextInteractionFlags(Qt.TextSelectableByMouse)
            layout.addWidget(row)
        reply = QHBoxLayout()
        reply_title = QLabel(tr("Ответ агента").upper())
        reply_title.setProperty("text_style", "overline")
        reply.addWidget(reply_title)
        reply.addWidget(waiting_badge(944))
        reply.addStretch(1)
        layout.addLayout(reply)
        layout.addWidget(
            Note(
                "info",
                tr("«Сохранено» не значит «агент работает»."),
                tr("Стадии ставит агент через MCP; текстового ответа ядро пока не хранит."),
            )
        )
        layout.addStretch(1)

    def _accept_goal(self):
        entry = next((row for row in self.model.rows if row.get("id") == self.selected_id), None)
        if not entry or not self.accept_goal_button.isEnabled():
            return
        dialog = ModalDialog(
            "Принять цель проекта?",
            "Принятие запишет решение владельца и обновит план проекта.",
            icon="flag",
            tone="agent",
            width=560,
            parent=self,
        )
        note = QLabel(entry.get("text") or tr("Нет данных"))
        note.setWordWrap(True)
        note.setTextInteractionFlags(Qt.TextSelectableByMouse)
        dialog.body.addWidget(note)
        dialog.add_button("Отмена", "text", on_click=dialog.reject)
        dialog.add_button("Принять цель", "primary", icon="check", on_click=dialog.accept)
        if dialog.exec_() != QDialog.Accepted:
            return
        try:
            arguments = accept_goal_arguments(
                self.host.project_directory,
                entry.get("id"),
                self.host.inbox_revision,
            )
        except ValueError as exc:
            self.status.setText(str(exc))
            return
        self._goal_pending = True
        self.accept_goal_button.setEnabled(False)
        project = self.host.project_directory
        self.status.setText(tr("Принимаем цель через локальный CLI ядра…"))
        self.owner_actions.submit(
            self.host.core_executable,
            arguments,
            lambda result: self._goal_accepted(result, project),
            lambda message: self._goal_failed(message, project),
            self,
        )

    def _goal_accepted(self, _result, project):
        self._goal_pending = False
        if self.host.project_directory != project:
            return
        self.status.setText(tr("Цель принята владельцем."))
        self.host.refresh_project()

    def _goal_failed(self, message, project):
        self._goal_pending = False
        if self.host.project_directory != project:
            return
        self.status.setText(message)
        self.accept_goal_button.setEnabled(True)
