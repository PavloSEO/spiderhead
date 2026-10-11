"""Application shell widgets of design v2: picker buttons, navigation panel, profile button."""

from __future__ import annotations

import getpass

from PyQt5.QtCore import QRect, QSize, Qt, pyqtSignal
from PyQt5.QtGui import QColor, QFont, QFontMetrics, QIcon, QPainter
from PyQt5.QtWidgets import (
    QFrame,
    QHBoxLayout,
    QLabel,
    QListWidget,
    QListWidgetItem,
    QProgressBar,
    QPushButton,
    QSizePolicy,
    QStyle,
    QStyledItemDelegate,
    QVBoxLayout,
    QWidget,
)

from .. import theming
from ..i18n import joined, tr
from .icons import MaterialIconLabel, material_icon
from .presentation import short_run_id  # noqa: F401  (re-exported: the shared run id format)
from .workspace import VIEW_IDS

# SHELL-CANON §3: one list, the Simple display only hides the agent items.
# id, icon, label, group, shown in the Simple display, the page exists
SECTIONS = (
    ("work", "checklist", "Работа", "top", True, True),
    ("inbox", "inbox", "Входящие", "top", False, True),
    ("crawler", "travel_explore", "Краулер", "top", True, True),
    ("scans", "history", "Сканы", "data", True, True),
    ("url", "table_view", "URL", "data", True, True),
    ("issues", "rule", "Проблемы", "data", True, True),
    ("compare", "compare_arrows", "Сравнение", "data", True, True),
    ("graph", "hub", "Граф ссылок", "data", True, True),
    ("search", "find_in_page", "Поиск в HTML", "data", True, True),
    ("methods", "menu_book", "Методы", "result", False, True),
    ("reports", "description", "Отчёты и задачи", "result", True, True),
    ("log", "terminal", "Журнал", "result", True, True),
)
# Navigation section -> view id used by workspace contexts and the page stack.
VIEW_OF = {"work": "work", "inbox": "inbox", "scans": "scans", "url": "url", "issues": "issues",
           "compare": "compare", "search": "content_search", "reports": "reports", "log": "journal",
           "crawler": "crawler", "methods": "methods", "graph": "graph"}
# SideNav sheet: without a project these items are shown dimmed with «нужен проект»; the rest stay active.
# SHELL-CANON hotkeys: ⌘1 Работа · ⌘2 Сканы · ⌘3 URL · ⌘4 Проблемы; the canon gives no other numbers
NUMBERED_SECTIONS = {1: "work", 2: "scans", 3: "url", 4: "issues"}
NEEDS_PROJECT = frozenset({"work", "inbox", "issues", "compare", "search", "methods", "reports"})
SIMPLE_VIEW_OF = {**VIEW_OF, "work": "tasks"}  # «Работа» opens the simple task list in the Simple display
GROUP_TITLES = {"data": "Данные", "result": "Результат"}
ROLE_ID, ROLE_COUNT, ROLE_DOT, ROLE_HEADER, ROLE_LOCKED = (Qt.UserRole + i for i in range(5))


def elide_words(metrics, text, width):
    """Shorten ``text`` to ``width`` px at a word boundary with «…»; one long word is cut by the font metrics."""
    if metrics.horizontalAdvance(text) <= width:
        return text
    words = text.split(" ")
    while len(words) > 1:
        words.pop()
        candidate = " ".join(words).rstrip(" ·,") + "…"
        if metrics.horizontalAdvance(candidate) <= width:
            return candidate
    return metrics.elidedText(text, Qt.ElideRight, width)


