"""Settings → Агент (sheet SetAgent): project access, permissions and the agent action log.

Data comes from optional context hooks (both must be cheap, they run in the UI thread):
  agent_projects() -> [{"name": str, "meta": str}]            projects the agent may be given
  agent_log(limit) -> [{"time", "text", "project", "kind"}]   kind: scan | tasks | wait | task | deny
  open_agent_log()                                            opens the full log
Without a hook the section says "Нет данных" instead of showing zeros or sample rows.
"""

from __future__ import annotations

from PyQt5.QtCore import QSize, Qt
from PyQt5.QtWidgets import (
    QFrame,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from ... import theming
from ...i18n import tr, trf
from ...screens.scan_common import StatusBadge
from ...settings_store import Setting
from ..controls import Note, SettingRow, Switch
from ..icons import material_icon
from .helpers import column_stack, group_label, keyed, page

ID, ICON, TITLE = "agent", "smart_toy", "Агент"
HINT = "Что агент видит и что может делать через MCP"

SCHEMA = (
    Setting("agent.projects", "", str),  # allowed project names, one per line; empty = none
    Setting("agent.run_scans", False, bool),
    Setting("agent.change_tasks", True, bool),
    Setting("agent.show_actions", True, bool),
)
LOG_LIMIT = 5
UNAVAILABLE = "Недоступно в этой сборке"
# kind -> (badge tone, label, icon); tones and icons follow the SetAgent canvas status tokens.
KINDS = {
    "scan": ("info", "скан", "progress_activity"),
    "tasks": ("goal", "задачи", "smart_toy"),
    "wait": ("warn", "ждёт вас", "rate_review"),
    "task": ("ok", "задача", "verified"),
    "deny": ("err", "отказ", "block"),
}


def allowed(store):
    return {name for name in store.get("agent.projects").split("\n") if name}


class CheckBox(QToolButton):
    """18 px checkbox: native checkable button, check mark drawn from the theme."""

    def __init__(self, accessible_name, checked=False):
        super().__init__()
        self.setProperty("cb", True)
        self.setCheckable(True)
        self.setChecked(checked)
        self.setAccessibleName(accessible_name)
        self.setCursor(Qt.PointingHandCursor)
        self.setIconSize(QSize(14, 14))
        self.toggled.connect(self._icon)
        theming.signals.changed.connect(self._icon)
        self._icon()

    def _icon(self, *_args):
        self.setIcon(
            material_icon("check", theming.roles()["on_primary"])
            if self.isChecked()
            else material_icon("")
        )


class ProjectRow(QFrame):
    """Checkbox + name + grey meta; a click anywhere on the row toggles it."""

    def __init__(self, store, project):
        super().__init__()
        self.name = project["name"]
        self.setProperty("list_row", True)
        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 4, 0, 4)
        layout.setSpacing(8)
        self.checkbox = CheckBox(self.name, self.name in allowed(store))
        self.checkbox.toggled.connect(lambda on: self._save(store, on))
        layout.addWidget(self.checkbox)
        layout.addWidget(QLabel(self.name))
        meta = QLabel(project.get("meta", ""))
        meta.setProperty("text_style", "meta")
        layout.addWidget(meta)
        layout.addStretch(1)

    def _save(self, store, on):
        names = allowed(store)
        if on:
            names.add(self.name)
        else:
            names.discard(self.name)
        store.set("agent.projects", "\n".join(sorted(names)))

    def mousePressEvent(self, event):
        if event.button() == Qt.LeftButton:
            self.checkbox.toggle()


def _projects_block(store, context):
    projects = context.request("agent_projects")
    box = QWidget()
    layout = QVBoxLayout(box)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(0)
    layout.addWidget(group_label("Доступ к проектам"))
    hint = QLabel()
    hint.setProperty("text_style", "meta")
    hint.setWordWrap(True)
    layout.addWidget(hint)
    box.hint = hint
    box.rows = []
    if projects is None:
        hint.setText(tr("Агент видит только отмеченные · нет данных"))
        layout.addWidget(_no_data("Список проектов недоступен в этой сборке"))
        return box

    def update_count(*_args):
        hint.setText(
            trf(
                "Агент видит только отмеченные · {n} из {total}",
                n=sum(r.checkbox.isChecked() for r in box.rows),
                total=len(box.rows),
            )
        )

    for project in projects:
        row = ProjectRow(store, project)
        row.checkbox.toggled.connect(update_count)
        box.rows.append(row)
        layout.addWidget(row)
    if not projects:
        layout.addWidget(_no_data("Проектов пока нет"))
    update_count()
    return box


