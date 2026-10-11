"""«Инструмент · запуск» (sheet ToolRun): one core tool, its arguments and the CLI command that equals the form.

The item is the core's own catalogue entry (``seohead tool-catalog --include-arguments``); the host passes it in with
``set_tool``. Nothing runs from this screen. Running from the app, batch sources, options and results wait for the
core, so the result slot shows the neutral waiting state instead of sample rows.
"""

from __future__ import annotations

import shlex

from PyQt5.QtCore import Qt
from PyQt5.QtWidgets import QFrame, QGridLayout, QHBoxLayout, QLabel, QLineEdit, QPushButton, QVBoxLayout, QWidget

from ..i18n import tr
from ..ui.kit import PageHeader, StatePanel, unavailable_tip, waiting_badge
from .base import Screen
from .work import scrolled

# Russian titles of the tools the board shows; any other tool keeps its machine name.
TITLES = {"seo_hreflang_check": "Проверка hreflang"}
# Core issue that brings running from the app, batch sources and results (its hint shows in the tooltip).
RUN_ISSUE = 998
RUN_HINT = "Запуск инструмента из приложения, пакетный режим по списку URL и по скану, результат на экране"


def command_line(item, values):
    """The CLI command equal to the form: ``seohead <command> --<arg> <value>``; an empty value shows ``<arg>``."""
    parts = ["seohead", item["command"]]
    for argument in item.get("arguments", []):
        value = values.get(argument["name"], "").strip()
        parts += [f"--{argument['name']}", shlex.quote(value) if value else f"<{argument['name']}>"]
    return " ".join(parts)


def pill(kind, text):
    label = QLabel(tr(text))
    label.setProperty("badge", kind)
    return label


class ToolRunScreen(Screen):
    slot = ""
    watches = ()

    def __init__(self, host, item=None):
        super().__init__(host)
        self.item = None
        self.fields = {}
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        self.header = PageHeader("Инструмент")
        # running from the app waits for the core (RUN_ISSUE); the button stays visible in the board's place
        self.run_button = self.header.add_action(QPushButton(tr("Запустить")))
        self.run_button.setProperty("role", "primary")
        self.run_button.setEnabled(False)
        self.run_button.setToolTip(unavailable_tip("Запуск из приложения", RUN_ISSUE))
        self.content = QWidget()
        self.body = QVBoxLayout(self.content)
        self.body.setContentsMargins(24, 20, 24, 16)
        self.body.setSpacing(16)
        root.addWidget(scrolled(self.content), 1)
        self.body.addWidget(self.header)
        self.set_tool(item)

    def refresh(self):
        self.set_tool(self.item)

    def set_tool(self, item):
        """Show ``item`` (a catalogue entry from the core), or the empty state when there is none."""
        self.item = item
        self.fields = {}
        self._clear_body()
        if item is None:
            self.header.title.setText(tr("Инструмент"))
            self.header.set_meta("")
            self.body.addWidget(StatePanel("empty", "Инструмент не выбран",
                                           "Выберите инструмент в каталоге: его параметры и команда появятся здесь."))
            self.body.addStretch(1)
            return
        self.header.title.setText(tr(TITLES.get(item["name"], item["name"])))
        self.header.set_meta(item["command"])
        self.body.addWidget(self._badges(item))
        self.body.addWidget(self._form(item))
        self.body.addWidget(self._batch_note())
        self.body.addWidget(self._command_card())
        self.body.addWidget(StatePanel("waiting", "Результат появится после запуска",
                                       "Запуск из приложения ждёт ядра. Команду можно выполнить в терминале или через MCP-агента.",
                                       issue=RUN_ISSUE, hint=RUN_HINT))
        self.body.addStretch(1)

    def _clear_body(self):
        # the header (first item) stays; everything after it is rebuilt
        while self.body.count() > 1:
            widget = self.body.takeAt(1).widget()
            if widget is not None:
                widget.setParent(None)
                widget.deleteLater()

    def _badges(self, item):
        row = QWidget()
        layout = QHBoxLayout(row)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(8)
        layout.addWidget(pill("info", f"MCP · {item['name']}"))
        if item.get("network"):
            layout.addWidget(pill("mut", "сеть"))
        if not item.get("writes"):
            layout.addWidget(pill("ok", "только чтение"))
        layout.addStretch(1)
        return row

    def _form(self, item):
        card = QFrame()
        card.setProperty("card", "panel")
        grid = QGridLayout(card)
        grid.setContentsMargins(16, 14, 16, 14)
        grid.setHorizontalSpacing(16)
        grid.setVerticalSpacing(10)
        grid.setColumnMinimumWidth(0, 200)
        grid.setColumnStretch(1, 1)
        for row, argument in enumerate(item.get("arguments", [])):
            name = argument["name"]
            caption = QLabel(name + (" *" if argument.get("default") == "required" else ""))
            caption.setProperty("text_style", "control")
            field = QLineEdit()
            field.setPlaceholderText(f"<{name}>")
            field.setAccessibleName(name)
            field.textChanged.connect(self._update_command)
            self.fields[name] = field
            grid.addWidget(caption, row, 0, Qt.AlignVCenter)
            grid.addWidget(field, row, 1)
        return card

    def _batch_note(self):
        note = QFrame()
        note.setProperty("note", "info")
        layout = QHBoxLayout(note)
        layout.setContentsMargins(12, 8, 12, 8)
        text = QLabel(tr("Пакетный режим (список URL, скан проекта) и опции проверки пока не работают в приложении."))
        text.setWordWrap(True)
        text.setProperty("text_style", "meta")
        layout.addWidget(text, 1)
        layout.addWidget(waiting_badge(RUN_ISSUE, RUN_HINT), 0, Qt.AlignVCenter)
        return note

    def _command_card(self):
        card = QFrame()
        card.setProperty("card", "panel")
        layout = QVBoxLayout(card)
        layout.setContentsMargins(16, 14, 16, 14)
        layout.setSpacing(8)
        caption = QLabel(tr("Команда"))
        caption.setProperty("text_style", "section")
        layout.addWidget(caption)
        self.command = QLabel()
        self.command.setTextInteractionFlags(Qt.TextSelectableByMouse)
        self.command.setWordWrap(True)
        layout.addWidget(self.command)
        note = QLabel(tr("Агент вызывает то же самое по MCP-инструменту с этим именем."))
        note.setProperty("text_style", "meta")
        layout.addWidget(note)
        self._update_command()
        return card

    def _update_command(self, *_):
        if self.item is None:
            return
        values = {name: field.text() for name, field in self.fields.items()}
        self.command.setText(f"$ {command_line(self.item, values)}")
