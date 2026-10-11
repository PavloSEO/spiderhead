"""«Источники данных проекта» (sheet ProjSources) and the «Настройки проекта» container (sheet ProjSettings).

The page shows, per service of the sheet, the access the core reports (provider-readiness, local, no network).
Choosing a resource, the sync time, "link" / "unlink" and «Сохранить связи» need the project↔resource storage the core
does not have yet: they are shown as the neutral «Недоступно в этой версии ядра» state. Nothing is written or faked.
"""

from __future__ import annotations

from PyQt5.QtCore import QSize, Qt
from PyQt5.QtWidgets import (
    QDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QScrollArea,
    QStackedWidget,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import i18n, theming
from ..i18n import joined, tr, trf
from ..ui.brand_logos import BrandTile
from ..ui.canvas_components import CanvasTabs
from ..ui.controls import Note
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import UNAVAILABLE, PageHeader, StatePanel, waiting_badge
from ..ui.presentation import ElidedLabel
from ..ui.settings.listing import badge, terminal
from .project_schedule_page import CrawlPage, SchedulePage, next_run_text
from .project_sources import GAP, GAP_HINT, access_count, build_rows
from .work import project_names

# Canvas grid: status, sync and actions are fixed widths. The free width goes to service and resource at 2 : 1,
# not the canvas 1.05 : 1.5: the sheet is a 940 px dialog here, and the access line of a service must stay readable.
SERVICE_STRETCH, RESOURCE_STRETCH = 2, 1
STATUS_W, SYNC_W, ACTIONS_W, GAP_W = 150, 110, 104, 12
NARROW = 760  # below this the sync column goes away


def unavailable_text(hint=GAP_HINT):
    return f"{tr(UNAVAILABLE)}\n{tr('Появится')}: {tr(hint)}"


def icon_button(name, tip, enabled=True, callback=None):
    button = QToolButton()
    button.setProperty("role", "icon")
    button.setIcon(material_icon(name, "role:text_2"))
    button.setToolTip(tip)
    button.setAccessibleName(tip)
    button.setEnabled(enabled)
    if callback is not None:
        button.clicked.connect(callback)
    return button


def clear_background(root):
    """Row cells are transparent so the row's own background (head band, hover) shows through; badges keep theirs."""
    for widget in root.findChildren(QWidget):
        if widget.property("badge") is None and not isinstance(widget, QToolButton):
            widget.setProperty("rowbg", "clear")


def cell(widget, width=None):
    if width is not None:
        widget.setFixedWidth(width)
    return widget


class SourceRowWidget(QFrame):
    """One service: mark, name + access, resource, status, sync, actions. 50 px, eight of them at most."""

    def __init__(self, row, open_access):
        super().__init__()
        self.row = row
        self.setProperty("source_row", "item")
        self.setProperty("service", row.key)
        self.setFixedHeight(50)
        layout = QHBoxLayout(self)
        layout.setContentsMargins(8, 0, 8, 0)
        layout.setSpacing(GAP_W)
        self.tile = BrandTile(row.key)
        layout.addWidget(self.tile)
        names = QVBoxLayout()
        names.setSpacing(1)
        names.setContentsMargins(0, 0, 0, 0)
        self.name = QLabel(tr(row.service))
        self.name.setProperty("text_style", "control")
        self.access = ElidedLabel(row.access)
        self.access.setProperty("text_style", "meta")
        self.access.setVisible(bool(row.access))
        names.addStretch(1)
        names.addWidget(self.name)
        names.addWidget(self.access)
        names.addStretch(1)
        layout.addLayout(names, SERVICE_STRETCH)
        self.resource = QLabel(tr("Нет данных"))
        self.resource.setProperty("na", True)
        self.resource.setToolTip(
            f"{trf('Связь проекта: {kind}', kind=row.resource_kind)}\n{unavailable_text()}"
        )
        layout.addWidget(self.resource, RESOURCE_STRETCH)
        if row.state is None:
            self.status = waiting_badge(
                GAP, "Состояние доступа этого сервиса ядро пока не сообщает"
            )
            self.status.setFixedWidth(
                self.status.minimumSizeHint().width()
            )  # the short pill, not a bar across the column
        else:
            self.status = badge(row.kind, tr(row.text), row.icon)
        self.status_holder = QWidget()
        holder = QHBoxLayout(self.status_holder)
        holder.setContentsMargins(0, 0, 0, 0)
        holder.addWidget(self.status)
        holder.addStretch(1)
        layout.addWidget(self.status_holder)
        self.sync = cell(QLabel("—"), SYNC_W)
        self.sync.setProperty("text_style", "meta")
        self.sync.setToolTip(f"{tr('Нет данных')}\n{unavailable_text()}")
        layout.addWidget(self.sync)
        actions = QHBoxLayout()
        actions.setSpacing(2)
        self.configure = icon_button(
            "tune",
            tr("Настроить доступ")
            if row.has_access_settings
            else tr("Настройки доступа этого сервиса в приложении пока нет"),
            row.has_access_settings,
            open_access,
        )
        self.sync_now = icon_button(
            "sync", f"{tr('Синхронизировать сейчас')} · {tr(UNAVAILABLE)}", False
        )
        self.unlink = icon_button("link_off", f"{tr('Отвязать')} · {tr(UNAVAILABLE)}", False)
        for button in (self.configure, self.sync_now, self.unlink):
            actions.addWidget(button)
        self.actions = cell(QWidget(), ACTIONS_W)
        self.actions.setLayout(actions)
        actions.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(self.actions)
        clear_background(self)


class HeadRow(QFrame):
    def __init__(self, status_width):
        super().__init__()
        self.setProperty("source_row", "head")
        self.setFixedHeight(30)
        layout = QHBoxLayout(self)
        layout.setContentsMargins(8, 0, 8, 0)
        layout.setSpacing(GAP_W)

        def caption(text, width=None):
            label = QLabel(tr(text).upper())
            label.setProperty("text_style", "overline")
            return cell(label, width)

        layout.addSpacing(40 + GAP_W)
        layout.addWidget(caption("Сервис · доступ"), SERVICE_STRETCH)
        resource = QWidget()
        resource_layout = QHBoxLayout(resource)
        resource_layout.setContentsMargins(0, 0, 0, 0)
        resource_layout.setSpacing(2)
        resource_layout.addWidget(caption("Ресурс"))
        self.help = icon_button("help", unavailable_text(), True)
        self.help.setProperty("size", "dense")
        self.help.setFocusPolicy(Qt.NoFocus)
        resource_layout.addWidget(self.help)
        resource_layout.addStretch(1)
        layout.addWidget(resource, RESOURCE_STRETCH)
        layout.addWidget(caption("Статус", status_width))
        self.sync = caption("Синхр.", SYNC_W)
        layout.addWidget(self.sync)
        layout.addWidget(caption("Действия", ACTIONS_W))
        clear_background(self)


class ProjectSourcesPage(QWidget):
    """States: none | loading | error | partial | ready. ``host`` supplies the project, the readiness request and the settings."""

    def __init__(self, host):
        super().__init__()
        self.host = host
        self.state = None
        self.rows = []
        self._token = 0
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        self.stack = QStackedWidget()
        outer.addWidget(self.stack)
        self.reload()

    # data
    def reload(self):
        if not self.host.project_directory:
            return self._show_state(
                "none",
                StatePanel(
                    "empty",
                    "Проект не открыт",
                    "Источники привязываются к открытому проекту.",
                    action=("Открыть проект…", self.host.choose_project),
                ),
            )
        self._token += 1
        token = self._token
        self._show_state(
            "loading",
            StatePanel(
                "loading",
                "Читаем доступы у ядра…",
                "Проверка сети не выполняется: ядро сообщает только, задан ли ключ",
            ),
        )
        self.host.request_providers(
            lambda result: self._loaded(token, result), lambda text: self._failed(token, text)
        )

    def _loaded(self, token, result):
        if token != self._token:
            return
        providers = result.get("providers") if isinstance(result, dict) else None
        if not isinstance(providers, dict) or not providers:
            self._show_state(
                "partial",
                StatePanel(
                    "partial",
                    "Ядро не вернуло список сервисов",
                    "Обновите ядро или проверьте его командой seohead provider-readiness",
                    action=("Повторить", self.reload),
                ),
            )
            return
        self.rows = build_rows(providers)
        self._show_state("ready", self._content())

    def _failed(self, token, text):
        if token == self._token:
            self._show_state(
                "error",
                StatePanel(
                    "error",
                    "Не удалось получить доступы",
                    str(text or ""),
                    action=("Повторить", self.reload),
                ),
            )

    def _show_state(self, state, widget):
        self.state = state
        while self.stack.count():
            old = self.stack.widget(0)
            self.stack.removeWidget(old)
            old.deleteLater()
        self.stack.addWidget(widget)
        i18n.retranslate(widget)

    # actions
    def open_access(self):
        """Application-level access settings (Settings → Источники данных); the readiness is read again after they close."""
        self.host.open_settings("sources")
        self.reload()

    # content
    def _content(self):
        label, host_name = project_names(self.host)
        meta = joined(
            " · ",
            [part for part in (label, host_name) if part]
            + [
                trf(
                    "доступ задан у {n} из {total} сервисов",
                    n=access_count(self.rows),
                    total=len(self.rows),
                )
            ],
        )
        body = QWidget()
        layout = QVBoxLayout(body)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(10)
        self.header = PageHeader("Источники данных проекта", meta)
        access = QPushButton(tr("Доступы приложения"))
        access.setIcon(material_icon("key", theming.roles()["text"]))
        access.clicked.connect(self.open_access)
        self.access_button = self.header.add_action(access)
        save = QPushButton(tr("Сохранить связи"))
        save.setProperty("role", "primary")
        save.setIcon(material_icon("save", theming.roles()["on_primary"]))
        save.setEnabled(False)
        save.setToolTip(unavailable_text())
        self.save_button = self.header.add_action(save)
        layout.addWidget(self.header)
        note_text = "Ядро пока не хранит связи проекта с ресурсами. Ниже — доступы, которые оно видит; выбор ресурса, синхронизация и «Сохранить связи» появятся позже."
        self.note = Note(
            "info",
            tr("Ресурсы проекта пока не привязываются."),
            tr(note_text),
            action=waiting_badge(GAP, GAP_HINT),
        )
        layout.addWidget(self.note)
        self.row_widgets = [SourceRowWidget(row, self.open_access) for row in self.rows]
        # the status column is as wide as the widest badge (a «not available» pill shrinks to its short form)
        status_width = max(
            STATUS_W,
            max(
                (w.status.minimumSizeHint() if w.row.state is None else w.status.sizeHint()).width()
                for w in self.row_widgets
            )
            + 4,
        )
        self.head = HeadRow(status_width)
        layout.addWidget(self.head)
        for widget in self.row_widgets:
            widget.status_holder.setFixedWidth(status_width)
            layout.addWidget(widget)
        layout.addStretch(1)
        layout.addWidget(
            terminal(
                f"seohead provider-readiness   # {tr('то же из терминала · сеть не проверяется')}",
                [],
            )
        )
        self._body = body
        self._fit()
        return body

    def _fit(self):
        narrow = self.width() < NARROW
        if hasattr(self, "head"):
            for widget in (self.head.sync, *(row.sync for row in self.row_widgets)):
                widget.setVisible(not narrow)

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._fit()


class ProjectSettingsPage(QWidget):
    """«Настройки проекта» (sheet ProjSettings): the container with its four tabs.

    Only «Источники» (the core's readiness) and the read-only «Основное» (name, host, folder from the open project) show
    real data. Default profile, goals and deletion need core storage that does not exist yet: they are neutral «Недоступно»
    badges, never samples. «Расписание» and «Обход» read and write the project settings through the core; «Экспорт» is a waiting state.
    """

    def __init__(self, host, parent=None):
        super().__init__(parent)
        self.host = host
        self.setObjectName("projectSettingsPage")
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        self.header_frame = self._header()
        root.addWidget(self.header_frame)
        self.page = ProjectSourcesPage(host)
        self.nav = CanvasTabs()
        self.nav.setObjectName("projectSettingsTabs")
        self.nav.setExpanding(False)
        self.nav.setDrawBase(False)
        self.nav.setIconSize(QSize(20, 20))
        self.nav.setFixedHeight(45)
        self.pages = QStackedWidget()
        self.pages.setObjectName("projectSettingsPages")
        tabs = (
            ("settings", "Main", self._main_page()),
            ("database", "Sources", _scrolled(self.page)),
            ("event_repeat", "Schedule", SchedulePage(host)),
            ("travel_explore", "Обход", CrawlPage(host)),
            (
                "ios_share",
                "Экспорт",
                StatePanel(
                    "waiting", "Экспорт и отчёты", "Экспорт настроек проекта появится позже"
                ),
            ),
        )
        for icon_name, title, widget in tabs:
            self.nav.add_canvas_tab(icon_name, tr(title))
            self.pages.addWidget(widget)
            if isinstance(widget, (SchedulePage, CrawlPage)):
                widget.revert.hide()
                widget.save_button.hide()
                widget.availabilityChanged.connect(lambda _available: self._sync_actions())
        self.nav.currentChanged.connect(self.pages.setCurrentIndex)
        self.nav.currentChanged.connect(self._sync_actions)
        self.nav.currentChanged.connect(
            lambda index: self.header_icon.setPixmap(
                material_icon(self.nav.names[index], "role:primary").pixmap(20, 20)
            )
        )
        self.nav.setCurrentIndex(0)
        root.addWidget(self.nav)
        self.pages.setContentsMargins(24, 20, 24, 20)
        root.addWidget(self.pages, 1)
        i18n.retranslate(self)
        self._sync_actions()

    def refresh(self):
        label, host_name = project_names(self.host)
        self.context.setText(joined(" · ", [part for part in (label, host_name) if part]))
        for index in (2, 3):
            self.pages.widget(index).reload()
        self.page.reload()

    def _sync_actions(self):
        page = self.pages.currentWidget()
        editable = isinstance(page, (SchedulePage, CrawlPage))
        self.save.setEnabled(editable and page.save_button.isEnabled())
        self.cancel.setEnabled(editable and page.revert.isEnabled())
        state = getattr(page, "state", None)
        self.next_pill.setVisible(
            self.nav.currentIndex() == 2
            and self.width() >= 1000
            and bool(state and state["settings"]["enabled"] and state["next_run"])
        )
        if state:
            self.next_badge.setText("Next scan · " + next_run_text(state["next_run"]))
        if hasattr(self.host, "navigation") and hasattr(self.host.navigation, "settings_note"):
            nav = self.host.navigation
            nav.settings_note.setVisible(
                self.isVisible()
                and self.nav.currentIndex() == 2
                and self.width() >= 1000
                and bool(state)
            )
            if state:
                nav.settings_revision.setText(f"schedule.json · rev {state['revision']}")

    def _edit_action(self, method):
        page = self.pages.currentWidget()
        if isinstance(page, (SchedulePage, CrawlPage)) and page.isEnabled():
            getattr(page, method)()

    def _header(self):
        frame = QFrame()
        frame.setObjectName("projectSettingsHeader")
        frame.setFixedHeight(65)
        head = QHBoxLayout(frame)
        head.setContentsMargins(24, 14, 24, 14)
        head.setSpacing(12)
        icon_label = QLabel()
        icon_label.setPixmap(material_icon("event_repeat", "role:primary").pixmap(24, 24))
        self.header_icon = icon_label
        head.addWidget(icon_label)
        title = QLabel(tr("Настройки проекта"))
        title.setProperty("text_style", "title")
        self.header_title = title
        label, host_name = project_names(self.host)
        meta = QLabel(
            joined(
                " · ",
                [part for part in (label, host_name) if part],
            )
        )
        meta.setProperty("text_style", "meta")
        meta.setProperty("canvas_context", True)
        head.addWidget(title)
        head.addWidget(meta)
        self.context = meta
        head.addStretch(1)
        self.next_pill = QFrame()
        self.next_pill.setProperty("canvas_next", True)
        self.next_pill.setFixedHeight(24)
        pill = QHBoxLayout(self.next_pill)
        pill.setContentsMargins(8, 0, 8, 0)
        pill.setSpacing(6)
        pill.addWidget(MaterialIconLabel("schedule", 18, color="role:on_selected"))
        self.next_badge = QLabel()
        pill.addWidget(self.next_badge)
        self.next_pill.hide()
        head.addWidget(self.next_pill)
        self.cancel = QPushButton(tr("Отмена"))
        self.cancel.setProperty("size", "lg")
        self.cancel.setFixedSize(76, 36)
        self.cancel.clicked.connect(lambda: self._edit_action("reload"))
        self.save = QPushButton(tr("Сохранить"))
        self.save.setProperty("role", "primary")
        self.save.setProperty("size", "lg")
        self.save.setFixedSize(92, 36)
        self.save.setIconSize(QSize(20, 20))
        self.save.setIcon(material_icon("save", "role:on_primary"))
        self.save.clicked.connect(lambda: self._edit_action("save"))
        head.addWidget(self.cancel)
        head.addWidget(self.save)
        return frame

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self.context.setVisible(self.width() >= 1000)
        self.header_title.setProperty("compact", self.width() < 1000)
        self.header_title.style().unpolish(self.header_title)
        self.header_title.style().polish(self.header_title)
        compact = self.width() < 1000
        self.header_frame.setFixedHeight(61 if compact else 65)
        margin = 16 if compact else 24
        self.header_frame.layout().setContentsMargins(
            margin, 12 if compact else 14, margin, 12 if compact else 14
        )
        self.pages.setContentsMargins(margin, 16 if compact else 20, margin, 16 if compact else 20)
        self.nav.setTabText(3, "Crawl" if compact else "Crawl & User-Agent")
        self.nav.setTabText(4, "" if compact else "Export")
        self.nav.names[4] = "more_horiz" if compact else "ios_share"
        self.nav.setTabToolTip(4, "Export")
        self.header_icon.setVisible(not compact)
        self.header_icon.setPixmap(
            material_icon(self.nav.names[self.nav.currentIndex()], "role:primary").pixmap(20, 20)
        )
        self._sync_actions()

    def _main_page(self):
        label, host_name = project_names(self.host)
        page = QWidget()
        layout = QVBoxLayout(page)
        layout.setContentsMargins(20, 16, 20, 16)
        layout.setSpacing(16)
        general = _card("Основное")
        general.layout().addWidget(_field("Название проекта", _value(label)))
        general.layout().addWidget(_field("Хост сайта", _value(host_name)))
        general.layout().addWidget(
            _field(
                "Профиль скана по умолчанию",
                waiting_badge(941, "Именованные профили скана и профиль по умолчанию в ядре"),
            )
        )
        general.layout().addWidget(
            _field("Папка проекта", _value(self.host.project_directory, mono=True))
        )
        layout.addWidget(general)
        goals = _card("Цели проекта")
        goals.layout().addWidget(
            waiting_badge(976, "Цели проекта ядро пока не хранит"), 0, Qt.AlignLeft
        )
        layout.addWidget(goals)
        members = _card("Участники")
        members.layout().addWidget(_text(tr("Проект локальный: участников нет.")))
        layout.addWidget(members)
        danger = _card("Опасная зона")
        danger.layout().addWidget(
            waiting_badge(976, "Удаление проекта из приложения, с сохранением файлов в Корзине"),
            0,
            Qt.AlignLeft,
        )
        delete = QPushButton(tr("Удалить проект"))
        delete.setEnabled(False)
        delete.setToolTip(tr(UNAVAILABLE))
        danger.layout().addWidget(delete, 0, Qt.AlignLeft)
        layout.addWidget(danger)
        layout.addStretch(1)
        return _scrolled(page)

    def _footer(self):
        footer = QFrame()
        footer.setObjectName("dialogFooter")
        footer_layout = QHBoxLayout(footer)
        footer_layout.setContentsMargins(24, 12, 24, 12)
        hint = QLabel(
            tr(
                "Ключи и токены задаются в настройках приложения · здесь их значения не показываются"
            )
        )
        hint.setProperty("text_style", "meta")
        hint.setWordWrap(True)
        footer_layout.addWidget(hint, 1)
        cancel = QPushButton(tr("Закрыть"))
        cancel.clicked.connect(self.reject)
        footer_layout.addWidget(cancel)
        return footer


def _scrolled(widget):
    scroll = QScrollArea()
    scroll.setWidgetResizable(True)
    scroll.setFrameShape(QFrame.NoFrame)
    holder = QWidget()
    holder_layout = QVBoxLayout(holder)
    holder_layout.setContentsMargins(24, 4, 24, 16)
    holder_layout.addWidget(widget)
    scroll.setWidget(holder)
    return scroll


def _card(title):
    card = QFrame()
    card.setProperty("card", "panel")
    layout = QVBoxLayout(card)
    layout.setContentsMargins(16, 14, 16, 14)
    layout.setSpacing(10)
    caption = QLabel(tr(title))
    caption.setProperty("text_style", "section")
    layout.addWidget(caption)
    return card


def _field(caption, value):
    field = QWidget()
    layout = QVBoxLayout(field)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(4)
    name = QLabel(tr(caption))
    name.setProperty("text_style", "meta")
    layout.addWidget(name)
    layout.addWidget(value, 0, Qt.AlignLeft)
    return field


def _value(text, mono=False):
    if not text:
        label = QLabel(tr("Нет данных"))
        label.setProperty("na", True)
        return label
    label = QLabel(text)
    label.setTextInteractionFlags(Qt.TextSelectableByMouse)
    if mono:
        label.setProperty("text_style", "mono")
    return label


def _text(text):
    label = QLabel(text)
    label.setProperty("text_style", "meta")
    label.setWordWrap(True)
    return label


class ProjectSettingsDialog(QDialog):
    """Standalone host for the same project-settings page."""

    def __init__(self, host, parent=None):
        super().__init__(parent)
        self.setWindowTitle(tr("Настройки проекта"))
        self.resize(1180, 800)
        self.setMinimumSize(700, 480)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        self.content = ProjectSettingsPage(host)
        self.nav, self.pages = self.content.nav, self.content.pages
        self.page = self.content.page
        layout.addWidget(self.content)


def open_project_settings(window):
    """Open project settings in the existing application workspace."""
    window.show_screen("project_settings")
