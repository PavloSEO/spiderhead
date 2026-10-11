"""«Новый скан»: the modal window of the design (ScanDialog / ScanList) and its settings window (Sc*).

The dialog shows a plan first and starts nothing by itself: «Запустить» is a separate button that stays disabled until
the volume is confirmed and every field is valid. It reads state from the window (project, core descriptor, settings),
asks it for the project's crawl policy and hands the finished plan to ``host.launch_scan``; it never calls the core,
the network or a scan itself.
"""

from __future__ import annotations

from pathlib import Path

from PyQt5.QtCore import Qt, QTimer
from PyQt5.QtWidgets import (
    QCheckBox,
    QDialog,
    QFileDialog,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPlainTextEdit,
    QPushButton,
    QScrollArea,
    QSizePolicy,
    QStackedWidget,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import i18n, theming
from ..i18n import tr, trf
from ..ui.controls import Note, Segmented, Switch
from ..ui.icons import MaterialIconLabel
from ..ui.icons import material_icon as icon
from ..ui.kit import StatePanel, waiting_badge
from ..ui.presentation import ElidedLabel
from .new_scan_draft import (
    ISSUE_ESTIMATE,
    ISSUE_PROFILES,
    ISSUE_URL_QUERY,
    LIST_FILE_CAP,
    PlanError,
    ScanDraft,
    grouped,
    list_from_file_text,
    request_project_policy,
)
from .new_scan_pages import ChoiceCard, FormRow, HelpIcon, Stepper, flow, number_edit

SOURCE_CARDS = (
    ("site", "travel_explore", "Сайт целиком", "Обход по ссылкам со стартового URL проекта"),
    ("sitemap", "account_tree", "По sitemap", "Только URL из sitemap.xml, без обхода ссылок"),
    ("list", "format_list_bulleted", "Список URL", "Вставить или загрузить свой список адресов"),
    ("sf", "bug_report", "Screaming Frog", "Недоступно в этой сборке"),
)
SOURCE_NAMES = {key: name for key, _icon, name, _text in SOURCE_CARDS}
ASIDE_WIDTH = 300  # the «Что произойдёт» summary column; the options column takes the rest
FIELD_TITLES = {
    "rps": "Запросов/с", "threads": "Потоки", "limit": "Лимит URL", "depth": "Глубина", "requests": "Лимит запросов",
    "minutes": "Время скана", "sitemap": "Адрес sitemap", "robots": "robots.txt", "min_delay": "Минимальная пауза",
    "max_delay": "Максимальная пауза", "timeouts": "Тайм-ауты подряд", "script_timeout": "Время JavaScript",
    "body_mb": "Максимум на один ответ", "free_gb": "Свободное место", "viewport": "Окно просмотра", "list": "Список URL",
}
# plan rows the core cannot measure before a run: the value is the neutral «unavailable» badge, never a number
UNMEASURED = {"duration": "Оценка длительности скана до запуска", "disk": "Оценка занятого места до запуска"}
ROBOTS_NAMES = {"respect": "соблюдать", "report_only": "только отчёт", "ignore": "игнорировать"}


def rps_label(draft):
    rate = draft.rps()
    return "—" if rate is None else f"{float(f'{rate:.3g}'):g}".replace(".", ",")


def hbox(*items, spacing=8, stretch=()):
    """A plain row widget: widgets in order, ``None`` is a stretch."""
    box = QWidget()
    layout = QHBoxLayout(box)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(spacing)
    for item in items:
        layout.addStretch(1) if item is None else layout.addWidget(item, 1 if item in stretch else 0)
    return box


def summary_rows(draft):
    """(caption, value) pairs of the configuration summary; every value is a setting, not an estimate."""
    v = draft.values
    depth = v.get("limits.max_depth")
    seconds = v.get("limits.max_crawl_seconds") or 0
    rows = [
        ("Старт", draft.target or None),
        ("Запросов/с", trf("{n} на хост", n=rps_label(draft)) if draft.rps() else None),
        ("Потоков", v.get("speed.concurrency")),
        ("Лимит URL", grouped(draft.url_limit) if draft.limit_enabled else tr("без лимита")),
        ("Глубина", None if depth is None else tr("без ограничения") if depth == -1 else depth),
        ("Время", trf("{n} мин", n=grouped(round(seconds / 60, 1) if seconds % 60 else seconds // 60)) if seconds else tr("без лимита")),
        ("robots.txt", tr(ROBOTS_NAMES.get(v.get("robots.policy"), "")) or None),
        ("JS", tr("всегда") if v.get("rendering.mode") == "js" else tr("выкл")),
        ("Тело", tr("не хранится") if v.get("storage.body_mode") == "off" else tr("сохраняется")),
        ("Экстракторов", tr("недоступно в этой версии ядра")),
    ]
    agent = v.get("http.user_agent")
    if agent:
        rows.append(("User-Agent", agent))
    if v.get("scope.internal") == "registrable_domain":
        rows.append(("Хосты", tr("весь домен")))
    return rows


class NewScanDialog(QDialog):
    """The dialog. ``plan`` is set only when the user pressed «Запустить» on a valid, confirmed draft."""

    def __init__(self, host, parent=None):
        super().__init__(parent or host)
        self.host = host
        self.plan = None
        self.draft = None
        self._busy = False
        self._confirmed_signature = None
        self._list_timer = QTimer(self)
        self._list_timer.setSingleShot(True)
        self._list_timer.timeout.connect(lambda: self.draft.emit_changed())
        self.setWindowTitle("Новый скан")
        self.setModal(True)
        self.setObjectName("newScanDialog")
        area = parent.size() if parent is not None else host.size()
        self.resize(min(960, max(720, area.width() - 40)), min(680, max(600, area.height() - 40)))
        self.setMinimumSize(640, 560)
        self.quick_requested = False
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        root.addWidget(self._header())
        self.stack = QStackedWidget()
        root.addWidget(self.stack, 1)
        self.state = StatePanel("loading", "Читаем настройки ядра…", "Параметры скана берутся из crawl-describe-settings.")
        self.retry = QPushButton(tr("Повторить загрузку настроек"))
        self.retry.setObjectName("scanRetryDescriptor")
        self.retry.clicked.connect(self._retry)
        holder = QWidget()
        holder_layout = QVBoxLayout(holder)
        holder_layout.addStretch(1)
        holder_layout.addWidget(self.state)
        holder_layout.addWidget(self.retry, 0, Qt.AlignHCenter)
        holder_layout.addStretch(1)
        self.stack.addWidget(holder)
        self.form = QWidget()
        self.stack.addWidget(self.form)
        root.addWidget(self._footer())
        host.crawl_descriptor_changed.connect(self._descriptor_changed)
        self.finished.connect(self._release)
        self._descriptor_changed()
        if host.crawl_descriptor is None and hasattr(host, "load_crawl_descriptor"):
            host.load_crawl_descriptor()
        i18n.retranslate(self)

    # ---- frame ---------------------------------------------------------------------------------------------------
    def _header(self):
        bar = QFrame()
        bar.setObjectName("scanHeader")
        bar.setFixedHeight(48)
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(20, 0, 8, 0)
        layout.setSpacing(10)
        layout.addWidget(MaterialIconLabel("play_circle", 20, color="role:primary"))
        title = QLabel(tr("Новый скан"))
        title.setProperty("text_style", "section")
        self.subtitle = ElidedLabel()
        self.subtitle.setProperty("text_style", "meta")
        layout.addWidget(title)
        layout.addWidget(self.subtitle, 1)
        quick = QPushButton(tr("Быстрый запуск"))
        quick.setObjectName("scanQuickLaunch")
        quick.setProperty("role", "text")
        quick.setProperty("size", "sm")
        quick.setIcon(icon("bolt"))
        quick.setToolTip(tr("Запуск одной строкой в шапке"))
        quick.clicked.connect(self._quick_launch)
        layout.addWidget(quick)
        close = QToolButton()
        close.setProperty("role", "icon")
        close.setIcon(icon("close"))
        close.setFocusPolicy(Qt.NoFocus)
        close.setAccessibleName(tr("Закрыть без запуска"))
        close.setToolTip(tr("Закрыть без запуска"))
        close.clicked.connect(self.reject)
        layout.addWidget(close)
        label = ((getattr(self.host, "project_result", None) or {}).get("project") or {}).get("site") or {}
        self.subtitle.setText(" · ".join(part for part in (label.get("label"), label.get("host")) if part))
        return bar

    def _quick_launch(self):
        """Close this window and show the one-line launcher instead (the draft is kept per project)."""
        self.quick_requested = True
        self.reject()

    def _footer(self):
        bar = QFrame()
        bar.setObjectName("dialogFooter")
        bar.setFixedHeight(56)
        layout = QHBoxLayout(bar)
        layout.setContentsMargins(20, 0, 20, 0)
        layout.setSpacing(8)
        profile = QPushButton(tr("Сохранить как профиль"))
        profile.setObjectName("scanSaveProfile")
        profile.setIcon(icon("bookmark_add"))
        profile.setEnabled(False)
        profile.setToolTip(f"{tr('Недоступно в этой версии ядра')}: {tr('ядро не хранит именованные профили скана')}")
        layout.addWidget(profile)
        layout.addWidget(waiting_badge(ISSUE_PROFILES))
        self.problem_icon = MaterialIconLabel("error", 18, color="role:error")
        self.problem = QLabel()  # wraps to two lines instead of eliding the reason
        self.problem.setWordWrap(True)
        self.problem.setObjectName("scanValidationFeedback")
        self.problem.setProperty("field_error", True)
        self.problem.setProperty("text_style", "meta")
        layout.addWidget(self.problem_icon)
        layout.addWidget(self.problem, 1)
        cancel = QPushButton(tr("Отмена"))
        cancel.setProperty("role", "text")
        cancel.setProperty("size", "lg")
        cancel.clicked.connect(self.reject)
        self.start = QPushButton(tr("Запустить"))
        self.start.setObjectName("scanStartButton")
        self.start.setProperty("role", "primary")
        self.start.setProperty("size", "lg")
        self.start.setIcon(icon("play_arrow", theming.roles()["on_primary"]))
        self.start.clicked.connect(self._accept_plan)
        layout.addWidget(cancel)
        layout.addWidget(self.start)
        return bar

    # ---- descriptor lifecycle ------------------------------------------------------------------------------------
    def _descriptor_changed(self):
        host = self.host
        if host.crawl_descriptor is not None:
            if self.draft is None:
                self._build_form()
            self.stack.setCurrentIndex(1)
        else:
            error = getattr(host, "_crawl_descriptor_error", None)
            layout = self.stack.widget(0).layout()
            layout.removeWidget(self.state)  # otherwise the replaced panel stays visible until it is deleted
            self.state.deleteLater()
            kind, title, text = (("error", "Настройки ядра недоступны", error) if error
                                 else ("loading", "Читаем настройки ядра…", "Параметры скана берутся из crawl-describe-settings."))
            self.state = StatePanel(kind, title, text or "")
            layout.insertWidget(1, self.state)
            i18n.retranslate(self.state)
            self.stack.setCurrentIndex(0)
        self.retry.setVisible(host.crawl_descriptor is None and bool(getattr(host, "_crawl_descriptor_error", None)))
        self._refresh()

    def _retry(self):
        self.host.load_crawl_descriptor()
        self._descriptor_changed()

    def _release(self, *_args):
        try:
            self.host.crawl_descriptor_changed.disconnect(self._descriptor_changed)
        except (TypeError, RuntimeError):
            pass

    # ---- form ----------------------------------------------------------------------------------------------------
    def _build_form(self):
        host = self.host
        site = ((getattr(host, "project_result", None) or {}).get("project") or {}).get("site") or {}
        self.draft = draft = ScanDraft(host.crawl_descriptor, target=site.get("target") or "", host=site.get("host") or "",
                                       project_directory=host.project_directory or "", prefs=getattr(host, "prefs", None), parent=self)
        key = host.note_project_key() if hasattr(host, "note_project_key") else None
        draft.restore(getattr(host, "_scan_drafts", {}).get(key))
        layout = QHBoxLayout(self.form)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        left = QWidget()
        column = QVBoxLayout(left)
        column.setContentsMargins(20, 14, 20, 14)
        column.setSpacing(12)
        column.addWidget(self._sources())
        column.addWidget(self._source_blocks())
        rule = QFrame()
        rule.setFixedHeight(1)
        rule.setProperty("rule", True)
        column.addWidget(rule)
        column.addWidget(self._options())
        column.addStretch(1)
        self.settings_link = QPushButton()
        self.settings_link.setObjectName("scanOpenSettings")
        self.settings_link.setProperty("role", "text")
        self.settings_link.setIcon(icon("tune"))
        self.settings_link.clicked.connect(lambda: self._open_settings("speed"))
        column.addWidget(self.settings_link, 0, Qt.AlignLeft)
        scroll.setWidget(left)
        layout.addWidget(scroll, 1)
        layout.addWidget(self._aside())
        draft.changed.connect(self._refresh)
        request_project_policy(self.host, self.draft)
        self._refresh()
        self.cards[draft.source if draft.source != "sf" else "site"].setFocus()

    def _sources(self):
        box = QWidget()
        layout = QVBoxLayout(box)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(6)
        self.cards = {}
        self.card_grid = QGridLayout()
        self.card_grid.setSpacing(8)
        for key, glyph, name, text in SOURCE_CARDS:
            card = ChoiceCard(key, glyph, name, text, "scanSource_" + key, compact=True)
            if key == "sf":
                card.setEnabled(False)
                card.setToolTip(tr("Недоступно в этой сборке"))
            card.clicked.connect(lambda _c, k=key: self._pick_source(k))
            self.cards[key] = card
        layout.addLayout(self.card_grid)
        self._layout_cards(4)
        self.source_text = ElidedLabel()
        self.source_text.setObjectName("scanSourceText")
        self.source_text.setProperty("text_style", "meta")
        layout.addWidget(self.source_text)
        return box

    def _layout_cards(self, columns):
        for index, card in enumerate(self.cards.values()):
            self.card_grid.addWidget(card, index // columns, index % columns)
        for column in range(4):
            self.card_grid.setColumnStretch(column, 1 if column < columns else 0)
        self._card_columns = columns

    def _layout_options(self, columns):
        normal = [item for item in self.option_items if item is not self.option_wide]
        for index, item in enumerate(normal):
            self.option_grid.addWidget(item, index // columns, index % columns)
        self.option_grid.addWidget(self.option_wide, -(-len(normal) // columns), 0, 1, columns)
        self._option_columns = columns

    def resizeEvent(self, event):
        super().resizeEvent(event)
        # columns follow the left pane (dialog minus the fixed aside and the 20 px gutters), not the whole dialog
        pane = self.width() - ASIDE_WIDTH - 40
        if getattr(self, "cards", None):
            columns = 4 if pane >= 760 else 2
            if columns != self._card_columns:
                self._layout_cards(columns)
        if getattr(self, "option_items", None):
            columns = 2 if pane >= 640 else 1
            if columns != self._option_columns:
                self._layout_options(columns)

    def _source_blocks(self):
        draft = self.draft
        box = QWidget()
        layout = QVBoxLayout(box)
        layout.setContentsMargins(0, 0, 0, 0)
        # site
        start = QLineEdit(draft.target)
        start.setObjectName("scanStartUrl")
        start.setReadOnly(True)
        start.setAccessibleName(tr("Стартовый URL"))
        start.setProperty("mono", True)
        badge = QLabel(tr("Сайт проекта"))
        badge.setProperty("badge", "ok")
        self.site_block = FormRow(tr("Стартовый URL"), hbox(start, badge, stretch=(start,)), tr("Адрес берётся из проекта"), field=start)
        layout.addWidget(self.site_block)
        # sitemap
        self.sitemap_edit = QLineEdit(draft.sitemap_url)
        self.sitemap_edit.setObjectName("scanSitemapUrl")
        self.sitemap_edit.setAccessibleName(tr("Адрес sitemap"))
        self.sitemap_edit.setPlaceholderText("https://example.test/sitemap.xml")
        self.sitemap_edit.setProperty("mono", True)
        self.sitemap_edit.textEdited.connect(self._sitemap_typed)
        self.sitemap_box = FormRow(tr("Адрес sitemap"), hbox(self.sitemap_edit, waiting_badge(ISSUE_ESTIMATE, "Число URL в sitemap до запуска скана"), stretch=(self.sitemap_edit,)),
                                   f"{tr('Состав sitemap покажет скан')} · {tr('предпросмотр без запуска')}: {tr('Недоступно в этой версии ядра')}", field=self.sitemap_edit)
        layout.addWidget(self.sitemap_box)
        # list
        self.list_block = QWidget()
        listing = QVBoxLayout(self.list_block)
        listing.setContentsMargins(0, 0, 0, 0)
        listing.setSpacing(6)
        title = QLabel(tr("Список URL · по одному в строке"))
        title.setWordWrap(True)
        title.setProperty("text_style", "control")
        from_file = QPushButton(tr("Из файла .txt / .csv"))
        from_file.setObjectName("scanListFile")
        from_file.setProperty("size", "sm")
        from_file.setIcon(icon("upload_file"))
        from_file.clicked.connect(self._list_from_file)
        paste = QPushButton(tr("Вставить"))
        paste.setObjectName("scanListPaste")
        paste.setProperty("size", "sm")
        paste.setIcon(icon("content_paste"))
        paste.clicked.connect(self._list_paste)
        listing.addWidget(flow(title, from_file, paste))
        self.list_edit = QPlainTextEdit(draft.list_text)
        self.list_edit.setObjectName("scanListText")
        self.list_edit.setAccessibleName(tr("Список URL"))
        self.list_edit.setPlaceholderText("https://site.ru/catalog/")
        self.list_edit.setMinimumHeight(76)
        self.list_edit.setMaximumHeight(96)
        self.list_edit.textChanged.connect(self._list_typed)
        listing.addWidget(self.list_edit)
        self.counter_labels = {}
        parts = []
        for name, kind in (("ready", "ok"), ("added", "info"), ("duplicates", "mut"), ("foreign", "warn"), ("invalid", "err")):
            label = QLabel()
            label.setObjectName("count_" + name)
            label.setProperty("badge", kind)
            self.counter_labels[name] = label
            parts.append(label)
        four = QPushButton(tr("Взять 4xx из скана"))
        four.setProperty("role", "text")
        four.setProperty("size", "sm")
        four.setEnabled(False)
        four.setToolTip(tr('Недоступно в этой версии ядра'))
        listing.addWidget(flow(*parts, four, waiting_badge(ISSUE_URL_QUERY)))
        self.list_note = Note("warn", tr("Запуск списка URL из приложения недоступен в этой версии ядра."))
        self.list_note.setToolTip(tr("Скан списка не попадает в наблюдение проекта: список можно проверить, но не запустить."))
        listing.addWidget(self.list_note)
        layout.addWidget(self.list_block)
        # Screaming Frog
        self.sf_block = Note("info", tr("Недоступно в этой сборке."), tr("Импорт и запуск Screaming Frog не подключены."))
        layout.addWidget(self.sf_block)
        return box

    def _options(self):
        draft = self.draft
        box = QWidget()
        grid = QGridLayout(box)
        grid.setContentsMargins(0, 0, 0, 0)
        grid.setHorizontalSpacing(16)
        grid.setVerticalSpacing(10)
        grid.setAlignment(Qt.AlignTop)
        # mode
        self.mode = Segmented([("raw", tr("Исходный HTML")), ("js", tr("С JS"))], draft.value("rendering.mode", "raw"), tr("Режим загрузки"))
        self.mode.setObjectName("scanRenderingMode")
        self.mode.setProperty("dense", True)
        self.mode.changed.connect(lambda value: draft.set_value("rendering.mode", value))
        self.mode_row = FormRow(tr("Режим"), self.mode, " ")
        # profile
        self.profile_line = QLineEdit()
        self.profile_line.setReadOnly(True)
        self.profile_line.setAccessibleName(tr("Профиль настроек"))
        all_profiles = QToolButton()
        all_profiles.setObjectName("scanAllProfiles")
        all_profiles.setProperty("role", "icon")
        all_profiles.setIcon(icon("tune"))
        all_profiles.setAccessibleName(tr("Все профили…"))
        all_profiles.setToolTip(tr("Все профили…"))
        all_profiles.clicked.connect(lambda: self._open_settings("profiles"))
        self.profile_row = FormRow(tr("Профиль"), hbox(self.profile_line, all_profiles, spacing=4, stretch=(self.profile_line,)), " ", field=self.profile_line)
        # speed
        self.rps_edit, self._sync_rps = number_edit(draft, "rps", tr("Запросов в секунду"), 0, "scanRequestRate")
        self.rps_box = FormRow(tr("Запросов/с"), Stepper(self.rps_edit, lambda: self._step_rps(-1), lambda: self._step_rps(1)),
                               tr("Бережно для боевого сайта: не больше 2"), field=self.rps_edit)
        self.threads_edit, self._sync_threads = number_edit(draft, "threads", tr("Потоки"), 0, "scanConcurrency")
        self.threads_box = FormRow(tr("Потоки"), Stepper(self.threads_edit, lambda: self._step_threads(-1), lambda: self._step_threads(1)),
                                   tr("Параллельных соединений в общем пуле скана"), field=self.threads_edit)
        # limits
        self.limit_switch = Switch(tr("Ограничить число URL"), draft.limit_enabled)
        self.limit_switch.setObjectName("scanLimitEnabled")
        self.limit_switch.toggled.connect(draft.set_limit_enabled)
        self.limit_edit, self._sync_limit = number_edit(draft, "limit", tr("Лимит URL"), 0, "scanUrlLimit")
        self.limit_stepper = Stepper(self.limit_edit, lambda: self._step_limit(-500), lambda: self._step_limit(500))
        self.limit_row = FormRow(tr("Лимит URL"), self.limit_stepper, "", label_widget=self.limit_switch, field=self.limit_edit)
        self.html_switch = Switch(tr("Сохранять HTML страниц"), draft.value("storage.body_mode") != "off")
        self.html_switch.setObjectName("scanSaveHtml")
        self.html_switch.setEnabled(draft.has("storage.body_mode"))
        self.html_switch.toggled.connect(lambda state: draft.set_value("storage.body_mode", "captured_entity_bytes" if state else "off"))
        self.html_state = ElidedLabel()  # elided, not clipped: the column must fit two options side by side
        self.html_state.setProperty("text_style", "meta")
        self.html_state.setToolTip(tr("Нужно для поиска в HTML и сравнений"))
        self.html_row = FormRow(tr("Сохранять HTML"), self.html_state, label_widget=self.html_switch, label_width=152)
        # why
        why = QLineEdit()
        why.setEnabled(False)
        why.setAccessibleName(tr("Зачем этот скан"))
        why.setPlaceholderText(tr("Связь запуска с задачей проекта"))
        self.why_row = FormRow(tr("Зачем этот скан"), hbox(why, waiting_badge(922, "Связь запуска с задачей проекта"), stretch=(why,)),
                               tr("В записи запуска ядра нет поля цели или задачи"), field=why)
        self.option_wide = self.why_row
        self.option_items = [self.mode_row, self.profile_row, self.rps_box, self.threads_box, self.limit_row, self.html_row, self.why_row]
        self.rows = self.option_items
        self.option_grid = grid
        self._layout_options(2)
        for column in (0, 1):
            grid.setColumnStretch(column, 1)
        return box

    def _aside(self):
        aside = QFrame()
        aside.setObjectName("scanAside")
        aside.setFixedWidth(ASIDE_WIDTH)
        layout = QVBoxLayout(aside)
        layout.setContentsMargins(16, 14, 16, 14)
        layout.setSpacing(8)
        head = QHBoxLayout()
        head.setSpacing(4)
        caption = QLabel(tr("Что произойдёт"))
        caption.setProperty("text_style", "control")
        head.addWidget(caption)
        head.addWidget(HelpIcon(tr("Запуск получит ID и появится в «Сканах» и у наблюдателя. Запуск начнётся только после «Запустить»; повторное нажатие не создаёт дубль.")))
        head.addStretch(1)
        layout.addLayout(head)
        self.plan_values = {}
        for key in ("source", "address", "mode", "urls", "requests", "speed", "duration", "disk", "paid", "impact"):
            row = QFrame()
            row.setProperty("kv_row", True)
            row.setFixedHeight(28)
            line = QHBoxLayout(row)
            line.setContentsMargins(0, 0, 0, 0)
            line.setSpacing(12)
            name = QLabel()
            name.setProperty("text_style", "meta")
            line.addWidget(name)
            if key in UNMEASURED:
                value = waiting_badge(ISSUE_ESTIMATE, UNMEASURED[key])
                value.setSizePolicy(QSizePolicy.Maximum, QSizePolicy.Fixed)
                line.addStretch(1)
                line.addWidget(value)
            else:
                value = ElidedLabel()
                value.setAlignment(Qt.AlignRight | Qt.AlignVCenter)
                value.setTextInteractionFlags(Qt.TextSelectableByMouse)
                line.addWidget(value, 1)
            value.setObjectName("plan_" + key)
            layout.addWidget(row)
            self.plan_values[key] = (name, value)
        layout.addStretch(1)
        self.confirm = QCheckBox()
        self.confirm.setObjectName("scanLargeApproval")
        self.confirm.setAccessibleName(tr("Я проверил объём и разрешаю обращаться к сайту"))
        self.confirm.toggled.connect(self._confirm_toggled)
        consent = QLabel(tr("Объём проверен, обращаться к сайту можно"))
        consent.setWordWrap(True)
        consent.setBuddy(self.confirm)
        consent.mousePressEvent = lambda _event: self.confirm.toggle()
        row = QHBoxLayout()
        row.setSpacing(8)
        row.addWidget(self.confirm, 0, Qt.AlignVCenter)
        row.addWidget(consent, 1)
        layout.addLayout(row)
        return aside

    # ---- actions -------------------------------------------------------------------------------------------------
    def _pick_source(self, key):
        self.draft.source = key
        self.draft.emit_changed()

    def _sitemap_typed(self, text):
        self.draft.sitemap_url = text
        self.draft.emit_changed()

    def _list_typed(self):
        if self._busy:
            return
        self.draft.list_text = self.list_edit.toPlainText()
        if len(self.draft.list_text) > 50_000:  # a very long list is counted once typing pauses
            self._list_timer.start(300)
        else:
            self.draft.emit_changed()

    def _list_paste(self):
        from PyQt5.QtWidgets import QApplication

        text = QApplication.clipboard().text()
        if text:
            self.list_edit.setPlainText((self.list_edit.toPlainText().rstrip("\n") + "\n" if self.list_edit.toPlainText() else "") + text)

    def _list_from_file(self):
        path, _ = QFileDialog.getOpenFileName(self, tr("Список URL"), "", tr("Списки (*.txt *.csv)"))
        if path:
            self.load_list_file(path)

    def load_list_file(self, path):
        file = Path(path)
        try:
            if file.stat().st_size > LIST_FILE_CAP:
                self.problem.setText(trf("Файл больше {n} МБ", n=LIST_FILE_CAP // 1024**2))
                return
            text = file.read_text(encoding="utf-8-sig", errors="replace")
        except OSError as exc:
            self.problem.setText(trf("Не удалось прочитать файл: {error}", error=exc.strerror or str(exc)))
            return
        self.list_edit.setPlainText(list_from_file_text(text, file.suffix))

    def _step_rps(self, step):
        rate = self.draft.rps() or 1
        self.draft.set_text("rps", f"{max(1, round(rate) + step)}")

    def _step_threads(self, step):
        current = self.draft.value("speed.concurrency") or 1
        self.draft.set_text("threads", f"{max(1, current + step)}")

    def _step_limit(self, step):
        self.draft.set_text("limit", f"{max(500, self.draft.url_limit + step)}")

    def _confirm_toggled(self, state):
        self.draft.confirmed = bool(state)
        self._confirmed_signature = self.draft.signature() if state else None
        self._refresh()

    def _open_settings(self, page):
        from .new_scan_settings import ScanSettingsDialog

        editor = ScanSettingsDialog(self.draft, self.host, self, page)
        editor.exec_()
        editor.deleteLater()

    def _accept_plan(self):
        draft = self.draft
        if draft is None:
            return
        try:
            plan = draft.build_plan()
        except PlanError as exc:
            self.problem.setText(str(exc))
            return
        if not draft.confirmed:
            self.problem.setText(tr("Подтвердите объём перед запуском"))
            return
        self.plan = plan
        self.accept()

    def done(self, result):
        if self.draft is not None and hasattr(self.host, "_scan_drafts") and hasattr(self.host, "note_project_key"):
            key = self.host.note_project_key()
            if key is not None:
                self.host._scan_drafts[key] = self.draft.snapshot()
        super().done(result)

    # ---- refresh -------------------------------------------------------------------------------------------------
    def _refresh(self):
        draft = self.draft
        if draft is None:
            self.start.setEnabled(False)
            self.problem.setText(tr("Параметры ядра загружаются…") if self.host.crawl_descriptor is None else "")
            self.problem_icon.setVisible(False)
            return
        if self._busy:
            return
        self._busy = True
        try:
            self._sync(draft)
        finally:
            self._busy = False

    def _sync(self, draft):
        problems = draft.problems()
        source = draft.source
        for key, card in self.cards.items():
            card.setChecked(key == source)
        declared = draft.capabilities.get("sitemap_only_retained") is True
        self.cards["sitemap"].setEnabled(declared)
        self.cards["sitemap"].setToolTip(tr(SOURCE_CARDS[1][3]) if declared else tr("Возможность не объявлена ядром"))
        self.source_text.setText(tr(next(text for key, _g, _n, text in SOURCE_CARDS if key == source)))
        self.site_block.setVisible(source == "site")
        self.sitemap_box.setVisible(source == "sitemap")
        self.list_block.setVisible(source == "list")
        self.sf_block.setVisible(source == "sf")
        if self.sitemap_edit.text() != draft.sitemap_url:
            self.sitemap_edit.setText(draft.sitemap_url)
        self.sitemap_box.set_error(problems.get("sitemap", ""))
        if self.list_edit.toPlainText() != draft.list_text:
            self.list_edit.setPlainText(draft.list_text)
        report = draft.list_report()
        texts = {
            "ready": trf("{n} готовы", n=grouped(report.ready)), "added": trf("{n} дополнен https://", n=grouped(report.added)),
            "duplicates": trf("{n} дубль убран", n=grouped(report.duplicates)), "foreign": trf("{n} чужой домен — пропущен", n=grouped(report.foreign)),
            "invalid": trf("{n} не адрес — пропущено", n=grouped(report.invalid)),
        }
        for name, label in self.counter_labels.items():
            label.setText(texts[name])
            label.setVisible(name in ("ready",) or bool(getattr(report, name)))
        self.list_note.setVisible(source == "list")
        for edit, sync in ((self.rps_edit, self._sync_rps), (self.threads_edit, self._sync_threads), (self.limit_edit, self._sync_limit)):
            sync()
        self.rps_box.set_error(problems.get("rps", ""))
        self.threads_box.set_error(problems.get("threads", ""))
        self.limit_row.set_error(problems.get("limit", ""))
        self.limit_stepper.set_enabled_value(draft.limit_enabled)
        saving = draft.value("storage.body_mode") != "off"
        self.html_state.setText(tr("для поиска и сравнений") if saving else tr("только метаданные"))
        self.html_state.setToolTip(tr("Нужно для поиска в HTML и сравнений"))  # ElidedLabel.setText overwrites the tooltip
        for switch, state in ((self.limit_switch, draft.limit_enabled), (self.html_switch, saving)):
            if switch.isChecked() != state:
                switch.blockSignals(True)
                switch.setChecked(state)
                switch.blockSignals(False)
        self.mode.setValue(draft.value("rendering.mode", "raw"))
        self.mode_row.help.setToolTip(tr("Медленнее примерно в 3 раза; страницы разбираются после выполнения JavaScript в браузере на этом компьютере")
                                      if draft.value("rendering.mode") == "js" else tr("Быстро; страницы разбираются без JavaScript"))
        names = {"core": tr("ядро"), "app": tr("настройки приложения"), "project": tr("профиль проекта")}
        state = draft.policy_state
        chain = trf("Умолчания: {chain}", chain=" → ".join(names[k] for k in ("core", "app") + (("project",) if draft.project_layer else ())))
        self.profile_line.setText(tr("Профиль проекта") if draft.project_layer else tr("Умолчания"))
        self.profile_line.setCursorPosition(0)  # narrow column: show the start of the name, not its tail
        hint = {"ready": trf("Профиль проекта: {n} парам.", n=len(draft.project_layer)) if draft.project_layer else tr("Профиль проекта не задан"),
                "loading": tr("Профиль проекта загружается…"), "unavailable": tr("Профиль проекта не прочитан"),
                "unknown": tr("Профиль проекта не прочитан")}[state]
        self.profile_line.setToolTip(f"{chain}\n{hint}")
        self.profile_row.help.setToolTip(f"{chain}\n{hint}")
        self.settings_link.setText(trf("Настройки скана… · доступно {n} из {total} параметров ядра", n=len(draft.core), total=len(draft.rows)))
        self._plan_block(draft)
        if draft.confirmed and self._confirmed_signature != draft.signature():
            draft.confirmed = False
            self._confirmed_signature = None
        if self.confirm.isChecked() != draft.confirmed:
            self.confirm.blockSignals(True)
            self.confirm.setChecked(draft.confirmed)
            self.confirm.blockSignals(False)
        self._footer_state(draft, problems)

    def _plan_block(self, draft):
        v = draft.values
        requests = v.get("limits.max_requests") or 0
        report = draft.list_report()
        address = {"site": draft.target, "sitemap": draft.sitemap_url.strip() or tr("не указан"),
                   "list": trf("Адресов: {n} (оценка приложения)", n=grouped(report.usable)), "sf": ""}[draft.source]
        rps = trf("{n} запр/с · {t} пот.", n=rps_label(draft), t=v.get("speed.concurrency", "—")) if draft.rps() else tr("Нет данных")
        rows = {
            "source": ("Источник", tr(SOURCE_NAMES[draft.source])),
            "address": ("Старт", address),
            "mode": ("Режим", tr("с рендерингом JS") if v.get("rendering.mode") == "js" else tr("исходный HTML")),
            "urls": ("Лимит URL", grouped(draft.url_limit) if draft.limit_enabled else tr("без лимита")),
            "requests": ("Запросов к сайту, до", grouped(requests) if requests else tr("без лимита")),
            "speed": ("Скорость", rps),
            "duration": ("Длительность", ""),
            "disk": ("Диск, до", ""),
            "paid": ("Платные провайдеры", tr("нет")),
            "impact": ("Влияние на сайт", tr("только чтение")),
        }
        for key, (caption, value) in rows.items():
            name, label = self.plan_values[key]
            name.setText(tr(caption))
            if key not in UNMEASURED:
                label.setText(str(value))
                label.setToolTip(str(value) + ("\n" + tr("GET-запросы, без изменений на сайте") if key == "impact" else ""))

    def _footer_state(self, draft, problems):
        titles = {key: tr(FIELD_TITLES[key]) for key in FIELD_TITLES}
        shown = ""
        for key, message in problems.items():
            shown = trf("Исправьте «{field}»", field=titles[key]) if key in titles and key != "source" else message
            break
        self.problem.setText(shown)
        self.problem.setToolTip("; ".join(problems.values()))
        self.problem_icon.setVisible(bool(shown))
        ready = bool(draft.confirmed) and not problems
        self.start.setEnabled(ready)
        self.start.setToolTip("" if ready else tr("Подтвердите объём и исправьте поля"))


def open_new_scan(host):
    """Entry used by the «Новый скан» button and ⌘N: opens the dialog, then launches only a confirmed plan."""
    if not host.project_directory or host._project_loading:
        host.notice.show_error("Сначала откройте проект и дождитесь его данных")
        return None
    dialog = NewScanDialog(host)
    dialog.exec_()
    plan, quick = dialog.plan, dialog.quick_requested
    dialog.deleteLater()
    if plan:
        return host.launch_scan(*plan)
    if quick:
        from .quick_scan import open_quick_scan

        open_quick_scan(host)
    return None
