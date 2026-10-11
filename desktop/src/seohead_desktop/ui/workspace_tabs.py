"""Bounded native workspace tabs containing immutable view contexts only.

Controllers, gateways and scan processes belong to the hosting window. A close
button emits an intent; removing its descriptor never stops a scan or closes the
window. The host decides what to show after the final context is removed.
"""

from __future__ import annotations

import math
from collections.abc import Mapping
from dataclasses import dataclass, field, replace
from types import MappingProxyType
from uuid import uuid4

from PyQt5.QtCore import QByteArray, QPointF, QSize, Qt, pyqtSignal
from PyQt5.QtGui import QKeySequence, QMouseEvent
from PyQt5.QtWidgets import (
    QHBoxLayout,
    QInputDialog,
    QMenu,
    QShortcut,
    QSizePolicy,
    QTabBar,
    QToolButton,
    QWidget,
)

from .. import theming
from ..i18n import signals as i18n_signals
from ..i18n import tr, trf
from .icons import material_icon

MAX_WORKSPACE_TABS = 12


def _freeze(value, depth=0):
    if depth > 12:
        raise ValueError("Workspace state nesting is too deep")
    if value is None or type(value) in {str, int, bool, bytes}:
        return value
    if type(value) is float:
        if not math.isfinite(value):
            raise ValueError("Workspace state numbers must be finite")
        return value
    if isinstance(value, (bytearray, QByteArray)):
        return bytes(value)
    if isinstance(value, Mapping):
        if any(type(key) is not str for key in value):
            raise TypeError("Workspace state keys must be strings")
        return MappingProxyType({key: _freeze(item, depth + 1) for key, item in value.items()})
    if isinstance(value, (tuple, list)):
        return tuple(_freeze(item, depth + 1) for item in value)
    raise TypeError("Workspace state accepts data only, never controllers or widgets")


def _thaw(value):
    if isinstance(value, Mapping):
        return {key: _thaw(item) for key, item in value.items()}
    if isinstance(value, tuple):
        return [_thaw(item) for item in value]
    return value


@dataclass(frozen=True)
class WorkspaceContext:
    """Identity and a deep-frozen snapshot; state keys remain owned by the host.

    Typical state keys are search, filter, offset, selected_url, selection and
    splitter bytes. No page records or live service objects are needed here.
    state_dict() returns an independent mutable copy for capture/restore code.
    """

    id: str = field(default_factory=lambda: uuid4().hex)
    project_uuid: str | None = None
    project_root: str | None = None
    project_label: str = "Новая вкладка"
    scan_uuid: str | None = None
    view_id: str = "work"
    state: Mapping[str, object] = field(default_factory=dict, hash=False)
    display_alias: str | None = None
    pinned: bool = False

    def __post_init__(self):
        for name in ("id", "project_label", "view_id"):
            value = getattr(self, name)
            if not isinstance(value, str) or not value.strip():
                raise ValueError(f"Workspace {name} must be a non-empty string")
        for name in ("project_uuid", "project_root", "scan_uuid"):
            value = getattr(self, name)
            if value is not None and not isinstance(value, str):
                raise TypeError(f"Workspace {name} must be a string or None")
        if self.display_alias is not None and (
            not isinstance(self.display_alias, str) or not self.display_alias.strip()
        ):
            raise ValueError("Workspace display alias must be a non-empty string or None")
        if type(self.pinned) is not bool:
            raise TypeError("Workspace pinned flag must be a boolean")
        if not isinstance(self.state, Mapping):
            raise TypeError("Workspace state must be a mapping")
        object.__setattr__(self, "state", _freeze(self.state))

    def state_dict(self) -> dict[str, object]:
        return _thaw(self.state)


class _WorkspaceTabBar(QTabBar):
    def __init__(self, parent):
        super().__init__(parent)
        self._drag_id = None
        self._drag_offset = 0

    def minimumSizeHint(self):
        size = super().minimumSizeHint()
        # Qt's scroll-button minimum can exceed the width of one short tab.
        size.setWidth(max(0, min(size.width(), self.sizeHint().width())))
        return size

    def mousePressEvent(self, event):
        index = self.tabAt(event.pos())
        self._drag_id = (
            self.tabData(index) if event.button() == Qt.LeftButton and index >= 0 else None
        )
        if self._drag_id is not None:
            self._drag_offset = event.x() - self.tabRect(index).center().x()
        super().mousePressEvent(event)

    def mouseMoveEvent(self, event):
        contexts = self.parentWidget()._contexts
        context = contexts.get(self._drag_id)
        if context is not None and event.buttons() & Qt.LeftButton:
            group = [
                index
                for index in range(self.count())
                if contexts[self.tabData(index)].pinned == context.pinned
            ]
            first = self.tabRect(group[0]).center().x() + self._drag_offset
            last = self.tabRect(group[-1]).center().x() + self._drag_offset
            x = max(first, min(event.x(), last))
            if x != event.x():
                # Clamping after tabMoved confuses Qt's private dragged-tab index.
                event = QMouseEvent(
                    event.type(),
                    QPointF(x, event.y()),
                    event.screenPos(),
                    event.button(),
                    event.buttons(),
                    event.modifiers(),
                )
        super().mouseMoveEvent(event)

    def mouseReleaseEvent(self, event):
        super().mouseReleaseEvent(event)
        self._drag_id = None


