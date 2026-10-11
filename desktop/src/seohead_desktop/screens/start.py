"""Screen «Старт · проекты» (sheet Start.dc.html): recent projects, open a folder, create a project.

Data come only from what the window keeps (host.recent_projects) and from local files read by a worker
(does the folder still exist, the site host in project.json). Nothing here scans, calls the network or the core
except the explicit «Создать» of the new-project dialog (core command ``project-new``).
"""

from __future__ import annotations

import html
import json
import os
import zlib
from datetime import datetime
from pathlib import Path

from PyQt5.QtCore import (
    QAbstractListModel,
    QObject,
    QRect,
    QRectF,
    QRunnable,
    QSize,
    QSortFilterProxyModel,
    Qt,
    pyqtSignal,
)
from PyQt5.QtGui import QColor, QFont, QPainter
from PyQt5.QtWidgets import (
    QDialog,
    QFileDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QListView,
    QMenu,
    QPushButton,
    QScrollArea,
    QStyle,
    QStyledItemDelegate,
    QVBoxLayout,
    QWidget,
)

from .. import i18n, theming
from ..project_create import ProjectCreator, normalize_target, suggest_directory, validate
from ..ui.controls import Note, Switch, polish
from ..ui.icons import material_icon
from ..ui.kit import UNAVAILABLE, StatePanel
from .base import Screen

tr, trf = i18n.tr, i18n.trf

ROW_HEIGHT = 64
ROW_ROLE = Qt.UserRole + 1
SEARCH_ROLE = Qt.UserRole + 2
AVATAR_KINDS = ("info", "ok", "warn", "goal", "mut")
BANNER_LIMIT = 3


class _Signals(QObject):
    done = pyqtSignal(object)


class _Job(QRunnable):
    def __init__(self, function, signals):
        super().__init__()
        self.function, self.signals = function, signals

    def run(self):
        try:
            value = self.function()
        except Exception as exc:  # a worker must always answer
            value = exc
        try:
            self.signals.done.emit(value)
        except RuntimeError:
            pass  # the owner was deleted while the job ran


def run_background(pool, function, callback, owner):
    """Run ``function`` in the thread pool; ``callback(result)`` runs on the UI thread (an Exception on failure)."""
    signals = _Signals(owner)

    def deliver(value):
        signals.deleteLater()
        callback(value)

    signals.done.connect(deliver)
    job = _Job(function, signals)
    job.setAutoDelete(True)
    pool.start(job)


def probe_project(path):
    """Local facts about a remembered project folder. Blocking disk access: call from a worker only."""
    base = Path(path)
    try:
        if not base.is_dir():
            return {"status": "missing", "host": ""}
        file = base / "project.json"
        if not file.is_file():
            return {"status": "not_project", "host": ""}
        host = ""
        if file.stat().st_size <= 1 << 20:
            site = (json.loads(file.read_text(encoding="utf-8")) or {}).get("site")
            host = site.get("host") if isinstance(site, dict) and isinstance(site.get("host"), str) else ""
        return {"status": "ok", "host": host}
    except (OSError, ValueError, AttributeError):
        return {"status": "missing", "host": ""}


def probe_all(items):
    return {item["path"]: probe_project(item["path"]) for item in items}


def initials(label):
    words = [w for w in label.replace("·", " ").split() if w[:1].isalnum()]
    if len(words) >= 2:
        return (words[0][0] + words[1][0]).upper()
    return (label.strip()[:2] or "?").upper()


def tilde(path):
    home = str(Path.home())
    return "~" + path[len(home):] if path == home or path.startswith(home + "/") else path


def when_text(stamp, now=None):
    """«сегодня 11:20» / «вчера» / «12.09.2026»; an empty string when the time was never recorded."""
    try:
        moment = datetime.fromisoformat(stamp)
    except (TypeError, ValueError):
        return ""
    now = now or datetime.now(moment.tzinfo)
    days = (now.date() - moment.date()).days
    if days == 0:
        return trf("сегодня {time}", time=moment.strftime("%H:%M"))
    if days == 1:
        return tr("вчера")
    return moment.strftime("%d.%m.%Y")