class WordElidedLabel(QLabel):
    """Single-line label that shortens by whole words; asks for its full text width, yields down to a small minimum."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self._full = ""
        self.setTextFormat(Qt.PlainText)
        self.setSizePolicy(QSizePolicy.Preferred, QSizePolicy.Preferred)

    def setText(self, text):
        self._full = str(text)
        self.updateGeometry()
        self._elide()

    def text(self):
        return self._full

    def sizeHint(self):
        size = super().sizeHint()
        size.setWidth(self.fontMetrics().horizontalAdvance(self._full) + 2)
        return size

    def minimumSizeHint(self):
        size = super().minimumSizeHint()
        size.setWidth(48)
        return size

    def _elide(self):
        super().setText(elide_words(self.fontMetrics(), self._full, max(48, self.width() - 2)))

    def resizeEvent(self, event):
        self._elide()
        super().resizeEvent(event)

    def changeEvent(self, event):
        super().changeEvent(event)
        if event.type() in (event.FontChange, event.StyleChange):
            self._elide()


class PickerButton(QPushButton):
    """Two-line picker (title 13/500, subtitle 11) with a leading icon and a chevron; the owner builds the menu."""

    def __init__(self, icon_name, accessible_name, parent=None):
        super().__init__(parent)
        self.setProperty("picker", True)
        self.setAccessibleName(accessible_name)
        self.setFixedHeight(40)
        self.setSizePolicy(QSizePolicy.Preferred, QSizePolicy.Fixed)
        layout = QHBoxLayout(self)
        layout.setContentsMargins(8, 0, 6, 0)
        layout.setSpacing(8)
        self._icon = MaterialIconLabel(icon_name, 20, color="role:text_2")
        layout.addWidget(self._icon)
        texts = QVBoxLayout()
        texts.setContentsMargins(0, 0, 0, 0)
        texts.setSpacing(0)
        self.title = WordElidedLabel()
        self.title.setProperty("picker_part", "title")
        self.subtitle = WordElidedLabel()
        self.subtitle.setProperty("picker_part", "subtitle")
        for label in (self.title, self.subtitle):
            label.setAttribute(Qt.WA_TransparentForMouseEvents)
            texts.addWidget(label)
        layout.addLayout(texts, 1)
        self._chevron = MaterialIconLabel("expand_more", 18, color="role:text_muted")
        layout.addWidget(self._chevron)

    def sizeHint(self):
        return self.layout().sizeHint()  # QPushButton would ignore the inner layout and stay at the minimum width

    def minimumSizeHint(self):
        return self.layout().minimumSize()

    def set_texts(self, title, subtitle="", detail=""):
        self.title.setText(title)
        self.subtitle.setText(subtitle)
        self.subtitle.setVisible(bool(subtitle) and not getattr(self, "_compact", False))
        self.setToolTip(joined("\n", [part for part in (title, subtitle, detail) if part]))

    def set_compact(self, compact):
        self._compact = bool(compact)
        self.subtitle.setVisible(not compact and bool(self.subtitle.text()))


class _NavDelegate(QStyledItemDelegate):
    def __init__(self, view):
        super().__init__(view)
        self.view = view

    def sizeHint(self, option, index):
        if index.data(ROLE_HEADER):
            return QSize(option.rect.width(), 0 if self.view.rail else 28)
        return QSize(option.rect.width(), 44 if self.view.rail else 36)

    def paint(self, painter, option, index):
        r = theming.roles()
        painter.save()
        painter.setRenderHint(QPainter.Antialiasing)
        rect = option.rect
        if index.data(ROLE_HEADER):
            if not self.view.rail:
                font = QFont(painter.font())
                font.setPixelSize(11)
                font.setWeight(QFont.Medium)
                font.setLetterSpacing(QFont.AbsoluteSpacing, 0.4)
                painter.setFont(font)
                painter.setPen(QColor(r["text_3"]))
                painter.drawText(rect.adjusted(12, 6, 0, 0), Qt.AlignLeft | Qt.AlignTop, index.data(Qt.DisplayRole).upper())
            painter.restore()
            return
        locked = bool(index.data(ROLE_LOCKED))
        if locked:
            painter.setOpacity(0.5)
        selected = bool(option.state & QStyle.State_Selected)  # a locked row still shows the open section; only hover is suppressed
        hovered = bool(option.state & QStyle.State_MouseOver) and not locked
        ink = QColor(r["on_selected"] if selected else r["text_2"])
        font = QFont(painter.font())
        font.setWeight(QFont.Medium)
        if self.view.rail:
            pill = QRect(rect.center().x() - 22, rect.top() + 8, 44, 28)  # icon only: the full label is the tooltip
            if selected or hovered:
                painter.setPen(Qt.NoPen)
                painter.setBrush(QColor(r["selected"] if selected else r["nav_hover"]))
                painter.drawRoundedRect(pill, 14, 14)
            index.data(Qt.DecorationRole).paint(painter, QRect(pill.center().x() - 10, pill.center().y() - 10, 20, 20), Qt.AlignCenter,
                                                 QIcon.Selected if selected else QIcon.Normal)
        else:
            body = rect.adjusted(0, 0, 0, 0)
            if selected or hovered:
                painter.setPen(Qt.NoPen)
                painter.setBrush(QColor(r["selected"] if selected else r["nav_hover"]))
                painter.drawRoundedRect(body, 8, 8)
            index.data(Qt.DecorationRole).paint(painter, QRect(body.left() + 12, body.center().y() - 10, 20, 20), Qt.AlignCenter,
                                                 QIcon.Selected if selected else QIcon.Normal)
            font.setPixelSize(13)
            painter.setFont(font)
            painter.setPen(ink)
            count = index.data(ROLE_COUNT)
            hint = tr("нужен проект") if locked else ""
            label_width = QFontMetrics(font).horizontalAdvance(index.data(Qt.DisplayRole))
            small = QFont(font)
            small.setPixelSize(10)
            hint_width = QFontMetrics(small).horizontalAdvance(hint) if hint else 0
            hint = hint if hint and 44 + label_width + 8 + hint_width + 12 <= body.width() else ""  # no room: the tooltip says it
            painter.drawText(body.adjusted(44, 0, -(hint_width + 16) if hint else -48 if count else -8, 0), Qt.AlignLeft | Qt.AlignVCenter,
                             QFontMetrics(font).elidedText(index.data(Qt.DisplayRole), Qt.ElideRight, body.width() - 44 - ((hint_width + 16) if hint else 48 if count else 8)))
            if hint:
                painter.setFont(small)
                painter.setPen(QColor(r["text_3"]))
                painter.drawText(body.adjusted(0, 0, -12, 0), Qt.AlignRight | Qt.AlignVCenter, hint)
            elif count:
                font.setPixelSize(11)
                painter.setFont(font)
                if index.data(ROLE_DOT):
                    badge = QRect(body.right() - 12 - 18, body.center().y() - 9, 18, 18)
                    painter.setPen(Qt.NoPen)
                    painter.setBrush(QColor(r["primary"]))
                    painter.drawRoundedRect(badge, 9, 9)
                    painter.setPen(QColor(r["on_primary"]))
                    painter.drawText(badge, Qt.AlignCenter, str(count))
                else:
                    painter.setPen(QColor(r["on_selected"] if selected else r["text_3"]))
                    painter.drawText(body.adjusted(0, 0, -12, 0), Qt.AlignRight | Qt.AlignVCenter, str(count))
        if option.state & QStyle.State_HasFocus:
            painter.setPen(QColor(r["ring"]))
            painter.setBrush(Qt.NoBrush)
            painter.drawRoundedRect(rect.adjusted(1, 1, -1, -1), 8, 8)
        painter.restore()


class NavList(QListWidget):
    """Sections with group headers; rows keep native keyboard navigation and accessibility."""

    lockedClicked = pyqtSignal(str)

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setObjectName("navView")
        self.rail = False
        self.setFrameShape(QFrame.NoFrame)
        self.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        self.setMouseTracking(True)
        self.setItemDelegate(_NavDelegate(self))
        self.setAccessibleName("Разделы проекта")
        self.setUniformItemSizes(False)
        self.setSelectionMode(QListWidget.SingleSelection)
        self.setFocusPolicy(Qt.StrongFocus)
        self._items = {}
        self.project_open = True

    def mousePressEvent(self, event):
        item = self.itemAt(event.pos())
        if item is not None and item.data(ROLE_LOCKED):  # dimmed «нужен проект»: a click leads to the project start, not to a page
            self.lockedClicked.emit(item.data(ROLE_ID))
            return
        super().mousePressEvent(event)

    def set_project_open(self, opened):
        self.project_open = bool(opened)
        self.setToolTip("" if self.project_open else tr("Разделы с пометкой «нужен проект» откроются после открытия или создания проекта"))
        for section_id, item in self._items.items():
            locked = section_id in NEEDS_PROJECT and not self.project_open
            item.setData(ROLE_LOCKED, locked)
            label = next(row[2] for row in SECTIONS if row[0] == section_id)
            item.setToolTip(tr("Нужен проект · «Сохранить как проект»") if locked else tr(label))
        self.viewport().update()

    def rebuild(self, simple, current):
        """Rows for the active display; selection is restored by section id."""
        self.blockSignals(True)
        self.clear()
        self._items = {}
        last_group = None
        for section_id, icon_name, label, group, in_simple, ready in SECTIONS:
            if not ready or (simple and not in_simple):
                continue
            if group in GROUP_TITLES and group != last_group:
                header = QListWidgetItem(tr(GROUP_TITLES[group]))
                header.setData(ROLE_HEADER, True)
                header.setFlags(Qt.NoItemFlags)
                self.addItem(header)
            last_group = group
            item = QListWidgetItem(tr(label))
            item.setData(ROLE_ID, section_id)
            item.setIcon(material_icon(icon_name))
            item.setData(Qt.AccessibleTextRole, tr(label))
            item.setToolTip(tr(label))
            self.addItem(item)
            self._items[section_id] = item
        self.blockSignals(False)
        self.set_project_open(self.project_open)
        if current in self._items:
            self.set_current(current, emit=False)

    def set_rail(self, rail):
        self.rail = bool(rail)
        self.setProperty("compact", self.rail)
        self.scheduleDelayedItemsLayout()
        self.viewport().update()

    def set_count(self, section_id, count, dot=False):
        item = self._items.get(section_id)
        if item is not None:
            item.setData(ROLE_COUNT, count or "")
            item.setData(ROLE_DOT, bool(dot))
            self.viewport().update()

    def current_section(self):
        item = self.currentItem()
        return item.data(ROLE_ID) if item is not None else None

    def set_current(self, section_id, emit=True):
        item = self._items.get(section_id)
        if item is None:
            return False
        if not emit:
            self.blockSignals(True)
        self.setCurrentItem(item)
        self.blockSignals(False)
        return True


class ProfileButton(QPushButton):
    """Avatar + name + display line + settings icon; opens the profile menu."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setProperty("profile", True)
        self.setFixedHeight(44)
        self.setAccessibleName("Профиль, отображение и настройки")
        user = getpass.getuser() or "user"
        layout = QHBoxLayout(self)
        layout.setContentsMargins(8, 0, 8, 0)
        layout.setSpacing(10)
        self.avatar = QLabel(user[:2].upper())
        self.avatar.setProperty("avatar", True)
        self.avatar.setFixedSize(28, 28)
        self.avatar.setAlignment(Qt.AlignCenter)
        self.avatar.setAttribute(Qt.WA_TransparentForMouseEvents)
        layout.addWidget(self.avatar)
        texts = QVBoxLayout()
        texts.setContentsMargins(0, 0, 0, 0)
        texts.setSpacing(0)
        self.name = QLabel(user.capitalize())
        self.name.setProperty("picker_part", "title")
        self.mode_line = WordElidedLabel()
        self.mode_line.setProperty("picker_part", "subtitle")
        for label in (self.name, self.mode_line):
            label.setAttribute(Qt.WA_TransparentForMouseEvents)
            texts.addWidget(label)
        self._texts = QWidget()
        self._texts.setLayout(texts)
        self._texts.setAttribute(Qt.WA_TransparentForMouseEvents)
        layout.addWidget(self._texts, 1)
        self._gear = MaterialIconLabel("settings", 18, color="role:text_2")
        layout.addWidget(self._gear)

    def set_mode_line(self, text):
        self.mode_line.setText(text)
        self.setToolTip(text)  # the subtitle elides on narrow widths; the full line stays one hover away

    def set_rail(self, rail):
        self._texts.setVisible(not rail)
        self._gear.setVisible(not rail)