class _CloseButton(QToolButton):
    """Tab close glyph drawn from the active theme (the style's default glyph ignores QSS colours)."""

    def __init__(self, owner):
        super().__init__()
        self._owner = owner
        self.setProperty("tab_close", True)
        self.setFocusPolicy(Qt.NoFocus)
        self.setIcon(material_icon("close", "role:text_muted"))
        self.setIconSize(QSize(16, 16))
        self.clicked.connect(self._close)
        theming.signals.changed.connect(self._theme_changed)

    def _theme_changed(self, _name):
        self.setIcon(material_icon("close", "role:text_muted"))

    def _close(self):
        bar = self._owner.tabbar
        for index in range(bar.count()):
            if bar.tabButton(index, QTabBar.RightSide) is self:
                bar.tabCloseRequested.emit(index)
                return


class WorkspaceTabs(QWidget):
    """A native strip; selected emits only when the active context ID changes."""

    selected = pyqtSignal(str)
    closeRequested = pyqtSignal(str)
    newRequested = pyqtSignal()
    duplicateRequested = pyqtSignal(str)
    settingsRequested = pyqtSignal()

    def __init__(self, parent=None, *, max_tabs=MAX_WORKSPACE_TABS):
        super().__init__(parent)
        if type(max_tabs) is not int or not 1 <= max_tabs <= MAX_WORKSPACE_TABS:
            raise ValueError(f"Workspace tab limit must be between 1 and {MAX_WORKSPACE_TABS}")
        self.max_tabs = max_tabs
        self._contexts = {}
        self._titles = {}
        self._icons = {}
        self._custom_titles = set()
        self._active_id = None
        self._shortcuts = []
        self._shortcut_owner = None
        self.setObjectName("tabstrip")
        self.setFixedHeight(38)
        self.setAttribute(Qt.WA_StyledBackground, True)
        self.setAccessibleName("Рабочие вкладки SEOHEAD")
        layout = QHBoxLayout(self)
        layout.setContentsMargins(8, 2, 8, 0)
        layout.setSpacing(0)
        layout.setAlignment(Qt.AlignBottom)
        self.tabbar = _WorkspaceTabBar(self)
        self.tabbar.setProperty("tabs", "workspace")
        self.tabbar.setFixedHeight(34)
        self.tabbar.setObjectName("workspaceTabBar")
        self.tabbar.setAccessibleName("Проекты, сканы и представления")
        self.tabbar.setUsesScrollButtons(True)
        self.tabbar.setExpanding(False)
        self.tabbar.setMovable(True)
        self.tabbar.setTabsClosable(True)
        self.tabbar.setElideMode(
            Qt.ElideNone
        )  # full names at 800 px: the strip scrolls instead of cutting them
        self.tabbar.setSelectionBehaviorOnRemove(QTabBar.SelectLeftTab)
        self.tabbar.setDrawBase(False)
        self.tabbar.setSizePolicy(QSizePolicy.Preferred, QSizePolicy.Fixed)
        self.tabbar.setContextMenuPolicy(Qt.CustomContextMenu)
        layout.addWidget(self.tabbar)
        self.new_button = QToolButton()
        self.new_button.setObjectName("workspaceNewTab")
        self.new_button.setIcon(material_icon("add"))
        self.new_button.setFixedSize(28, 28)
        self.new_button.setContentsMargins(0, 0, 0, 0)
        self.new_button.setAccessibleName("Новая рабочая вкладка")
        self.new_button.setToolTip("Новая рабочая вкладка")
        self.new_button.clicked.connect(self.request_new)
        layout.addWidget(self.new_button)
        layout.addStretch(1)
        self.overflow_button = QToolButton()
        self.overflow_button.setObjectName("workspaceOverflow")
        self.overflow_button.setIcon(material_icon("tune"))
        self.overflow_button.setFixedSize(28, 28)
        self.overflow_button.setAccessibleName("Настроить вкладки и панели")
        self.overflow_button.setToolTip("Настроить вкладки и панели")
        self.overflow_button.clicked.connect(self.settingsRequested)
        self.menu = QMenu(self.overflow_button)
        self.overflow_button.setContextMenuPolicy(Qt.CustomContextMenu)
        self.overflow_button.customContextMenuRequested.connect(self._show_workspace_list)
        self.menu.aboutToShow.connect(self._rebuild_menu)
        layout.addWidget(self.overflow_button)
        self.tabbar.currentChanged.connect(self._sync_selected)
        self.tabbar.tabCloseRequested.connect(self._close_index)
        self.tabbar.tabMoved.connect(self._tab_moved)
        self.context_menu = QMenu(self.tabbar)
        self.tabbar.customContextMenuRequested.connect(self._show_context_menu)
        self.tabbar.tabBarDoubleClicked.connect(self._rename_index)
        self._update_capacity()
        self._rebuild_menu()
        i18n_signals.changed.connect(self._language_changed)

    def _language_changed(self, _language=None):
        try:
            for id in self._contexts:
                self._refresh_tab(id)
            self._update_capacity()  # the overflow menu is rebuilt every time it opens
        except RuntimeError:  # deleted with its window
            pass

    @property
    def current_id(self) -> str | None:
        index = self.tabbar.currentIndex()
        return self.tabbar.tabData(index) if index >= 0 else None

    def contexts(self) -> tuple[WorkspaceContext, ...]:
        return tuple(
            self._contexts[self.tabbar.tabData(index)] for index in range(self.tabbar.count())
        )

    def _index(self, id):
        return next(
            (index for index in range(self.tabbar.count()) if self.tabbar.tabData(index) == id),
            -1,
        )

    def add(
        self,
        context: WorkspaceContext,
        title=None,
        icon=None,
        select=True,
        *,
        after_id=None,
    ):
        if not isinstance(context, WorkspaceContext):
            raise TypeError("add requires a WorkspaceContext")
        if context.id in self._contexts:
            raise ValueError("Workspace context ID already exists")
        if len(self._contexts) >= self.max_tabs:
            raise ValueError(f"At most {self.max_tabs} workspaces may be open")
        if after_id is not None and after_id not in self._contexts:
            raise KeyError(after_id)
        label = context.project_label if title is None else self._checked_title(title)
        anchor = self.current_id if after_id is None else after_id
        index = self._index(anchor) + 1
        pinned_count = sum(item.pinned for item in self._contexts.values())
        index = min(index, pinned_count) if context.pinned else max(index, pinned_count)
        self._contexts[context.id] = context
        self._titles[context.id] = label
        self._icons[context.id] = icon or material_icon("folder_open")
        if title is not None:
            self._custom_titles.add(context.id)
        blocked = self.tabbar.blockSignals(True)
        index = self.tabbar.insertTab(index, self._icons[context.id], label)
        self.tabbar.setTabData(index, context.id)
        self._refresh_tab(context.id)
        if select:
            self.tabbar.setCurrentIndex(index)
        self.tabbar.blockSignals(blocked)
        self._update_capacity()
        self._sync_selected()
        return context.id

    def remove(self, id) -> WorkspaceContext | None:
        context = self._contexts.get(id)
        if context is None:
            return None
        blocked = self.tabbar.blockSignals(True)
        self.tabbar.removeTab(self._index(id))
        self._contexts.pop(id)
        self._titles.pop(id)
        self._icons.pop(id)
        self._custom_titles.discard(id)
        self.tabbar.blockSignals(blocked)
        self._update_capacity()
        self._sync_selected()
        return context

    def select(self, id):
        if id not in self._contexts:
            raise KeyError(id)
        self.tabbar.setCurrentIndex(self._index(id))
        self._sync_selected()

    def update_context(self, context_id, **fields) -> WorkspaceContext:
        if "id" in fields:
            raise ValueError("Workspace identity cannot be changed")
        updated = replace(self._contexts[context_id], **fields)
        was_pinned = self._contexts[context_id].pinned
        self._contexts[context_id] = updated
        if "project_label" in fields and context_id not in self._custom_titles:
            self._titles[context_id] = updated.project_label
        if was_pinned != updated.pinned:
            pinned_count = sum(item.pinned for item in self._contexts.values())
            destination = pinned_count - 1 if updated.pinned else pinned_count
            blocked = self.tabbar.blockSignals(True)
            self.tabbar.moveTab(self._index(context_id), destination)
            self.tabbar.blockSignals(blocked)
        self._refresh_tab(context_id)
        return updated

    def update_title(self, id, title, icon=None):
        if id not in self._contexts:
            raise KeyError(id)
        title = self._checked_title(title)
        self._custom_titles.add(id)
        self._titles[id] = title
        if icon is not None:
            self._icons[id] = icon
        self._refresh_tab(id)

    def set_alias(self, id, alias: str | None) -> WorkspaceContext:
        if alias is not None and not isinstance(alias, str):
            raise TypeError("Workspace display alias must be a string or None")
        return self.update_context(id, display_alias=alias.strip() or None if alias else None)

    def set_pinned(self, id, pinned: bool) -> WorkspaceContext:
        return self.update_context(id, pinned=pinned)

    def update_state(self, id, state) -> WorkspaceContext:
        return self.update_context(id, state=state)

    def _refresh_tab(self, id):
        context = self._contexts[id]
        index = self._index(id)
        override = getattr(self, "_screen_presentation", None)
        active_override = override if override and override[0] == id else None
        title = (
            active_override[1] if active_override else context.display_alias or tr(self._titles[id])
        )
        self.tabbar.setTabText(index, title)
        tooltip = trf("Закреплена · {title}", title=title) if context.pinned else title
        if context.display_alias and context.display_alias != self._titles[id]:
            tooltip += "\n" + tr(self._titles[id])
        self.tabbar.setTabToolTip(index, tooltip)
        pin_icon = material_icon("push_pin") if context.pinned else None
        self.tabbar.setTabIcon(
            index,
            pin_icon
            if pin_icon is not None and not pin_icon.isNull()
            else material_icon(active_override[2])
            if active_override
            else self._icons[id],
        )
        if self.tabbar.tabsClosable() and not isinstance(
            self.tabbar.tabButton(index, QTabBar.RightSide), _CloseButton
        ):
            self.tabbar.setTabButton(index, QTabBar.RightSide, _CloseButton(self))

        for side in (QTabBar.LeftSide, QTabBar.RightSide):
            button = self.tabbar.tabButton(index, side)
            if button is not None:
                button.setAccessibleName(trf("Закрыть вкладку {title}", title=title))
                button.setToolTip(trf("Закрыть вкладку {title}", title=title))
                button.setEnabled(not context.pinned)
                button.setVisible(not context.pinned)

    def set_screen_presentation(self, title=None, icon=None):
        """Temporary screen caption; stored aliases, project and view identity remain intact."""
        old = getattr(self, "_screen_presentation", None)
        new = (self.current_id, title, icon) if title and self.current_id else None
        if old == new:
            return
        self._screen_presentation = new
        for id in {item[0] for item in (old, new) if item}:
            if id in self._contexts:
                self._refresh_tab(id)

    @staticmethod
    def _checked_title(title):
        if not isinstance(title, str) or not title.strip():
            raise ValueError("Workspace title must be a non-empty string")
        return title

    def request_new(self):
        if len(self._contexts) < self.max_tabs:
            self.newRequested.emit()

    def request_close(self, id=None, *, explicit=False):
        id = self.current_id if id is None else id
        if id in self._contexts and (explicit or not self._contexts[id].pinned):
            self.closeRequested.emit(id)

    def request_rename(self, id=None):
        id = self.current_id if id is None else id
        if id not in self._contexts:
            return
        context = self._contexts[id]
        dialog = QInputDialog(self)
        dialog.setWindowTitle(tr("Переименовать вкладку"))
        dialog.setLabelText(tr("Название (пустое — по проекту):"))
        dialog.setTextValue(context.display_alias or tr(self._titles[id]))
        dialog.setOkButtonText(tr("Сохранить"))
        dialog.setCancelButtonText(tr("Отмена"))
        accepted = dialog.exec_()
        title = dialog.textValue()
        dialog.deleteLater()
        if accepted and id in self._contexts:
            self.set_alias(id, title)

    def toggle_pinned(self, id=None):
        id = self.current_id if id is None else id
        if id in self._contexts:
            self.set_pinned(id, not self._contexts[id].pinned)

    def request_duplicate(self, id=None):
        id = self.current_id if id is None else id
        if id in self._contexts and len(self._contexts) < self.max_tabs:
            self.duplicateRequested.emit(id)

    def select_next(self):
        if self.tabbar.count():
            self.tabbar.setCurrentIndex((self.tabbar.currentIndex() + 1) % self.tabbar.count())

    def select_previous(self):
        if self.tabbar.count():
            self.tabbar.setCurrentIndex((self.tabbar.currentIndex() - 1) % self.tabbar.count())

    def _sync_selected(self, *_args):
        current = self.current_id
        if current == self._active_id:
            return
        self._active_id = current
        if current is not None:
            self.selected.emit(current)

    def _close_index(self, index):
        if 0 <= index < self.tabbar.count():
            self.request_close(self.tabbar.tabData(index))

    def _tab_moved(self, _from, destination):
        context = self._contexts[self.tabbar.tabData(destination)]
        pinned_count = sum(item.pinned for item in self._contexts.values())
        boundary = (
            min(destination, pinned_count - 1) if context.pinned else max(destination, pinned_count)
        )
        if boundary != destination:
            blocked = self.tabbar.blockSignals(True)
            self.tabbar.moveTab(destination, boundary)
            self.tabbar.blockSignals(blocked)
        self._sync_selected()

    def _rename_index(self, index):
        if 0 <= index < self.tabbar.count():
            self.request_rename(self.tabbar.tabData(index))

    def _show_workspace_list(self, position):
        self._rebuild_menu()
        self.menu.popup(self.overflow_button.mapToGlobal(position))

    def _show_context_menu(self, position):
        index = self.tabbar.tabAt(position)
        if index < 0:
            return
        self.context_menu.clear()
        self._add_context_actions(self.context_menu, self.tabbar.tabData(index))
        self.context_menu.popup(self.tabbar.mapToGlobal(position))

    def _update_capacity(self):
        available = len(self._contexts) < self.max_tabs
        self.new_button.setEnabled(available)
        self.new_button.setToolTip(
            tr("Новая рабочая вкладка")
            if available
            else trf("Открыто максимум вкладок: {max}", max=self.max_tabs)
        )

    def _rebuild_menu(self):
        self.menu.clear()
        create = self.menu.addAction(material_icon("add_circle"), tr("Новая вкладка"))
        create.setEnabled(len(self._contexts) < self.max_tabs)
        create.triggered.connect(self.request_new)
        self._add_context_actions(self.menu, self.current_id)
        self.menu.addSeparator()
        for context in self.contexts():
            action = self.menu.addAction(
                self.tabbar.tabIcon(self._index(context.id)),
                context.display_alias or tr(self._titles[context.id]),
            )
            action.setData(context.id)
            action.setCheckable(True)
            action.setChecked(context.id == self.current_id)
            action.triggered.connect(
                lambda checked=False, id=context.id: (
                    self.select(id) if id in self._contexts else None
                )
            )

    def _add_context_actions(self, menu, id):
        context = self._contexts.get(id)
        rename = menu.addAction(material_icon("edit_note"), tr("Переименовать вкладку…"))
        rename.setEnabled(context is not None)
        rename.triggered.connect(lambda: self.request_rename(id))
        pin = menu.addAction(
            tr("Открепить вкладку" if context and context.pinned else "Закрепить вкладку")
        )
        pin.setEnabled(context is not None)
        pin.triggered.connect(lambda: self.toggle_pinned(id))
        duplicate = menu.addAction(material_icon("content_copy"), tr("Дублировать вкладку"))
        duplicate.setEnabled(context is not None and len(self._contexts) < self.max_tabs)
        duplicate.triggered.connect(lambda: self.request_duplicate(id))
        close = menu.addAction(
            material_icon("close"),
            tr("Закрыть закреплённую вкладку" if context and context.pinned else "Закрыть вкладку"),
        )
        close.setEnabled(context is not None)
        close.triggered.connect(lambda: self.request_close(id, explicit=True))

    def install_shortcuts(self, owner):
        """Opt in once per window; never install duplicate bindings in the host."""
        if self._shortcut_owner is owner:
            return
        if self._shortcut_owner is not None:
            raise ValueError("Workspace shortcuts are already attached to another window")
        if not isinstance(owner, QWidget):
            raise TypeError("Shortcut owner must be a QWidget")
        self._shortcut_owner = owner
        bindings = (
            (QKeySequence.AddTab, self.request_new, False),
            (QKeySequence.Close, lambda: self.request_close(), False),
            (QKeySequence.NextChild, self.select_next, True),
            (QKeySequence.PreviousChild, self.select_previous, True),
        )
        seen = set()
        for standard, callback, tab_only in bindings:
            for sequence in QKeySequence.keyBindings(standard):
                text = sequence.toString(QKeySequence.PortableText)
                if text in seen or (tab_only and text.split("+")[-1] not in {"Tab", "Backtab"}):
                    continue
                seen.add(text)
                shortcut = QShortcut(sequence, owner)
                shortcut.setContext(Qt.WindowShortcut)
                shortcut.activated.connect(callback)
                self.destroyed.connect(shortcut.deleteLater)
                self._shortcuts.append(shortcut)
