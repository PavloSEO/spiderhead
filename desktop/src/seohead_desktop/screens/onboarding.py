"""Screen «Первый запуск · мастер» (sheet Onboarding.dc.html): three steps, only locally checkable facts.

Step 1 the projects folder (setting general.projects_folder), step 2 the core (found or not; version and
compatibility are not measured yet: #979), step 3 the display (С агентом / Простой) plus theme and language.
Connecting an agent and the MCP server belong to roadmap step 7 and are shown as unavailable. Disk facts are read
by a worker, never in the UI thread.
"""

from __future__ import annotations

import os
import shutil
from pathlib import Path

from PyQt5.QtCore import QSize, Qt
from PyQt5.QtGui import QIcon
from PyQt5.QtWidgets import (
    QFileDialog,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QScrollArea,
    QStackedWidget,
    QVBoxLayout,
    QWidget,
)

from .. import i18n, theming
from ..cli_install import InstallCLIButton
from ..common import ROOT
from ..core_discovery import discover_core
from ..mcp_integration import PermissionDialog
from ..ui.controls import Note, Segmented, polish
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import waiting_badge
from .base import Screen
from .start import run_background, tilde

tr, trf = i18n.tr, i18n.trf

STEPS = ("Папка проектов", "Проверка ядра", "Отображение")
LOW_DISK = 10 * 1000**3
UNAVAILABLE = "Недоступно в этой сборке"
CORE_VERSION_ISSUE = 979


def folder_facts(path):
    """Free space, write access and existing projects of the projects folder. Blocking: call from a worker."""
    base = Path(path).expanduser()
    exists = base.is_dir()
    anchor = base
    while not anchor.exists() and anchor != anchor.parent:
        anchor = anchor.parent
    facts = {
        "exists": exists,
        "free": shutil.disk_usage(anchor).free,
        "writable": os.access(base if exists else anchor, os.W_OK),
        "projects": 0,
    }
    if exists:
        with os.scandir(base) as entries:
            for number, entry in enumerate(entries):
                if number >= 500:
                    break
                if entry.is_dir() and os.path.isfile(os.path.join(entry.path, "project.json")):
                    facts["projects"] += 1
    return facts


def core_facts(path):
    """Is the remembered core command still an executable file. Blocking: call from a worker."""
    return {"path": path, "ok": bool(path) and os.path.isfile(path) and os.access(path, os.X_OK)}


def size_text(value):
    gigabytes = value / 1000**3
    return trf("{value} ГБ", value=i18n.Num(gigabytes, 0 if gigabytes >= 10 else 1))


class Option(QPushButton):
    """Checkable choice card: icon + title + description."""

    def __init__(self, icon, title, text, parent=None):
        super().__init__(parent)
        self.setCheckable(True)
        self.setProperty("opt", True)
        self.setAccessibleName(tr(title))
        self.setCursor(Qt.PointingHandCursor)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(16, 16, 16, 16)
        layout.setSpacing(8)
        head = QHBoxLayout()
        head.setSpacing(8)
        head.addWidget(MaterialIconLabel(icon, 20, color="role:text_2"))
        name = QLabel(tr(title))
        name.setProperty("text_style", "section")
        head.addWidget(name, 1)
        layout.addLayout(head)
        body = QLabel(tr(text))
        body.setProperty("text_style", "meta")
        body.setWordWrap(True)
        layout.addWidget(body)
        for label in self.findChildren(QLabel):
            label.setAttribute(Qt.WA_TransparentForMouseEvents)


