"""Main-window methods: top bar (52 px) with project and scan pickers."""

from __future__ import annotations

import sys

from PyQt5.QtCore import QEvent, Qt
from PyQt5.QtWidgets import (
    QComboBox,
    QHBoxLayout,
    QLabel,
    QMenu,
    QPushButton,
    QToolButton,
    QWidget,
)

from . import theming
from .i18n import tr, trf
from .ui.icons import material_icon as icon
from .ui.kit import waiting_badge
from .ui.presentation import StateBadge, short_run_id
from .ui.shell import PickerButton


def scan_texts(combo, index, run_ids=None, compact=False):
    """(title, subtitle) of one saved-scan choice: «Скан №3 · r-1a2b» and «09.10.2026 12:34 · 1 314 URL · частичный».

    The number is the position among the loaded scans by creation time (the oldest is №1); r-… is the short run id.
    """
    row = combo.itemData(index) if index >= 0 else None
    if not isinstance(row, dict):
        return tr(combo.itemText(index)) if index >= 0 else "", "", ""
    rows = [(i, combo.itemData(i)) for i in range(combo.count()) if isinstance(combo.itemData(i), dict)]
    order = sorted(rows, key=lambda pair: (str(pair[1].get("created_at") or ""), -pair[0]))
    ordinal = next((n for n, (i, _r) in enumerate(order, 1) if i == index), 1)
    full_id = str((run_ids or {}).get(row.get("path")) or row.get("uuid") or "")
    run_id = short_run_id(full_id)
    if compact:
        title = trf("№{n} · {id}", n=ordinal, id=run_id) if run_id else trf("№{n}", n=ordinal)
    else:
        title = trf("Скан №{n} · {id}", n=ordinal, id=run_id) if run_id else trf("Скан №{n}", n=ordinal)
    from .screens.scan_common import number, parse_time

    stamp = parse_time(row.get("finished_at") or row.get("created_at"))
    parts = [stamp.astimezone().strftime("%d.%m.%Y %H:%M")] if stamp else []
    done = ((row.get("evidence") or {}).get("frontier") or {}).get("counts", {}).get("done")
    if type(done) is int:
        parts.append(f"{number(done)} URL")
    if row.get("crawl_partial") is True:
        parts.append(tr("частичный"))
    return title, " · ".join(parts), full_id


class _HideWhenDisabled(QPushButton):
    """Cancel control: present only while there is something to cancel."""

    def changeEvent(self, event):
        super().changeEvent(event)
        if event.type() == QEvent.EnabledChange:
            self.setVisible(self.isEnabled())


def icon_button(icon_name, accessible_name, tooltip=None):
    button = QToolButton()
    button.setIcon(icon(icon_name))
    button.setProperty("role", "icon")
    button.setAccessibleName(accessible_name)
    button.setToolTip(tooltip or accessible_name)
    return button