class ProjectModel(QAbstractListModel):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.rows = []

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid():
            return None
        row = self.rows[index.row()]
        if role == Qt.DisplayRole:
            return row["label"]
        if role == ROW_ROLE:
            return row
        if role == SEARCH_ROLE:
            return " ".join((row["label"], row["host"], row["path"])).lower()
        if role == Qt.ToolTipRole:
            return row["path"]
        return None

    def replace(self, rows):
        self.beginResetModel()
        self.rows = rows
        self.endResetModel()


class ProjectDelegate(QStyledItemDelegate):
    """Paints avatar, name, «host · path», status badge, last-opened time and a chevron; no widget per row."""

    STATUS = {"missing": ("mut", "folder_off", "Папка не найдена"), "not_project": ("warn", "warning", "Нет project.json"),
              "open": ("info", "check_circle", "Открыт сейчас"),
              # the core has no per-project status yet (issue #1224): neutral pill, no invented state
              "waiting": ("mut", "", UNAVAILABLE)}

    @classmethod
    def badge_key(cls, row):
        """Which badge a row shows: the open project, the core gap for a healthy folder, or the probe result."""
        if row.get("current"):
            return "open"
        return "waiting" if row["status"] == "ok" else row["status"]

    def sizeHint(self, option, index):
        return QSize(option.rect.width(), ROW_HEIGHT)

    def paint(self, painter, option, index):
        row = index.data(ROW_ROLE)
        if not row:
            return
        roles, badges = theming.roles(), theming.theme()["badges"]
        painter.save()
        painter.setRenderHint(QPainter.Antialiasing)
        rect = option.rect
        selected = bool(option.state & QStyle.State_Selected)
        if selected:
            painter.fillRect(rect, QColor(roles["selected"]))
        elif option.state & QStyle.State_MouseOver:
            painter.fillRect(rect, QColor(roles["hover_row"]))
        painter.setPen(QColor(roles["divider_inner"]))
        painter.drawLine(rect.left(), rect.bottom(), rect.right(), rect.bottom())
        # avatar
        kind = AVATAR_KINDS[zlib.crc32(row["path"].encode()) % len(AVATAR_KINDS)]
        background, ink = badges[kind]
        box = QRectF(rect.left() + 16, rect.center().y() - 20, 40, 40)
        painter.setPen(Qt.NoPen)
        painter.setBrush(QColor(background))
        painter.drawRoundedRect(box, 10, 10)
        font = QFont(option.font)
        font.setPixelSize(15)
        font.setWeight(QFont.Medium)
        painter.setFont(font)
        painter.setPen(QColor(ink))
        painter.drawText(box, Qt.AlignCenter, initials(row["label"]))
        # right side, from the edge: chevron, time, status
        right = rect.right() - 16
        icon_rect = QRect(right - 18, rect.center().y() - 9, 18, 18)
        material_icon("chevron_right", roles["on_selected"] if selected else roles["disabled_icon"]).paint(painter, icon_rect)
        right = icon_rect.left() - 14
        small = QFont(option.font)
        small.setPixelSize(12)
        painter.setFont(small)
        wide = rect.width() >= 640
        when = when_text(row.get("opened_at")) if wide else ""
        if when:
            right -= 120
            painter.setPen(QColor(roles["text_3"]))
            painter.drawText(QRect(right, rect.top(), 120, rect.height()), Qt.AlignVCenter | Qt.AlignLeft,
                             painter.fontMetrics().elidedText(when, Qt.ElideRight, 120))
            right -= 14
        status = self.badge_key(row)
        if status in self.STATUS:
            kind, icon_name, text = self.STATUS[status]
            text = tr(text)
            inset = 24 if icon_name else 8
            badge_font = QFont(small)
            badge_font.setWeight(QFont.Medium)
            painter.setFont(badge_font)
            width = min(painter.fontMetrics().horizontalAdvance(text) + inset + 12, max(60, right - rect.left() - 200))
            badge = QRectF(right - width, rect.center().y() - 11, width, 22)
            painter.setPen(Qt.NoPen)
            painter.setBrush(QColor(badges[kind][0]))
            painter.drawRoundedRect(badge, 6, 6)
            if icon_name:
                material_icon(icon_name, badges[kind][1]).paint(painter, QRect(int(badge.left()) + 6, int(badge.center().y()) - 7, 14, 14))
            painter.setPen(QColor(badges[kind][1]))
            painter.drawText(QRectF(badge.left() + inset, badge.top(), badge.width() - inset - 4, 22), Qt.AlignVCenter | Qt.AlignLeft,
                             painter.fontMetrics().elidedText(text, Qt.ElideRight, int(badge.width()) - inset - 4))
            right = int(badge.left()) - 14
        # texts
        left = rect.left() + 70
        width = max(40, right - left)
        name_font = QFont(option.font)
        name_font.setPixelSize(14)
        name_font.setWeight(QFont.Medium)
        painter.setFont(name_font)
        painter.setPen(QColor(roles["on_selected"] if selected else roles["text"]))
        painter.drawText(QRect(left, rect.top() + 10, width, 22), Qt.AlignVCenter | Qt.AlignLeft,
                         painter.fontMetrics().elidedText(row["label"], Qt.ElideRight, width))
        painter.setFont(small)
        painter.setPen(QColor(roles["text_3"]))
        subtitle = " · ".join(part for part in (row["host"], tilde(row["path"])) if part)
        painter.drawText(QRect(left, rect.top() + 32, width, 20), Qt.AlignVCenter | Qt.AlignLeft,
                         painter.fontMetrics().elidedText(subtitle, Qt.ElideMiddle, width))
        painter.restore()