class OnboardingScreen(Screen):
    watches = ()
    chrome_free = True  # SHELL-CANON §7: no project / scan switchers

    def __init__(self, host):
        super().__init__(host)
        self.step = 0
        self._generation = 0
        self.folder_state = None  # None until the worker answered
        self.core_state = None
        self.setObjectName("startPage")
        self.setAttribute(Qt.WA_StyledBackground, True)
        row = QHBoxLayout(self)
        row.setContentsMargins(24, 32, 24, 32)
        row.addStretch(1)
        card = QFrame()
        card.setProperty("card", "panel")
        card.setMaximumWidth(900)
        card.setMaximumHeight(680)
        row.addWidget(card, 100)
        row.addStretch(1)
        layout = QHBoxLayout(card)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        layout.addWidget(self._rail())
        content = QVBoxLayout()
        content.setContentsMargins(0, 0, 0, 0)
        content.setSpacing(0)
        self.stack = QStackedWidget()
        self.stack.addWidget(self._folder_page())
        self.stack.addWidget(self._core_page())
        self.stack.addWidget(self._display_page())
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        scroll.setWidget(self.stack)
        content.addWidget(scroll, 1)
        content.addWidget(self._footer())
        layout.addLayout(content, 1)
        self.go(0)

    # frame: left rail with brand and steps (sheet Onboarding: nav rail 220 px)
    def _rail(self):
        box = QFrame()
        box.setProperty("onb_part", "rail")
        box.setFixedWidth(220)
        column = QVBoxLayout(box)
        column.setContentsMargins(16, 20, 16, 20)
        column.setSpacing(4)
        head = QHBoxLayout()
        head.setSpacing(8)
        mark = QLabel()
        mark.setPixmap(QIcon(str(ROOT / "assets/app/seohead-small.svg")).pixmap(28, 28))
        mark.setAccessibleName("SpiderHead")
        head.addWidget(mark)
        texts = QVBoxLayout()
        texts.setSpacing(0)
        brand = QLabel("SpiderHead")
        brand.setProperty("brand", "word")
        meta = QLabel(trf("Первый запуск · {n} шага", n=len(STEPS)))
        meta.setProperty("text_style", "meta")
        texts.addWidget(brand)
        texts.addWidget(meta)
        head.addLayout(texts, 1)
        column.addLayout(head)
        column.addSpacing(16)
        self.dots, self.names, self.items = [], [], []
        for title in STEPS:
            item = QFrame()
            item.setProperty("rail_item", True)
            line = QHBoxLayout(item)
            line.setContentsMargins(8, 8, 8, 8)
            line.setSpacing(8)
            dot = QLabel()
            dot.setFixedSize(22, 22)
            dot.setAlignment(Qt.AlignCenter)
            name = QLabel(tr(title))
            name.setWordWrap(True)
            line.addWidget(dot)
            line.addWidget(name, 1)
            self.dots.append(dot)
            self.names.append(name)
            self.items.append(item)
            column.addWidget(item)
        column.addStretch(1)
        self.skip_button = QPushButton(tr("Пропустить"))
        self.skip_button.setProperty("role", "text")
        self.skip_button.clicked.connect(self.skip)
        column.addWidget(self.skip_button, 0, Qt.AlignLeft)
        return box

    def _footer(self):
        box = QFrame()
        box.setProperty("onb_part", "footer")
        line = QHBoxLayout(box)
        line.setContentsMargins(28, 12, 28, 12)
        line.setSpacing(8)
        self.counter = QLabel()
        self.counter.setProperty("text_style", "meta")
        line.addWidget(self.counter)
        line.addStretch(1)
        self.back_button = QPushButton(tr("Назад"))
        self.next_button = QPushButton(tr("Далее"))
        self.next_button.setProperty("role", "primary")
        self.next_button.setIcon(material_icon("arrow_forward", theming.roles()["on_primary"]))
        self.next_button.setLayoutDirection(Qt.RightToLeft)
        self.finish_button = QPushButton(tr("Готово · создать первый проект"))
        self.finish_button.setProperty("role", "primary")
        self.back_button.clicked.connect(lambda: self.go(self.step - 1))
        self.next_button.clicked.connect(lambda: self.go(self.step + 1))
        self.finish_button.clicked.connect(self.finish)
        for button in (self.back_button, self.next_button, self.finish_button):
            line.addWidget(button)
        return box

    @staticmethod
    def _body(title, text=""):
        page = QWidget()
        layout = QVBoxLayout(page)
        layout.setContentsMargins(28, 24, 28, 24)
        layout.setSpacing(16)
        heading = QLabel(tr(title))
        heading.setProperty("text_style", "title")
        heading.setWordWrap(True)
        layout.addWidget(heading)
        lead = QLabel(tr(text))
        lead.setWordWrap(True)
        lead.setVisible(bool(text))
        layout.addWidget(lead)
        layout.addStretch(1)
        return page, layout, heading, lead

    # step 1: folder
    def _folder_page(self):
        page, layout, _heading, _lead = self._body(
            "Где хранить проекты",
            "В папке будут лежать проекты, базы сканов и отчёты. Её можно открыть в Finder и забэкапить целиком.",
        )
        field = QFrame()
        field.setProperty("onb_field", True)
        line = QHBoxLayout(field)
        line.setContentsMargins(12, 0, 4, 0)
        line.setSpacing(8)
        line.addWidget(MaterialIconLabel("folder", 18, color="role:text_2"))
        self.folder_label = QLabel()
        self.folder_label.setProperty("text_style", "mono")
        self.folder_label.setWordWrap(
            True
        )  # a long path wraps instead of widening the card past the window
        self.folder_label.setTextInteractionFlags(Qt.TextSelectableByMouse)
        line.addWidget(self.folder_label, 1)
        self.choose_button = QPushButton(tr("Выбрать…"))
        self.choose_button.clicked.connect(self.choose_folder)
        line.addWidget(self.choose_button)
        field.setMinimumHeight(40)
        layout.insertWidget(layout.count() - 1, field)
        grid = QGridLayout()
        grid.setHorizontalSpacing(16)
        grid.setVerticalSpacing(8)
        grid.setColumnMinimumWidth(0, 200)
        grid.setColumnStretch(1, 1)
        self.fact_values = {}
        for number, (key, caption) in enumerate(
            (
                ("free", "Свободно на диске"),
                ("write", "Права на запись"),
                ("projects", "Существующие проекты"),
            )
        ):
            name = QLabel(tr(caption))
            name.setProperty("text_style", "meta")
            value = QLabel()
            value.setWordWrap(True)
            grid.addWidget(name, number, 0, Qt.AlignTop)
            grid.addWidget(value, number, 1)
            self.fact_values[key] = value
        layout.insertLayout(layout.count() - 1, grid)
        self.folder_note = Note(
            "info", "Размер скана зависит от сайта и настроек.", "Держите минимум 10 ГБ свободными."
        )
        layout.insertWidget(layout.count() - 1, self.folder_note)
        self.folder_warning = Note(
            "warn",
            "Папки пока нет.",
            "Создайте её в Finder или выберите другую: проект создаётся внутри существующей папки.",
        )
        self.folder_warning.hide()
        layout.insertWidget(layout.count() - 1, self.folder_warning)
        return page

    def projects_folder(self):
        return str(self.host.prefs.get("general.projects_folder") or "")

    def load_folder(self):
        path = self.projects_folder()
        self.folder_label.setText(tilde(str(Path(path).expanduser())) if path else tr("Не выбрана"))
        self.folder_state = None
        for value in self.fact_values.values():
            value.setText(tr("Проверяется…"))
        self._generation += 1
        generation = self._generation
        run_background(
            self.host.pool,
            lambda: folder_facts(path),
            lambda r: self._folder_loaded(generation, r),
            self,
        )

    def _folder_loaded(self, generation, facts):
        if generation != self._generation:
            return
        self.folder_state = facts
        if isinstance(facts, Exception):
            for value in self.fact_values.values():
                value.setText(tr("Нет данных"))
            return
        free = facts["free"]
        low = free < LOW_DISK
        self.fact_values["free"].setText(
            trf("{size} — мало места", size=size_text(free)) if low else size_text(free)
        )
        self.fact_values["free"].setProperty("low", low)
        polish(self.fact_values["free"])
        self.fact_values["write"].setText(
            tr("есть" if facts["writable"] else "нет — выберите другую папку")
        )
        count = facts["projects"]
        self.fact_values["projects"].setText(
            trf("найдено: {n}", n=count) if count else tr("не найдены — начнём с пустой папки")
        )
        self.folder_warning.setVisible(not facts["exists"])
        if not facts["exists"]:
            self.fact_values["projects"].setText(tr("папки нет"))

    def choose_folder(self):
        start = str(Path(self.projects_folder()).expanduser())
        directory = QFileDialog.getExistingDirectory(self, tr("Папка проектов"), start)
        if directory:
            self.host.prefs.set("general.projects_folder", directory)
            self.load_folder()

    # step 2: core
    def _core_page(self):
        page, layout, self.core_heading, self.core_lead = self._body(
            "Ядро найдено",
            "Сканы выполняет ядро seohead — то же, что в CLI и MCP. Окно приложения — только оболочка.",
        )
        card = QFrame()
        card.setProperty("card", "panel")
        rows = QVBoxLayout(card)
        rows.setContentsMargins(16, 12, 16, 12)
        rows.setSpacing(8)
        self.core_line = self._check_row(rows)
        self.version_line = self._check_row(rows)
        self.version_text.setText(tr("Версия и совместимость ядра — нет данных"))
        self.version_icon.set_material_icon("schedule", "role:text_muted")
        self.version_line.layout().addWidget(waiting_badge(CORE_VERSION_ISSUE))
        layout.insertWidget(layout.count() - 1, card)
        buttons = QHBoxLayout()
        buttons.setSpacing(8)
        self.recheck_button = QPushButton(tr("Проверить снова"))
        self.recheck_button.setIcon(material_icon("refresh"))
        self.recheck_button.clicked.connect(self.recheck_core)
        self.other_core_button = QPushButton(tr("Указать другое ядро"))
        self.other_core_button.setIcon(material_icon("tune"))
        self.other_core_button.clicked.connect(lambda: self.host.open_settings("core"))
        buttons.addWidget(self.recheck_button)
        buttons.addWidget(self.other_core_button)
        buttons.addWidget(InstallCLIButton(self))
        buttons.addStretch(1)
        layout.insertLayout(layout.count() - 1, buttons)
        return page

    def _check_row(self, parent_layout):
        box = QWidget()
        line = QHBoxLayout(box)
        line.setContentsMargins(0, 0, 0, 0)
        line.setSpacing(8)
        icon = MaterialIconLabel("check_circle", 18, color="role:success")
        text = QLabel()
        text.setWordWrap(True)
        text.setTextFormat(Qt.PlainText)
        line.addWidget(icon)
        line.addWidget(text, 1)
        parent_layout.addWidget(box)
        if not hasattr(self, "core_icon"):
            self.core_icon, self.core_text = icon, text
        else:
            self.version_icon, self.version_text = icon, text
        return box

    def load_core(self):
        path = self.host.core_executable
        self.core_state = None
        self._generation += 1
        generation = self._generation
        self.core_text.setText(tr("Проверяется…"))
        run_background(
            self.host.pool,
            lambda: core_facts(path),
            lambda r: self._core_loaded(generation, r),
            self,
        )

    def _core_loaded(self, generation, facts):
        if generation != self._generation:
            return
        self.core_state = facts
        found = not isinstance(facts, Exception) and facts["ok"]
        self.core_heading.setText(tr("Ядро найдено" if found else "Ядро не найдено"))
        self.core_icon.set_material_icon(
            "check_circle" if found else "error", "role:success" if found else "role:error"
        )
        if found:
            self.core_text.setText(trf("Команда ядра: {path}", path=tilde(facts["path"])))
        elif self.host.core_executable:
            self.core_text.setText(
                trf("Команда ядра не запускается: {path}", path=tilde(self.host.core_executable))
            )
        else:
            self.core_text.setText(tr("Ядро seohead не найдено ни в одном из известных мест"))
        self.version_line.setVisible(found)

    def recheck_core(self):
        if not self.host.core_executable or self.host.prefs.get("core.custom"):
            found = discover_core(self.host.prefs)
            if found:
                self.host.core_executable = found
                self.host.core_label.setText(tr("Ядро найдено"))
                self.host.core_label.setToolTip(found)
        self.load_core()

    # step 3: display
    def _display_page(self):
        page, layout, _heading, _lead = self._body("Как вы будете работать")
        grid = QGridLayout()
        grid.setSpacing(12)
        self.option_agent = Option(
            "smart_toy",
            "С агентом",
            "Цель, задачи, входящие от агента. Агент читает сканы через MCP и предлагает задачи — вы подтверждаете.",
        )
        self.option_simple = Option(
            "person",
            "Простой",
            "Скан, проблемы, задачи и отчёт. Без агента и без входящих. Переключить можно в любой момент.",
        )
        grid.addWidget(self.option_agent, 0, 0)
        grid.addWidget(self.option_simple, 0, 1)
        self.option_agent.clicked.connect(lambda: self.pick("agent"))
        self.option_simple.clicked.connect(lambda: self.pick("simple"))
        layout.insertLayout(layout.count() - 1, grid)

        # Sheet Onboarding: one line; the permission window is the same as in Settings → MCP-сервер.
        self.mcp_card = QFrame()
        self.mcp_card.setProperty("card", "panel")
        line = QHBoxLayout(self.mcp_card)
        line.setContentsMargins(16, 12, 16, 12)
        line.setSpacing(12)
        line.addWidget(MaterialIconLabel("hub", 20, color="role:text_muted"))
        texts = QVBoxLayout()  # the link sits under the title so the card keeps its width at 800 px
        texts.setSpacing(2)
        title = QLabel(tr("Подключить агента"))
        title.setProperty("text_style", "control")
        texts.addWidget(title)
        link = QPushButton(tr("Подробнее о MCP и подключении Claude"))
        link.setProperty("role", "text")
        link.setCursor(Qt.PointingHandCursor)
        link.clicked.connect(lambda: self.host.open_settings("mcp"))
        texts.addWidget(link, 0, Qt.AlignLeft)
        line.addLayout(texts, 1)
        self.connect_button = QPushButton(tr("Прописать…"))
        self.connect_button.setProperty("role", "tonal")
        self.connect_button.setEnabled(bool(self.host.core_executable))
        self.connect_button.setToolTip(
            tr("Подключение — в Настройках → MCP-сервер. Запись в конфиг требует разрешения.")
            if self.host.core_executable
            else tr("CLI ядра seohead не найден")
        )
        self.connect_button.clicked.connect(self.connect_agent)
        line.addWidget(self.connect_button)
        layout.insertWidget(layout.count() - 1, self.mcp_card)
        self.later_note = Note(
            "info",
            "Агента можно подключить позже.",
            "Подключение — в Настройках → MCP-сервер. Запись в конфиг требует разрешения.",
        )
        layout.insertWidget(layout.count() - 1, self.later_note)

        look = (
            QGridLayout()
        )  # one choice per row: two side by side are wider than the 800 px window allows
        look.setHorizontalSpacing(12)
        look.setVerticalSpacing(8)
        self.theme_choice = Segmented(
            [("light", tr("Светлая")), ("dark", tr("Тёмная")), ("system", tr("Как в системе"))],
            self.host.prefs.get("view.theme"),
            tr("Тема"),
        )
        self.language_choice = Segmented(
            [("ru", "Русский"), ("en", "English")], self.host.prefs.get("view.language"), tr("Язык")
        )
        self.theme_choice.changed.connect(lambda value: self.host.prefs.set("view.theme", value))
        self.language_choice.changed.connect(
            lambda value: self.host.prefs.set("view.language", value)
        )
        for row, (caption, control) in enumerate(
            ((tr("Тема"), self.theme_choice), (tr("Язык"), self.language_choice))
        ):
            look.addWidget(QLabel(caption), row, 0)
            look.addWidget(control, row, 1, Qt.AlignLeft)
        look.setColumnStretch(2, 1)
        layout.insertLayout(layout.count() - 1, look)
        self.sync_display()
        return page

    def connect_agent(self):
        PermissionDialog(self.host.core_executable, ("claude-code",), self).exec_()

    def pick(self, mode):
        self.host.set_display(mode)
        self.sync_display()

    def sync_display(self):
        agent = self.host.display != "simple"
        self.option_agent.setChecked(agent)
        self.option_simple.setChecked(not agent)
        self.mcp_card.setVisible(agent)
        self.later_note.setVisible(agent)

    # navigation
    def go(self, step):
        self.step = max(0, min(len(STEPS) - 1, step))
        self.stack.setCurrentIndex(self.step)
        for number, (dot, name, item) in enumerate(zip(self.dots, self.names, self.items)):
            state = "on" if number == self.step else "done" if number < self.step else "todo"
            dot.setText("✓" if state == "done" else str(number + 1))
            for widget in (dot, name, item):
                widget.setProperty("step", state)
                polish(widget)
            dot.setProperty("step_dot", True)
            polish(dot)
        self.counter.setText(trf("Шаг {n} из {total}", n=self.step + 1, total=len(STEPS)))
        last = self.step == len(STEPS) - 1
        self.back_button.setVisible(self.step > 0)
        self.next_button.setVisible(not last)
        self.finish_button.setVisible(last)
        if self.step == 0:
            self.load_folder()
        elif self.step == 1:
            self.load_core()
        else:
            self.sync_display()

    def refresh(self):
        self.go(self.step)

    def _done(self):
        self.host.prefs.set("shell.onboarding_done", True)
        self.host.show_screen("start")

    def skip(self):
        self._done()

    def finish(self):
        self._done()
        self.host.extra_screens["start"].new_project()

    def sizeHint(self):
        return QSize(760, 640)