def _no_data(text):
    label = QLabel(text)
    label.setProperty("na", True)
    label.setWordWrap(True)
    label.setContentsMargins(0, 8, 0, 8)
    return label


def _log_row(entry):
    badge, label, icon = KINDS.get(entry.get("kind"), ("mut", str(entry.get("kind", "")), "help"))
    row = QFrame()
    row.setProperty("list_row", True)
    layout = QHBoxLayout(row)
    layout.setContentsMargins(0, 8, 0, 8)
    layout.setSpacing(8)
    time = QLabel(entry.get("time", ""))
    time.setProperty("text_style", "mono")
    time.setFixedWidth(40)
    layout.addWidget(time)
    text = QVBoxLayout()
    text.setSpacing(0)
    main = QLabel(entry.get("text", ""))
    main.setWordWrap(True)
    sub = QLabel(entry.get("project", ""))
    sub.setProperty("text_style", "meta")
    text.addWidget(main)
    text.addWidget(sub)
    layout.addLayout(text, 1)
    chip = StatusBadge()
    chip.set_state(badge, label, icon)
    layout.addWidget(chip, 0, Qt.AlignVCenter)
    return row


def _log_block(context):
    entries = context.request("agent_log", limit=LOG_LIMIT)
    box = QWidget()
    layout = QVBoxLayout(box)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(0)
    layout.addWidget(group_label("ЖУРНАЛ ДЕЙСТВИЙ · ПОСЛЕДНИЕ {n}", n=LOG_LIMIT))
    if entries is None:
        layout.addWidget(_no_data("Нет данных"))
    elif not entries:
        layout.addWidget(_no_data("Агент ничего не делал"))
    else:
        for entry in entries[:LOG_LIMIT]:
            layout.addWidget(_log_row(entry))
    full = QPushButton("Весь журнал")
    full.setIcon(material_icon("history"))
    if context.can("open_agent_log"):
        full.clicked.connect(lambda: context.request("open_agent_log"))
    else:
        full.setEnabled(False)
        full.setToolTip(UNAVAILABLE)
    # The configuration sheet (SetAgentConfig) has no screen in this build yet: shown disabled, not hidden.
    configure = QPushButton("Что агент может настраивать")
    configure.setIcon(material_icon("tune"))
    configure.setEnabled(False)
    configure.setToolTip(UNAVAILABLE)
    footer = QHBoxLayout()
    footer.setContentsMargins(0, 8, 0, 0)
    footer.setSpacing(8)
    footer.addWidget(full)
    footer.addWidget(configure)
    footer.addStretch(1)
    layout.addLayout(footer)
    return box


def _later_switch(title, description):
    """Stored in the schema but unused: shown off and disabled, never written."""
    switch = Switch(title, False)
    switch.setEnabled(False)
    return SettingRow(title, description, switch)


def build_page(store, context):
    paid = Switch("Платные провайдеры", True)
    paid.setEnabled(False)
    paid.setToolTip("Всегда включено")
    left = [
        _projects_block(store, context),
        group_label("Права"),
        keyed(
            _later_switch("Запускать сканы", "С лимитами и подтверждениями проекта"),
            "agent.run_scans",
        ),
        keyed(
            _later_switch("Менять задачи и статусы", "Иначе — только предлагать во «Входящие»"),
            "agent.change_tasks",
        ),
        SettingRow("Платные провайдеры", "Всегда спрашивать о расходе · не отключается", paid),
    ]
    right = [
        keyed(
            _later_switch(
                "Показывать действия агента", "Плашка и тосты, когда агент меняет проект"
            ),
            "agent.show_actions",
        ),
        _log_block(context),
        Note(
            "info",
            "Что это меняет.",
            "Границы агента.<br>Права работают в ядре, а не в окне: CLI и любой MCP-клиент получают те же ограничения.",
        ),
    ]
    right[-0].setContentsMargins(0, 12, 0, 0)
    return page(column_stack(left, right))