class ActiveScanCard(QFrame):
    openRequested = pyqtSignal()

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setProperty("card", "panel")
        layout = QVBoxLayout(self)
        layout.setContentsMargins(10, 10, 10, 10)
        layout.setSpacing(8)
        head = QHBoxLayout()
        head.setSpacing(6)
        head.addWidget(MaterialIconLabel("sensors", 16, color="role:primary"))
        title = QLabel("Активный скан")
        title.setProperty("text_style", "control")
        head.addWidget(title, 1)
        layout.addLayout(head)
        self.text = QLabel()
        self.text.setProperty("text_style", "meta")
        self.text.setWordWrap(True)
        layout.addWidget(self.text)
        self.bar = QProgressBar()
        self.bar.setTextVisible(False)
        layout.addWidget(self.bar)
        self.active = False
        link = QPushButton("Открыть наблюдение")
        link.setProperty("role", "text")
        link.clicked.connect(self.openRequested)
        layout.addWidget(link, 0, Qt.AlignLeft)
        self.hide()

    def show_progress(self, text, done=None, total=None, stale=False):
        """total None -> progress is unknown (indeterminate bar); never fake a percentage. A stale observation never animates."""
        self.text.setText(tr(text))
        self.active = True
        if total:
            self.bar.setRange(0, int(total))
            self.bar.setValue(int(done or 0))
        elif stale:
            self.bar.setRange(0, 1)
            self.bar.setValue(0)
        else:
            self.bar.setRange(0, 0)
        self.setVisible(not self.parentWidget() or not self.parentWidget().property("compact"))

    def clear(self):
        self.active = False
        self.hide()