class ProjectList(QListView):
    removeRequested = pyqtSignal(int)  # source row

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setProperty("project_list", True)
        self.setMouseTracking(True)
        self.setFrameShape(QFrame.NoFrame)
        self.setEditTriggers(QListView.NoEditTriggers)
        self.setVerticalScrollMode(QListView.ScrollPerPixel)
        self.setItemDelegate(ProjectDelegate(self))
        self.setUniformItemSizes(True)
        self.setAccessibleName("Недавние проекты")

    def keyPressEvent(self, event):
        if event.key() in (Qt.Key_Return, Qt.Key_Enter) and self.currentIndex().isValid():
            self.clicked.emit(self.currentIndex())
            return
        if event.key() in (Qt.Key_Delete, Qt.Key_Backspace) and self.currentIndex().isValid():
            self.removeRequested.emit(self.currentIndex().data(ROW_ROLE)["_row"])
            return
        super().keyPressEvent(event)


class NewProjectDialog(QDialog):
    """Sheet Modals «Новый проект»: name, main site, folder. «Создать» is the user's confirmation of the write."""

    def __init__(self, host, parent=None):
        super().__init__(parent or host)
        self.host = host
        self.result_info = None
        self.open_after = True
        self._directory_edited = False
        self.setWindowTitle(tr("Новый проект"))
        self.setModal(True)
        self.setMinimumWidth(520)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(24, 20, 24, 16)
        layout.setSpacing(14)
        title = QLabel(tr("Новый проект"))
        title.setProperty("text_style", "dialog")
        hint = QLabel(tr("Папка, сайт и название"))
        hint.setProperty("text_style", "meta")
        layout.addWidget(title)
        layout.addWidget(hint)

        self.label = QLineEdit()
        self.label.setPlaceholderText(tr("По умолчанию — адрес сайта"))
        self.label.setAccessibleName(tr("Название"))
        self.target = QLineEdit()
        self.target.setPlaceholderText("https://")
        self.target.setAccessibleName(tr("Основной сайт"))
        self.directory = QLineEdit()
        self.directory.setAccessibleName(tr("Папка проекта"))
        self.choose = QPushButton(tr("Выбрать…"))
        self.choose.setAccessibleName(tr("Выбрать папку проекта"))
        folder = QHBoxLayout()
        folder.setSpacing(8)
        folder.addWidget(self.directory, 1)
        folder.addWidget(self.choose)
        self.target_error = QLabel()
        self.target_error.setProperty("text_style", "meta")
        self.target_error.setWordWrap(True)
        self.target_error.hide()
        layout.addLayout(self._field(tr("Название"), self.label))
        layout.addLayout(self._field(tr("Основной сайт"), self.target, self.target_error))
        self.directory_error = QLabel()
        self.directory_error.setProperty("text_style", "meta")
        self.directory_error.setWordWrap(True)
        self.directory_error.hide()
        folder_box = QVBoxLayout()
        folder_box.setSpacing(4)
        caption = QLabel(tr("Папка проекта"))
        caption.setProperty("text_style", "control")
        folder_box.addWidget(caption)
        folder_box.addLayout(folder)
        self.folder_hint = QLabel(tr("Папки ещё нет — ядро создаст её вместе с project.json. Скан не запускается."))
        self.folder_hint.setProperty("text_style", "meta")
        self.folder_hint.setWordWrap(True)
        folder_box.addWidget(self.folder_hint)
        folder_box.addWidget(self.directory_error)
        layout.addLayout(folder_box)
        self.open_switch = Switch(tr("Сразу открыть проект"), True)
        row = QHBoxLayout()
        row.setSpacing(8)
        row.addWidget(self.open_switch)
        row.addWidget(QLabel(tr("Сразу открыть проект после создания")), 1)
        layout.addLayout(row)
        self.failure = Note("error", "Проект не создан.", "-")
        self.failure_text = self.failure.findChildren(QLabel)[-1]
        self.failure_text.setTextFormat(Qt.RichText)
        self.failure.hide()
        layout.addWidget(self.failure)
        buttons = QHBoxLayout()
        buttons.setSpacing(8)
        buttons.addStretch(1)
        self.cancel = QPushButton(tr("Отмена"))
        self.create = QPushButton(tr("Создать"))
        self.create.setProperty("role", "primary")
        self.create.setIcon(material_icon("add", theming.roles()["on_primary"]))
        buttons.addWidget(self.cancel)
        buttons.addWidget(self.create)
        layout.addLayout(buttons)

        base = Path(str(host.prefs.get("general.projects_folder")) or "~").expanduser()
        self._base = base
        if not host.core_executable:
            self.create.setEnabled(False)
            self.create.setToolTip(tr("Ядро seohead не найдено"))
            self._fail(tr("Ядро seohead не найдено — создать проект нечем."))
        self.creator = ProjectCreator(self)
        self.creator.created.connect(self._created)
        self.creator.failed.connect(self._failed)
        self.cancel.clicked.connect(self.reject)
        self.create.clicked.connect(self.submit)
        self.choose.clicked.connect(self.pick_folder)
        self.target.textChanged.connect(self._suggest)
        self.label.textChanged.connect(self._suggest)
        self.directory.textEdited.connect(self._edited)
        self.target.editingFinished.connect(self._normalize)
        self.target.setFocus()
        i18n.retranslate(self)

    @staticmethod
    def _field(caption, widget, error=None):
        box = QVBoxLayout()
        box.setSpacing(4)
        label = QLabel(caption)
        label.setProperty("text_style", "control")
        box.addWidget(label)
        box.addWidget(widget)
        if error is not None:
            box.addWidget(error)
        return box

    def _edited(self, _text):
        self._directory_edited = True

    def _suggest(self):
        if not self._directory_edited:
            self.directory.setText(suggest_directory(self._base, self.target.text(), self.label.text()))

    def _normalize(self):
        text = normalize_target(self.target.text())
        if text != self.target.text():
            self.target.setText(text)

    def pick_folder(self):
        parent = QFileDialog.getExistingDirectory(self, tr("Родительская папка нового проекта"), str(self._base))
        if parent:
            name = Path(self.directory.text()).name or Path(suggest_directory(parent, self.target.text(), self.label.text())).name
            self.directory.setText(str(Path(parent) / name) if name else parent)
            self._directory_edited = True

    def _set_invalid(self, field, message):
        for edit, error, key in ((self.target, self.target_error, "target"), (self.directory, self.directory_error, "directory")):
            bad = bool(message) and field == key
            edit.setProperty("invalid", bad)
            polish(edit)
            error.setText(tr(message) if bad else "")
            error.setVisible(bad)

    def _fail(self, text):
        self.failure_text.setText(f"<b>{html.escape(tr('Проект не создан.'))}</b> {html.escape(tr(text))}")
        self.failure.show()

    def submit(self):
        self.failure.hide()
        self._normalize()
        problem = validate(self.directory.text(), self.target.text())
        self._set_invalid(*(problem or ("", "")))
        if problem or not self.host.core_executable:
            return
        self.create.setEnabled(False)
        self.create.setText(tr("Создание…"))
        self.creator.start(self.host.core_executable, self.directory.text(), self.target.text(), self.label.text())

    def _created(self, result):
        self.result_info = result
        self.open_after = self.open_switch.isChecked()
        self.accept()

    def _failed(self, text):
        self.create.setEnabled(bool(self.host.core_executable))
        self.create.setText(tr("Создать"))
        self._fail(text)