class ChromeMixin:
    def topbar(self):
        """One 52 px row (SHELL-CANON §2): navigation toggle, project and scan pickers, agent state, ⌘K, refresh, new scan.

        The brand lives in the window title, Dock, About, Start and onboarding, not in this row.
        """
        top = QWidget()
        top.setObjectName("topbar")
        top.setFixedHeight(theming.metrics()["layout"]["topbar"])
        layout = QHBoxLayout(top)
        layout.setContentsMargins(8, 0, 12, 0)
        layout.setSpacing(6)
        self.nav_toggle = icon_button("side_navigation", "Свернуть или развернуть навигацию")
        self.nav_toggle.clicked.connect(self.toggle_navigation)
        layout.addWidget(self.nav_toggle)

        # The combo boxes hold the project/scan choices (and signals the rest of the app uses);
        # the picker buttons show them and open a menu. The combos themselves are never shown.
        self.project_picker = QComboBox(self)
        self.project_picker.hide()
        self.project_picker.addItem("Проект не открыт", None)
        self.project_picker.addItem("Открыть другой проект…", {"action": "open"})
        self.project_picker.activated.connect(self.activate_project_picker)
        self.scan_picker = QComboBox(self)
        self.scan_picker.hide()
        self.scan_picker.addItem("Нет сохранённых сканов", None)
        self.scan_picker.setEnabled(False)
        self.scan_picker.currentIndexChanged.connect(self.select_scan_from_picker)
        self.project_button = PickerButton("workspaces", "Выбрать проект")
        self.project_button.setMinimumWidth(180)
        self.project_button.setMaximumWidth(360)
        self.scan_button = PickerButton("manage_search", "Выбрать сохранённый запуск")
        self.scan_button.setMinimumWidth(250)
        self.scan_button.setMaximumWidth(340)
        self.project_button.clicked.connect(lambda: self.show_picker_menu(self.project_picker, self.project_button))
        self.scan_button.clicked.connect(lambda: self.show_picker_menu(self.scan_picker, self.scan_button))
        for combo, button in ((self.project_picker, self.project_button), (self.scan_picker, self.scan_button)):
            combo.currentIndexChanged.connect(lambda _i, c=combo, b=button: self.sync_picker(c, b))
            combo.model().rowsInserted.connect(lambda *_a, c=combo, b=button: self.sync_picker(c, b))
            combo.model().rowsRemoved.connect(lambda *_a, c=combo, b=button: self.sync_picker(c, b))
            combo.model().modelReset.connect(lambda c=combo, b=button: self.sync_picker(c, b))
            combo.model().dataChanged.connect(lambda *_a, c=combo, b=button: self.sync_picker(c, b))
            combo.installEventFilter(self)
            self.sync_picker(combo, button)
        layout.addWidget(self.project_button)
        chevron = QLabel()
        chevron.setPixmap(icon("chevron_right", theming.roles()["text_muted"]).pixmap(18, 18))
        self.project_chevron = chevron
        layout.addWidget(chevron)
        layout.addWidget(self.scan_button)
        self.scan_state_badge = StateBadge("unknown")
        self.scan_state_badge.hide()
        layout.addWidget(self.scan_state_badge)
        layout.addStretch(1)

        self.agent_pill = QLabel("Агент подключён")
        self.agent_pill.setProperty("status_pill", "agent")
        self.agent_pill.hide()
        self.simple_pill = QPushButton("Простой режим")
        self.simple_pill.setProperty("status_pill", "simple")
        self.simple_pill.setIcon(icon("expand_more"))
        self.simple_pill.setLayoutDirection(Qt.RightToLeft)
        self.simple_pill.setAccessibleName("Простой режим: сменить отображение")
        self.simple_pill.clicked.connect(self.show_display_menu)
        self.simple_pill.hide()
        # Agent chip slot (canvas TopBar, mode «С агентом»): no heartbeat in the core yet, so a neutral badge, never a claim.
        self.agent_gap = waiting_badge(1238)
        layout.addWidget(self.agent_gap)
        layout.addWidget(self.agent_pill)
        layout.addWidget(self.simple_pill)
        self.cancel_button = _HideWhenDisabled("Отменить чтение")
        self.cancel_button.setProperty("role", "danger")
        self.cancel_button.setIcon(icon("stop", theming.roles()["error"]))
        self.cancel_button.setAccessibleName("Отменить чтение или остановить выбранный собственный запуск")
        self.cancel_button.clicked.connect(self.cancel_active_work)
        self.cancel_button.setEnabled(False)
        self.cancel_button.hide()
        layout.addWidget(self.cancel_button)
        self.action_finder_button = QPushButton("Действия и переходы")
        self.action_finder_button.setProperty("search_field", True)
        self.action_finder_button.setIcon(icon("search"))
        self.action_finder_button.setFixedWidth(220)
        self.action_finder_button.setFixedHeight(32)
        self.action_finder_button.setLayout(QHBoxLayout())
        self.action_finder_button.layout().setContentsMargins(0, 0, 8, 0)
        self.action_finder_button.layout().addStretch(1)
        self.finder_hint = QLabel("Ctrl+K" if sys.platform != "darwin" else "⌘K")
        self.finder_hint.setProperty("kbd_badge", True)  # a small badge inside the pill: no border, no divider
        self.finder_hint.setAttribute(Qt.WA_TransparentForMouseEvents)
        self.finder_hint.setFixedHeight(20)
        self.action_finder_button.layout().addWidget(self.finder_hint, 0, Qt.AlignVCenter)
        self.action_finder_button.setToolTip("Найти действие · Ctrl/⌘+K")
        self.action_finder_button.setAccessibleName("Найти действие или раздел")
        self.action_finder_button.clicked.connect(self.show_action_finder)
        layout.addWidget(self.action_finder_button)
        self.refresh_button = icon_button("refresh", "Обновить данные проекта", "Перечитать сохранённые данные проекта")
        self.refresh_button.clicked.connect(self.refresh_project)
        self.refresh_button.setEnabled(False)
        layout.addWidget(self.refresh_button)
        self.new_scan = QPushButton("Новый скан")
        self.new_scan.setProperty("role", "primary")
        self.new_scan.setAccessibleName("Открыть план нового скана")
        self.new_scan.setToolTip("Открыть настройки и проверить план; запуск — отдельной кнопкой")
        self.new_scan.setIcon(icon("play_arrow", theming.roles()["on_primary"]))
        self.new_scan.clicked.connect(self.scan_preview)
        layout.addWidget(self.new_scan)
        return top

    def sync_picker(self, combo, button):
        """Mirror the hidden combo (current text, optional subtitle in the tooltip role, enabled) on its button."""
        index = combo.currentIndex()
        if combo is self.scan_picker:
            title, subtitle, full_id = scan_texts(combo, index, self.scan_run_ids(), bool(getattr(button, "_compact", False)))
        else:
            title = tr(combo.itemText(index)) if index >= 0 else ""
            subtitle = combo.itemData(index, Qt.ToolTipRole) if index >= 0 else ""
            full_id = ""
        button.set_texts(title, tr(subtitle) if isinstance(subtitle, str) else "", full_id)
        button.setEnabled(combo.isEnabled() and combo.count() > 0)
        if combo is self.scan_picker and hasattr(self, "scan_state_badge"):
            self.scan_state_badge.setVisible(combo.currentData() is not None)

    def scan_run_ids(self):
        """Saved-scan path -> id of its run, so the top bar and the Scans table show the same «r-xxxx»."""
        from .screens.scan_common import build_rows

        try:
            return {row.scan.get("path"): row.id for row in build_rows(self) if row.scan and row.id}
        except (AttributeError, RuntimeError):  # the models are not built yet during start-up
            return {}

    def show_picker_menu(self, combo, button):
        menu = QMenu(button)
        for index in range(combo.count()):
            if combo is self.scan_picker:
                title, subtitle, _full = scan_texts(combo, index, self.scan_run_ids())  # the list always carries the date
                action = menu.addAction(f"{title}  ·  {subtitle}" if subtitle else title)
            else:
                action = menu.addAction(combo.itemText(index))
            action.setCheckable(index != combo.count() - 1 or combo.itemData(index) != {"action": "open"})
            action.setChecked(index == combo.currentIndex())
            action.triggered.connect(lambda _checked, i=index: self._pick(combo, i))
        if combo is self.project_picker:
            from .screens.project_sources_page import open_project_settings

            menu.addSeparator()
            settings = menu.addAction(icon("settings"), tr("Настройки проекта…"))
            settings.setEnabled(bool(self.project_directory))
            settings.triggered.connect(lambda _checked: open_project_settings(self))
        menu.exec_(button.mapToGlobal(button.rect().bottomLeft()))

    @staticmethod
    def _pick(combo, index):
        combo.setCurrentIndex(index)
        combo.activated.emit(index)