class NavPanel(QFrame):
    """Left column: sections, active-scan card and the profile button. ``current_section`` follows the list."""

    sectionChanged = pyqtSignal(str)
    currentRowChanged = pyqtSignal(int)  # index into VIEW_IDS (page/workspace identity), kept for existing callers
    profileClicked = pyqtSignal()
    openScanRequested = pyqtSignal()
    lockedClicked = pyqtSignal(str)

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setObjectName("navPanel")
        self.setProperty("compact", False)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(8, 10, 8, 8)
        layout.setSpacing(8)
        self.list = NavList()
        self.list.currentItemChanged.connect(self._item_changed)
        self.list.lockedClicked.connect(self.lockedClicked)
        layout.addWidget(self.list, 1)
        self.card = ActiveScanCard()
        self.card.openRequested.connect(self.openScanRequested)
        layout.addWidget(self.card)
        self.profile = ProfileButton()
        self.profile.clicked.connect(self.profileClicked)
        layout.addWidget(self.profile)
        self.simple = False
        self.list.rebuild(False, None)

    def _item_changed(self, current, _previous):
        if current is not None and current.data(ROLE_ID):
            self.sectionChanged.emit(current.data(ROLE_ID))
            self.currentRowChanged.emit(self.currentRow())

    def views(self):
        return SIMPLE_VIEW_OF if self.simple else VIEW_OF

    def currentRow(self):
        view = self.views().get(self.current_section())
        return VIEW_IDS.index(view) if view else -1

    def setCurrentRow(self, row):
        view = VIEW_IDS[row] if 0 <= row < len(VIEW_IDS) else None
        section = next((s for s, v in self.views().items() if v == view and self.has_section(s)), None)
        return self.select_section(section) if section else False

    def set_display(self, simple):
        current = self.list.current_section()
        self.simple = bool(simple)
        self.list.rebuild(self.simple, current)
        self.profile.set_mode_line(tr("Простой режим · MCP выкл." if self.simple else "С агентом · MCP вкл."))
        if current in self.list._items:
            self.currentRowChanged.emit(self.currentRow())

    def set_rail(self, rail):
        self.setProperty("compact", bool(rail))
        self.list.set_rail(rail)
        self.layout().setContentsMargins(4 if rail else 8, 10, 4 if rail else 8, 8)
        self.profile.set_rail(rail)
        self.card.setVisible(self.card.active and not rail)
        self.style().unpolish(self)
        self.style().polish(self)

    # Section API
    def current_section(self):
        return self.list.current_section()

    def select_section(self, section_id, emit=True):
        return self.list.set_current(section_id, emit)

    def select_number(self, number):
        """Ctrl+1…4 (SHELL-CANON): fixed sections, not positions; a hidden or unassigned number does nothing."""
        section = NUMBERED_SECTIONS.get(number)
        return bool(section) and self.has_section(section) and self.select_section(section)

    def has_section(self, section_id):
        return section_id in self.list._items

    def set_count(self, section_id, count, dot=False):
        self.list.set_count(section_id, count, dot)

    def set_project_open(self, opened):
        self.list.set_project_open(opened)