class StartScreen(Screen):
    """Project list before a project is open (sheet Start). Not a navigation slot: registered as an extra."""

    watches = ("project", "recents")
    chrome_free = True  # SHELL-CANON §7: no project / scan switchers in the top bar

    def __init__(self, host):
        super().__init__(host)
        self._generation = 0
        self._probes = {}
        self._relocate = None
        self.checked = False
        self.model = ProjectModel(self)
        self.proxy = QSortFilterProxyModel(self)
        self.proxy.setSourceModel(self.model)
        self.proxy.setFilterRole(SEARCH_ROLE)
        self.proxy.setFilterCaseSensitivity(Qt.CaseInsensitive)

        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        outer.addWidget(scroll)
        page = QWidget()
        page.setObjectName("startPage")
        scroll.setWidget(page)
        row = QHBoxLayout(page)
        row.setContentsMargins(24, 48, 24, 48)
        row.addStretch(1)
        column = QWidget()
        column.setProperty("plain", True)
        column.setMaximumWidth(920)
        column.setMinimumWidth(0)
        row.addWidget(column, 100)
        row.addStretch(1)
        body = QVBoxLayout(column)
        body.setContentsMargins(0, 0, 0, 0)
        body.setSpacing(16)

        head = QHBoxLayout()
        head.setSpacing(8)
        texts = QVBoxLayout()
        texts.setSpacing(4)
        self.title = QLabel(tr("Проекты"))
        self.title.setProperty("text_style", "title")
        self.meta = QLabel()
        self.meta.setProperty("text_style", "meta")
        self.meta.setWordWrap(True)
        texts.addWidget(self.title)
        texts.addWidget(self.meta)
        head.addLayout(texts, 1)
        self.open_button = QPushButton(tr("Открыть папку проекта"))
        self.open_button.setIcon(material_icon("folder_open"))
        self.new_button = QPushButton(tr("Новый проект"))
        self.new_button.setProperty("role", "primary")
        self.new_button.setIcon(material_icon("add", theming.roles()["on_primary"]))
        head.addWidget(self.open_button, 0, Qt.AlignBottom)
        head.addWidget(self.new_button, 0, Qt.AlignBottom)
        body.addLayout(head)

        self.search = QLineEdit()
        self.search.setPlaceholderText(tr("Найти по имени или домену"))
        self.search.setAccessibleName(tr("Найти проект"))
        self.search.setClearButtonEnabled(True)
        self.search.addAction(material_icon("search", "role:text_muted"), QLineEdit.LeadingPosition)
        body.addWidget(self.search)

        self.card = QFrame()
        self.card.setProperty("card", "panel")
        card_layout = QVBoxLayout(self.card)
        card_layout.setContentsMargins(0, 0, 0, 0)
        card_layout.setSpacing(0)
        self.list = ProjectList()
        self.list.setModel(self.proxy)
        card_layout.addWidget(self.list)
        self.empty = StatePanel("empty", "Недавних проектов нет",
                                "Откройте папку с project.json или создайте новый проект — он появится в этом списке.",
                                action=("Новый проект", self.new_project))
        card_layout.addWidget(self.empty)
        self.no_match = StatePanel("empty", "Ничего не найдено", "Измените запрос: поиск идёт по имени, домену и пути.")
        card_layout.addWidget(self.no_match)
        body.addWidget(self.card)
        self.banners = QVBoxLayout()
        self.banners.setSpacing(8)
        body.addLayout(self.banners)
        body.addStretch(1)

        self.open_button.clicked.connect(self.host.choose_project)
        self.new_button.clicked.connect(self.new_project)
        self.search.textChanged.connect(self._filter)
        self.list.clicked.connect(self._activated)
        self.list.removeRequested.connect(self.forget)
        self.list.setContextMenuPolicy(Qt.CustomContextMenu)
        self.list.customContextMenuRequested.connect(self._menu)
        self.refresh()

    # data
    def _data_changed(self, kind):
        if kind == "project":
            self._finish_relocation()
            if self.host.project_directory and self.host.pages.currentWidget() is self:
                self.host.leave_start()
        if self.host.pages.currentWidget() is self:  # a hidden list is refreshed when it is shown again
            super()._data_changed(kind)

    def refresh(self):
        items = [dict(item) for item in self.host.recent_projects]
        current = self.host.project_directory if self.host.project_directory else None
        known = {path: probe for path, probe in self._probes.items()}
        rows = []
        for index, item in enumerate(items):
            probe = known.get(item["path"], {"status": "checking", "host": ""})
            rows.append({**item, **probe, "_row": index, "current": current is not None and _same(item["path"], current)})
        self.model.replace(rows)
        meta = "Открытые ранее" if self.host.display == "simple" else "Открытые ранее · агент работает с теми же проектами через MCP"
        self.meta.setText(tr(meta))
        self._sync_view()
        self._generation += 1
        generation = self._generation
        self.checked = not items
        if items:
            run_background(self.host.pool, lambda: probe_all(items), lambda result: self._probed(generation, result), self)

    def _probed(self, generation, result):
        if generation != self._generation or isinstance(result, Exception):
            return
        self._probes = result
        for row in self.model.rows:
            row.update(result.get(row["path"], {}))
        self.model.dataChanged.emit(self.model.index(0), self.model.index(max(0, len(self.model.rows) - 1)))
        self.checked = True
        self._sync_view()

    def _sync_view(self):
        has_rows = bool(self.model.rows)
        self.empty.setVisible(not has_rows)
        self.no_match.setVisible(has_rows and self.proxy.rowCount() == 0)
        self.list.setVisible(has_rows and self.proxy.rowCount() > 0)
        self.search.setEnabled(has_rows)
        self.list.setFixedHeight(min(max(1, self.proxy.rowCount()), 8) * ROW_HEIGHT)
        self._rebuild_banners()

    def _filter(self, text):
        self.proxy.setFilterFixedString(text.strip().lower())
        self._sync_view()

    # banners for folders that are gone
    def _rebuild_banners(self):
        while self.banners.count():
            widget = self.banners.takeAt(0).widget()
            if widget is not None:
                widget.setParent(None)
                widget.deleteLater()
        bad = [row for row in self.model.rows if row["status"] in ("missing", "not_project")]
        for row in bad[:BANNER_LIMIT]:
            self.banners.addWidget(self._banner(row))
        if len(bad) > BANNER_LIMIT:
            more = QLabel(trf("И ещё проектов с недоступной папкой: {n}", n=len(bad) - BANNER_LIMIT))
            more.setProperty("text_style", "meta")
            self.banners.addWidget(more)

    def _banner(self, row):
        name, path = html.escape(row["label"]), html.escape(tilde(row["path"]))
        if row["status"] == "missing":
            title, text = trf("Проект «{name}» не найден по прежнему пути.", name=name), trf("Возможно, папку перенесли: {path}", path=path)
        else:
            title, text = trf("В папке проекта «{name}» нет project.json.", name=name), path
        box = QWidget()
        box.setProperty("plain", True)
        buttons = QHBoxLayout(box)
        buttons.setContentsMargins(0, 0, 0, 0)
        buttons.setSpacing(8)
        locate = QPushButton(tr("Указать новый путь"))
        locate.clicked.connect(lambda _c=False, p=row["path"]: self.relocate(p))
        forget = QPushButton(tr("Убрать из списка"))
        forget.setProperty("role", "text")
        forget.clicked.connect(lambda _c=False, p=row["path"]: self.forget_path(p))
        buttons.addWidget(locate)
        buttons.addWidget(forget)
        note = Note("warn", title, text, action=box)
        note.setProperty("project_banner", row["path"])
        return note

    # actions (all through the window)
    def _activated(self, index):
        row = index.data(ROW_ROLE)
        if not row:
            return
        if row["status"] in ("missing", "not_project"):
            self.host.statusBar().showMessage(trf("Папка проекта «{name}» недоступна: {path}", name=row["label"], path=row["path"]))
            return
        self.host.read_project(row["path"])

    def forget_path(self, path):
        self.host.forget_project(path)

    def forget(self, row):
        if 0 <= row < len(self.host.recent_projects):
            self.host.forget_project(self.host.recent_projects[row]["path"])

    def _menu(self, point):
        index = self.list.indexAt(point)
        row = index.data(ROW_ROLE)
        if not row:
            return
        menu = QMenu(self.list)
        open_action = menu.addAction(material_icon("folder_open"), tr("Открыть"))
        open_action.setEnabled(row["status"] not in ("missing", "not_project"))
        open_action.triggered.connect(lambda: self._activated(index))
        forget = menu.addAction(material_icon("close"), tr("Убрать из списка"))
        forget.setToolTip(tr("Файлы проекта не удаляются"))
        forget.triggered.connect(lambda: self.forget(row["_row"]))
        menu.exec_(self.list.viewport().mapToGlobal(point))

    def relocate(self, old_path):
        directory = QFileDialog.getExistingDirectory(self, tr("Новая папка проекта"), str(Path(old_path).parent))
        if not directory:
            return
        if not (Path(directory) / "project.json").is_file():
            self.host.notice.show_error("В выбранной папке нет project.json SEOHEAD.", "project-open")
            return
        self._relocate = (old_path, directory)
        self.host.read_project(directory)

    def _finish_relocation(self):
        if self._relocate and self.host.project_directory and _same(self.host.project_directory, self._relocate[1]):
            old, self._relocate = self._relocate[0], None
            if not _same(old, self.host.project_directory):
                self.host.forget_project(old)

    def new_project(self):
        dialog = NewProjectDialog(self.host, self.window())
        accepted = dialog.exec_() == QDialog.Accepted
        info = dialog.result_info
        open_after = dialog.open_after
        dialog.deleteLater()
        if not accepted or not info:
            return None
        project = info.get("project") or {}
        site = project.get("site") or {}
        label = site.get("label") or site.get("host") or Path(info["path"]).name
        self.host.remember_project(label, info["path"])
        if open_after:
            self.host.read_project(info["path"])
        return info


def _same(left, right):
    return os.path.normpath(left) == os.path.normpath(right)
