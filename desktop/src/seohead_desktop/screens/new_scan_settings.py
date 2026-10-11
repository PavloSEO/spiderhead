"""«Настройки скана»: the eight-page settings window opened from the «Новый скан» dialog (sheets Sc*).

It edits a copy of the draft; «Применить» copies the copy back, «Отмена» drops it.
"""

from __future__ import annotations

from PyQt5.QtCore import QSize, Qt
from PyQt5.QtWidgets import (
    QDialog,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QScrollArea,
    QSizePolicy,
    QStackedWidget,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import i18n
from ..i18n import tr, trf
from ..ui.icons import MaterialIconLabel
from ..ui.icons import material_icon as icon
from ..ui.kit import waiting_badge
from ..ui.presentation import ElidedLabel
from .new_scan_draft import ISSUE_PROFILES, grouped
from .new_scan_pages import PAGES


class ScanSettingsDialog(QDialog):
    def __init__(self, draft, host, parent=None, page="speed"):
        super().__init__(parent)
        self.original = draft
        self.draft = draft.clone()
        self.host = host
        self.setWindowTitle("Настройки скана")
        self.setObjectName("scanSettingsDialog")
        self.setModal(True)
        size = host.size()
        self.resize(min(1380, max(720, size.width() - 40)), min(840, max(600, size.height() - 40)))
        self.setMinimumSize(640, 520)
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        root.addWidget(self._header())
        body = QHBoxLayout()
        body.setSpacing(0)
        body.addWidget(self._nav())
        self.stack = QStackedWidget()
        self.pages = {}
        for page_id, title, _glyph, build in PAGES:
            widget = build(self.draft, host)
            scroll = QScrollArea()
            scroll.setWidgetResizable(True)
            scroll.setFrameShape(QFrame.NoFrame)
            scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
            wrap = QWidget()
            wrap_layout = QVBoxLayout(wrap)
            wrap_layout.setContentsMargins(28, 20, 28, 20)
            wrap_layout.addWidget(widget)
            scroll.setWidget(wrap)
            self.stack.addWidget(scroll)
            self.pages[page_id] = (scroll, widget)
        body.addWidget(self.stack, 1)
        self.aside = self._aside()
        body.addWidget(self.aside)
        root.addLayout(body, 1)
        root.addWidget(self._footer())
        self.draft.changed.connect(self.refresh)
        self.show_page(page)
        i18n.retranslate(self)

    def _header(self):
        bar = QFrame()
        bar.setObjectName("scanHeader")
        bar.setFixedHeight(56)
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(20, 0, 12, 0)
        layout.setSpacing(12)
        layout.addWidget(MaterialIconLabel("manage_search", 24, color="role:primary"))
        title = QLabel(tr("Настройки скана"))
        title.setProperty("text_style", "dialog")
        layout.addWidget(title)
        self.header_meta = ElidedLabel(trf("{host} · новый скан", host=self.draft.host or tr("Нет данных")))
        self.header_meta.setProperty("text_style", "meta")
        layout.addWidget(self.header_meta, 1)
        self.profile_button = QPushButton()
        self.profile_button.setObjectName("scanSettingsProfile")
        self.profile_button.setIcon(icon("expand_more"))
        self.profile_button.setMinimumWidth(150)
        self.profile_button.setSizePolicy(QSizePolicy.Preferred, QSizePolicy.Fixed)
        self.profile_button.setToolTip(tr("Все профили…"))
        self.profile_button.clicked.connect(lambda: self.show_page("profiles"))
        layout.addWidget(self.profile_button)
        save = QPushButton(tr("Сохранить как профиль"))
        save.setObjectName("scanSettingsSaveProfile")
        save.setIcon(icon("bookmark_add"))
        save.setAccessibleName(tr("Сохранить как профиль"))
        save.setEnabled(False)
        save.setToolTip(f"{tr('Недоступно в этой версии ядра')}: {tr('ядро не хранит именованные профили скана')}")
        self.save_profile = save
        self._save_text = tr("Сохранить как профиль")
        layout.addWidget(save)
        layout.addWidget(waiting_badge(ISSUE_PROFILES))
        close = QToolButton()
        close.setProperty("role", "icon")
        close.setIcon(icon("close"))
        close.setFocusPolicy(Qt.NoFocus)
        close.setAccessibleName(tr("Закрыть"))
        close.setToolTip(tr("Закрыть без применения"))
        close.clicked.connect(self.reject)
        layout.addWidget(close)
        return bar

    def _nav(self):
        nav = QFrame()
        nav.setObjectName("settingsNav")
        nav.setFixedWidth(232)
        self.nav_frame = nav
        layout = QVBoxLayout(nav)
        layout.setContentsMargins(10, 14, 10, 12)
        layout.setSpacing(2)
        self.nav = {}
        for page_id, title, glyph, _build in PAGES:
            button = QToolButton()
            button.setObjectName("settingsNavItem")
            button.setText(tr(title))
            button.setIcon(icon(glyph))
            button.setIconSize(QSize(20, 20))
            button.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
            button.setCheckable(True)
            button.setAutoExclusive(True)
            button.setAccessibleName(tr(title))
            button.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
            button.clicked.connect(lambda _c, p=page_id: self.show_page(p))
            layout.addWidget(button)
            self.nav[page_id] = button
        layout.addStretch(1)
        return nav

    def _aside(self):
        aside = QFrame()
        aside.setObjectName("scanAside")
        aside.setFixedWidth(300)
        layout = QVBoxLayout(aside)
        layout.setContentsMargins(18, 18, 18, 14)
        layout.setSpacing(12)
        title = QLabel(tr("Сводка конфигурации"))
        title.setProperty("text_style", "control")
        layout.addWidget(title)
        card = QFrame()
        card.setProperty("note", "info")
        box = QVBoxLayout(card)
        box.setContentsMargins(12, 10, 12, 10)
        caption = QLabel(tr("Границы нагрузки"))
        caption.setProperty("text_style", "meta")
        self.load = QLabel()
        self.load.setObjectName("scanLoadBounds")
        self.load.setWordWrap(True)
        later = QLabel(tr("Длительность и размер: оценка недоступна в этой версии ядра"))
        later.setProperty("text_style", "meta")
        later.setWordWrap(True)
        for widget in (caption, self.load, later):
            box.addWidget(widget)
        layout.addWidget(card)
        self.summary = QGridLayout()
        self.summary.setHorizontalSpacing(10)
        self.summary.setVerticalSpacing(4)
        self.summary.setColumnStretch(1, 1)
        layout.addLayout(self.summary)
        layout.addStretch(1)
        self.changed_label = QLabel()
        self.changed_label.setObjectName("scanChangedCount")
        self.changed_label.setProperty("text_style", "meta")
        self.changed_label.setWordWrap(True)
        layout.addWidget(self.changed_label)
        self.command = QLabel()
        self.command.setWordWrap(True)
        self.command.setObjectName("scanCommandPreview")
        self.command.setProperty("text_style", "mono")
        layout.addWidget(self.command)
        return aside

    def _footer(self):
        bar = QFrame()
        bar.setObjectName("dialogFooter")
        layout = QHBoxLayout(bar)
        bar.setFixedHeight(60)
        layout.setContentsMargins(20, 0, 20, 0)
        layout.setSpacing(8)
        reset = QPushButton(tr("Сбросить"))
        reset.setObjectName("scanSettingsReset")
        reset.setProperty("role", "text")
        reset.setIcon(icon("restart_alt"))
        reset.setToolTip(tr("Вернуть умолчания: профиль проекта, затем настройки приложения, затем ядро"))
        reset.clicked.connect(lambda: self.draft.reset())
        layout.addWidget(reset)
        hint = QLabel(tr("Параметры уходят в команду запуска; ядро проверит их ещё раз"))
        hint.setProperty("text_style", "meta")
        layout.addWidget(hint, 1)
        cancel = QPushButton(tr("Отмена"))
        cancel.setObjectName("scanSettingsCancel")
        cancel.setProperty("role", "text")
        cancel.setProperty("size", "lg")
        cancel.clicked.connect(self.reject)
        self.apply = QPushButton(tr("Применить"))
        self.apply.setObjectName("scanSettingsApply")
        self.apply.setProperty("role", "primary")
        self.apply.setProperty("size", "lg")
        self.apply.clicked.connect(self._apply)
        layout.addWidget(cancel)
        layout.addWidget(self.apply)
        return bar

    def resizeEvent(self, event):
        super().resizeEvent(event)
        if hasattr(self, "aside"):
            self.aside.setVisible(self.width() >= 1000)
            # Below 1000 px the header keeps the title, the profile and the actions; the host line and the save label go.
            self.header_meta.setVisible(self.width() >= 1000)
            self.save_profile.setText(self._save_text if self.width() >= 1000 else "")
            self._elide_profile()
            wide = self.width() >= 900
            for _scroll, page in self.pages.values():
                page.responsive(self.width() >= 1100)
            self.nav_frame.setFixedWidth(232 if wide else 60)
            for page_id, title, _glyph, _build in PAGES:
                button = self.nav[page_id]
                button.setToolButtonStyle(Qt.ToolButtonTextBesideIcon if wide else Qt.ToolButtonIconOnly)
                button.setToolTip("" if wide else tr(title))

    def _elide_profile(self):
        """The profile name elides to the room the header leaves it."""
        full = getattr(self, "_profile_full", "")
        room = max(60, self.profile_button.width() - 40)
        self.profile_button.setText(self.profile_button.fontMetrics().elidedText(full, Qt.ElideRight, room))

    def show_page(self, page_id):
        if page_id not in self.pages:
            page_id = PAGES[0][0]
        self.stack.setCurrentWidget(self.pages[page_id][0])
        self.nav[page_id].setChecked(True)
        self.refresh()

    def _apply(self):
        self.original.adopt(self.draft)
        self.accept()

    def refresh(self):
        draft = self.draft
        problems = draft.problems()
        for _scroll, page in self.pages.values():
            page.sync(problems)
        chain = tr("профиль проекта") if draft.project_layer else tr("настройки приложения")
        self._profile_full = trf("Профиль: {name}", name=chain)
        self._elide_profile()
        self.apply.setEnabled(not any(k in problems for k in problems if k not in ("source", "list", "sitemap")))
        rate = draft.rps()
        limit = trf("лимит {n} URL", n=grouped(draft.url_limit)) if draft.limit_enabled else tr("без лимита URL")
        self.load.setText(trf("{rate} · потоков {threads} · {limit}", rate=trf("до {n} запросов/с", n=f"{rate:g}".replace(".", ",")) if rate else tr("скорость не задана"),
                              threads=draft.value("speed.concurrency", "—"), limit=limit))
        from .new_scan import summary_rows

        while self.summary.count():
            item = self.summary.takeAt(0)
            if item.widget():
                item.widget().deleteLater()
        for index, (key, value) in enumerate(summary_rows(draft)):
            name = QLabel(tr(key))
            name.setProperty("text_style", "meta")
            shown = QLabel(tr("Нет данных") if value is None else str(value))
            shown.setToolTip(shown.text())
            shown.setWordWrap(True)
            self.summary.addWidget(name, index, 0)
            self.summary.addWidget(shown, index, 1)
        edited = draft.edited_paths()
        self.changed_label.setText(trf("Изменено полей: {n}", n=len(edited)))
        self.changed_label.setToolTip(", ".join(edited))
        command = draft.command_text()
        self.command.setText(command.split(" --input")[0] + " …" if command else tr("Команда появится, когда поля заполнены верно"))
        if command:
            self.command.setToolTip(command)
