"""Detail panels of the URL screen (canvas Main.dc.html bottom panel and aside, MainB.dc.html right accordion).

Everything shown here comes from ``scan-url-detail`` of the selected URL and from the saved scan row (counters the core
already holds). Tabs whose data the core does not give yet keep their place and say which core issue they wait for.
"""

from __future__ import annotations

from PyQt5.QtCore import Qt, QUrl, pyqtSignal
from PyQt5.QtGui import QDesktopServices
from PyQt5.QtWidgets import (
    QApplication,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QScrollArea,
    QSizePolicy,
    QStackedWidget,
    QTabBar,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .. import theming
from ..i18n import joined, tr, trf
from ..ui.controls import polish
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import StatePanel, waiting_badge
from ..ui.presentation import short_run_id
from .scan_common import Pairs, StatusBadge, number, parse_time
from .url_query import http_badge, index_badge, index_reason, seconds_to_ms
from .url_widgets import StackBar, section_label

SOURCE_KINDS = {
    "native": "Встроенный краулер",
    "sitemap": "Sitemap",
    "screaming_frog": "Импорт Screaming Frog",
}


def clean(value):
    """Text of a stored field; empty strings (an absent tag) and non-text stay None -> «Нет данных»."""
    return value if isinstance(value, str) and value.strip() else None


def source_text(source, page):
    kind = SOURCE_KINDS.get((source or {}).get("source_kind"))
    if kind is None:
        return None
    mode = (page or {}).get("representation")
    suffix = {"static": "без рендеринга", "rendered": "с рендерингом"}.get(mode)
    return f"{tr(kind)} · {tr(suffix)}" if suffix else tr(kind)


def stamp_text(value):
    stamp = parse_time(value)
    return stamp.astimezone().strftime("%d.%m.%Y %H:%M:%S") if stamp else None


def first_response(detail):
    items = ((detail or {}).get("responses") or {}).get("items") or []
    return items[0] if items and isinstance(items[0], dict) else {}


def headers_text(detail):
    """Status line and header lines of the first saved response; None when the core saved none."""
    response = first_response(detail)
    headers = response.get("response_headers")
    status = response.get("status_code")
    if not isinstance(headers, list) or type(status) is not int:
        return None
    lines = [f"HTTP {status}"]
    for pair in headers:
        if isinstance(pair, (list, tuple)) and len(pair) == 2:
            lines.append(f"{pair[0]}: {pair[1]}")
    return "\n".join(lines)


def page_metrics(row, page):
    """(words, depth, ms) of the selected URL: the detail answer first, the table row while it loads."""
    page = page or {}
    words = page.get("word_count", row.get("word_count"))
    depth = page.get("crawl_depth", row.get("crawl_depth"))
    ms = seconds_to_ms(page.get("response_time", row.get("response_time")))
    return (words if type(words) is int else None, depth if type(depth) is int else None, ms)


def key_label(text):
    label = QLabel(tr(text))
    label.setProperty("text_style", "meta")
    return label


def icon_button(icon, tip, enabled=True):
    button = QToolButton()
    button.setProperty("role", "icon")
    button.setIcon(material_icon(icon))
    button.setToolTip(tr(tip))
    button.setAccessibleName(tr(tip))
    button.setEnabled(enabled)
    return button


class UrlHead(QWidget):
    """HTTP badge, URL (mono, selectable), copy / open in browser, optional note to the agent."""

    note_requested = pyqtSignal()

    def __init__(self, parent=None):
        super().__init__(parent)
        self.url = None
        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(8)
        self.http = StatusBadge()
        self.http.hide()
        layout.addWidget(self.http)
        self.label = QLabel()
        self.label.setProperty("text_style", "mono")
        self.label.setTextInteractionFlags(Qt.TextSelectableByMouse)
        self.label.setSizePolicy(
            self.label.sizePolicy().horizontalPolicy(), self.label.sizePolicy().verticalPolicy()
        )
        self.label.setMinimumWidth(40)
        layout.addWidget(self.label, 1)
        self.copy = icon_button("content_copy", "Копировать URL", False)
        self.copy.clicked.connect(lambda _c=False: QApplication.clipboard().setText(self.url or ""))
        self.browser = icon_button("open_in_new", "Открыть в системном браузере", False)
        self.browser.clicked.connect(
            lambda _c=False: self.url and QDesktopServices.openUrl(QUrl(self.url))
        )
        self.note_allowed = True
        self.note = QPushButton(tr("Заметка агенту"))
        self.note.setProperty("size", "sm")
        self.note.setIcon(material_icon("edit_note"))
        self.note.clicked.connect(self.note_requested.emit)
        for widget in (self.copy, self.browser, self.note):
            layout.addWidget(widget)

    def set_row(self, row):
        self.url = row.get("url") if row else None
        self.label.setText(self.url or "")
        self.label.setToolTip(self.url or "")
        self.copy.setEnabled(bool(self.url))
        self.browser.setEnabled(bool(self.url))
        if row:
            kind, text = http_badge(row.get("status_code"))
            self.http.set_state(
                kind,
                text,
                {"ok": "check_circle", "info": "turn_right", "warn": "warning", "err": "error"}.get(
                    kind, "help"
                ),
            )
            self.http.show()
        else:
            self.http.hide()


class OverviewPage(QWidget):
    """«Обзор»: indexation / title / canonical / redirect / words / H1 on the left, origin of the saved data on the right."""

    def __init__(self, parent=None):
        super().__init__(parent)
        grid = QGridLayout(self)
        grid.setContentsMargins(20, 12, 20, 12)
        grid.setHorizontalSpacing(32)
        grid.setColumnStretch(0, 5)
        grid.setColumnStretch(1, 4)
        self.left = Pairs(
            (
                "Индексация",
                "Title",
                "Canonical",
                "Цель редиректа",
                "Слов в контенте",
                "Глубина обхода",
                "H1",
            ),
            mono=("Canonical", "Цель редиректа"),
        )
        self.left.values["Глубина обхода"].setToolTip(
            tr("Расстояние от стартовых адресов; адреса из sitemap считаются стартовыми")
        )
        self.right_box = QWidget()
        right = QVBoxLayout(self.right_box)
        right.setContentsMargins(0, 0, 0, 0)
        right.setSpacing(8)
        right.addWidget(section_label("Происхождение"))
        self.right = Pairs(("Источник", "Скан", "Снято", "Ответ"), mono=("Скан",))
        right.addWidget(self.right)
        right.addWidget(key_label("Связано"))
        right.addWidget(self._waiting("Проблемы URL, задача, глубина кликов", 980))
        right.addStretch(1)
        self.grid = grid
        grid.addWidget(self.left, 1, 0, Qt.AlignTop)
        grid.addWidget(self.right_box, 1, 1, Qt.AlignTop)
        grid.setRowStretch(3, 1)
        self.note = QLabel()
        self.note.setProperty("text_style", "meta")
        self.note.setWordWrap(True)
        grid.addWidget(self.note, 3, 0, 1, 2, Qt.AlignTop)
        self._narrow = False

    def resizeEvent(self, event):
        super().resizeEvent(event)
        narrow = event.size().width() < 1000
        if narrow != self._narrow:
            self._narrow = narrow
            self.grid.removeWidget(self.left)
            self.grid.removeWidget(self.right_box)
            if narrow:
                self.grid.addWidget(self.left, 1, 0, 1, 2, Qt.AlignTop)
                self.grid.addWidget(self.right_box, 2, 0, 1, 2, Qt.AlignTop)
            else:
                self.grid.addWidget(self.left, 1, 0, Qt.AlignTop)
                self.grid.addWidget(self.right_box, 1, 1, Qt.AlignTop)

    @staticmethod
    def _waiting(text, issue):
        holder = QWidget()
        layout = QHBoxLayout(holder)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(4)
        label = QLabel(tr(text))
        label.setProperty("text_style", "meta")
        layout.addWidget(label)
        layout.addWidget(waiting_badge(issue))
        layout.addStretch(1)
        return holder

    def fill(self, row, page, detail, state, text=""):
        page = page or {}
        badge = (
            index_badge(page.get("status_code", row.get("status_code")), row.get("indexable"))[1]
            if row
            else None
        )
        reason = index_reason(page) if page else None
        reasons = (reason,) if reason and (badge or "").casefold() != reason.casefold() else ()
        self.left.set("Индексация", joined(" · ", [badge, *reasons]) if badge else None)
        self.left.set(
            "Title", clean(page.get("title")) or clean((row or {}).get("title"))
        )  # the table row answers while the detail loads
        self.left.set("Canonical", clean(page.get("canonical")))
        self.left.set(
            "Цель редиректа", clean(page.get("redirect_url")) or clean(page.get("final_url"))
        )
        words, _depth, ms = page_metrics(row or {}, page)
        self.left.set("Слов в контенте", number(words))
        depth = page.get("crawl_depth", row.get("crawl_depth")) if row else None
        self.left.set("Глубина обхода", number(depth) if type(depth) is int else None)
        self.left.set("H1", clean(page.get("h1")))
        source = (detail or {}).get("source") or {}
        response = first_response(detail)
        self.right.set("Источник", source_text(source, page))
        self.right.set("Скан", short_run_id(source.get("scan_uuid")))
        self.right.values["Скан"].setToolTip(source.get("scan_uuid") or "")
        self.right.set("Снято", stamp_text(response.get("received_at")))
        self.right.set("Ответ", trf("{ms} мс", ms=number(ms)) if ms is not None else None)
        text = text or (tr("Читаю страницу…") if state == "loading" else "")
        self.note.setText(text)
        self.note.setVisible(bool(text))


class SnippetPage(QWidget):
    """«Сниппет»: title and description with their lengths, as the page stores them."""

    def __init__(self, host=None, parent=None):
        super().__init__(parent)
        self.host = host
        self.url = ""
        self.title = ""
        self.description = ""
        layout = QVBoxLayout(self)
        layout.setContentsMargins(20, 12, 20, 12)
        self.pairs = Pairs(("Title", "Длина title", "Description", "Длина description"))
        layout.addWidget(self.pairs)
        self.edit_button = QPushButton("Prepare review task…")
        self.edit_button.clicked.connect(self._edit)
        layout.addWidget(self.edit_button, 0, Qt.AlignLeft)
        layout.addStretch(1)

    def fill(self, page, url=""):
        page = page or {}
        title, description = clean(page.get("title")), clean(page.get("meta_description"))
        self.url, self.title, self.description = url or "", title or "", description or ""
        self.pairs.set("Title", title)
        self.pairs.set("Длина title", trf("{n} зн.", n=len(title)) if title else None)
        self.pairs.set("Description", description)
        self.pairs.set(
            "Длина description", trf("{n} зн.", n=len(description)) if description else None
        )

    def _edit(self):
        if not self.host or not self.url:
            return
        from .methods import VIEW_METHODS

        page = self.host.placeholder_pages.get(VIEW_METHODS)
        if page is None or not hasattr(page, "open_snippet"):
            return
        page.open_snippet(self.url, self.title, self.description)
        self.host.navigation.select_section("methods")


class Accordion(QFrame):
    """One section of the right-hand details: header row (icon, title, badge, chevron) and a text body."""

    def __init__(self, icon, title, parent=None):
        super().__init__(parent)
        self.setProperty("card", "section")
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        self.head = QPushButton()
        self.head.setProperty("accordion", True)
        self.head.setCheckable(True)
        self.head.setFlat(True)
        self.head.setText(tr(title))
        self.head.setIcon(material_icon(icon))
        self.head.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Fixed)
        self.head.setMinimumHeight(40)
        head_row = QHBoxLayout()
        head_row.setContentsMargins(0, 0, 0, 0)
        head_row.addWidget(self.head, 1)
        self.badge = QLabel()
        self.badge.setProperty("badge", "mut")
        head_row.addWidget(self.badge)
        head_row.addSpacing(8)
        layout.addLayout(head_row)
        self.body = QLabel()
        self.body.setWordWrap(True)
        self.body.setContentsMargins(16, 4, 16, 12)
        self.body.setTextInteractionFlags(Qt.TextSelectableByMouse)
        self.body.setVisible(False)
        layout.addWidget(self.body)
        self.head.toggled.connect(self.body.setVisible)

    def set_content(self, badge_kind, badge, text):
        self.badge.setText(badge)
        self.badge.setVisible(bool(badge))
        self.badge.setProperty("badge", badge_kind)
        polish(self.badge)
        self.body.setText(text)


class UrlSideDetail(QFrame):
    """Layout B (MainB.dc.html): details to the right of the table, header + metrics + chips + accordion."""

    tab_requested = pyqtSignal(
        str
    )  # a chip of the strip «Вкладки URL целиком» asks the card to open that tab
    hide_requested = pyqtSignal()
    note_requested = pyqtSignal()
    back_requested = pyqtSignal()

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setProperty("aside", True)
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        head = QWidget()
        head_layout = QVBoxLayout(head)
        head_layout.setContentsMargins(16, 12, 12, 8)
        head_layout.setSpacing(4)
        top = QHBoxLayout()
        top.setSpacing(4)
        self.http = StatusBadge()
        self.index = StatusBadge()
        self.http.hide()
        self.index.hide()
        top.addWidget(self.http)
        top.addWidget(self.index)
        top.addStretch(1)
        self.back = icon_button("arrow_back", "Назад", False)
        self.back.clicked.connect(self.back_requested.emit)
        self.browser = icon_button("open_in_new", "Открыть в системном браузере", False)
        self.browser.clicked.connect(
            lambda _c=False: self.url and QDesktopServices.openUrl(QUrl(self.url))
        )
        self.close_button = icon_button("right_panel_close", "Скрыть детали")
        self.close_button.clicked.connect(self.hide_requested.emit)
        for widget in (self.back, self.browser, self.close_button):
            top.addWidget(widget)
        head_layout.addLayout(top)
        self.url = None
        self.address = QLabel()
        self.address.setProperty("text_style", "mono")
        self.address.setWordWrap(True)
        self.address.setTextInteractionFlags(Qt.TextSelectableByMouse)
        head_layout.addWidget(self.address)
        self.title = QLabel()
        self.title.setProperty("text_style", "section")
        self.title.setWordWrap(True)
        head_layout.addWidget(self.title)
        root.addWidget(head)
        self.metrics = {}
        strip = QFrame()
        strip.setProperty("table_head", True)
        strip_layout = QHBoxLayout(strip)
        strip_layout.setContentsMargins(0, 0, 0, 0)
        strip_layout.setSpacing(0)
        for key in ("Слов", "Входящих", "Глубина обхода", "Ответ"):
            cell = QVBoxLayout()
            cell.setContentsMargins(16, 8, 8, 8)
            cell.setSpacing(0)
            caption = key_label(key)
            value = QLabel()
            value.setProperty("text_style", "section")
            cell.addWidget(caption)
            cell.addWidget(value)
            strip_layout.addLayout(cell, 1)
            self.metrics[key] = value
        root.addWidget(strip)
        chips = QWidget()
        chip_grid = QGridLayout(chips)
        chip_grid.setContentsMargins(16, 8, 16, 8)
        chip_grid.setSpacing(4)
        for slot, (tab, label) in enumerate(
            (
                ("hdr", "Заголовки"),
                ("links", "Ссылки"),
                ("html", "HTML"),
                ("res", "Ресурсы"),
                ("redir", "Редиректы"),
                ("hist", "История"),
                ("schema", "Schema"),
            )
        ):
            chip = QToolButton()
            chip.setProperty("pill", "group")
            chip.setText(tr(label))
            chip.clicked.connect(lambda _c=False, t=tab: self.tab_requested.emit(t))
            chip_grid.addWidget(chip, slot // 4, slot % 4)
        chip_grid.setColumnStretch(4, 1)
        root.addWidget(chips)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        body = QWidget()
        body_layout = QVBoxLayout(body)
        body_layout.setContentsMargins(0, 0, 0, 0)
        body_layout.setSpacing(0)
        self.sections = {}
        for key, icon, title in (
            ("meta", "sell", "Метаданные"),
            ("in", "link", "Входящие ссылки"),
            ("out", "call_made", "Исходящие ссылки"),
            ("hdr", "http", "HTTP-заголовки"),
            ("html", "code", "HTML"),
            ("checks", "rule", "Проверки"),
        ):
            section = Accordion(icon, title)
            self.sections[key] = section
            body_layout.addWidget(section)
        body_layout.addStretch(1)
        scroll.setWidget(body)
        root.addWidget(scroll, 1)
        self.placeholder = StatePanel(
            "empty", "Выберите URL в таблице", "Детали сохранённого ответа появятся здесь"
        )
        root.addWidget(self.placeholder)

    def render(self, row, page, detail, state, text=""):
        has = state != "none" and bool(row)
        self.placeholder.setVisible(not has)
        for widget in (self.http, self.index):
            widget.setVisible(has)
        if not has:
            self.url = None
            self.address.setText("")
            self.title.setText("")
            self.browser.setEnabled(False)
            for label in self.metrics.values():
                label.setText("")
            return
        page = page or {}
        self.url = row.get("url")
        self.browser.setEnabled(bool(self.url))
        self.address.setText(self.url or "")
        self.title.setText(clean(page.get("title")) or clean(row.get("title")) or tr("Нет данных"))
        kind, label = http_badge(row.get("status_code"))
        self.http.set_state(
            kind,
            label,
            {"ok": "check_circle", "info": "turn_right", "warn": "warning", "err": "error"}.get(
                kind, "help"
            ),
        )
        ikind, itext = index_badge(row.get("status_code"), row.get("indexable"))
        self.index.set_state(
            ikind, itext, {"ok": "check_circle", "info": "turn_right"}.get(ikind, "block")
        )
        words, depth, ms = page_metrics(row, page)
        self.metrics["Слов"].setText(number(words) or tr("Нет данных"))
        self.metrics["Входящих"].setText(tr("Недоступно"))
        self.metrics["Глубина обхода"].setText(number(depth) or tr("Нет данных"))
        self.metrics["Ответ"].setText(
            trf("{ms} мс", ms=number(ms)) if ms is not None else tr("Нет данных")
        )
        title, description = clean(page.get("title")), clean(page.get("meta_description"))
        meta = joined(
            "\n",
            [
                trf("Title: {t} ({n} зн.)", t=title, n=len(title))
                if title
                else tr("Title: нет данных"),
                trf("Description: {t} ({n} зн.)", t=description, n=len(description))
                if description
                else tr("Description: нет данных"),
                trf("Canonical: {t}", t=clean(page.get("canonical")) or tr("нет данных")),
            ],
        )
        self.sections["meta"].set_content("mut", "", meta)
        self.sections["in"].set_content(
            "mut",
            tr("Недоступно в этой версии ядра"),
            tr("Счётчик и список входящих ссылок ядро пока не отдаёт. Это не «0»."),
        )
        outlinks, external = page.get("outlinks"), page.get("external_outlinks")
        out_text = (
            trf(
                "Исходящих ссылок: {n}, из них внешних: {e}. Список ссылок с rel и HTTP цели недоступен в этой версии ядра.",
                n=number(outlinks),
                e=number(external),
            )
            if type(outlinks) is int and type(external) is int
            else tr("Исходящие ссылки в ответе ядра не пришли.")
        )
        self.sections["out"].set_content("mut", number(outlinks) or tr("Нет данных"), out_text)
        headers = headers_text(detail)
        self.sections["hdr"].set_content(
            "mut",
            number(len(headers.splitlines()) - 1) if headers else tr("Нет данных"),
            headers or tr("Ядро не сохранило заголовки этого ответа."),
        )
        self.sections["html"].set_content(
            "mut",
            tr("Недоступно в этой версии ядра"),
            tr("Читатель сохранённого исходника ждёт ядра."),
        )
        self.sections["checks"].set_content(
            "mut",
            tr("Недоступно в этой версии ядра"),
            tr("Находки по одному URL ядро пока не отдаёт."),
        )


class RunSummary(QFrame):
    """Right aside (296): «Обзор» / «Проблемы» / «Время» of the selected saved scan."""

    class_requested = pyqtSignal(str)  # a status class legend row was clicked: "2xx" … "5xx"
    issues_requested = pyqtSignal()
    hide_requested = pyqtSignal()

    CLASSES = (
        ("2xx", "2xx · успешно", "s2xx"),
        ("3xx", "3xx · редирект", "s3xx"),
        ("4xx", "4xx · ошибка клиента", "s4xx"),
        ("5xx", "5xx · ошибка сервера", "s5xx"),
    )
    BUCKETS = (
        ("lt200", "< 200 мс"),
        ("200_500", "200–500 мс"),
        ("500_1000", "0,5–1 с"),
        ("1000_3000", "1–3 с"),
        ("gt3000", "> 3 с"),
    )

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setProperty("aside", True)
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        head = QFrame()
        head.setProperty("aside_head", True)
        head_layout = QHBoxLayout(head)
        head_layout.setContentsMargins(4, 0, 4, 0)
        self.tabs = QTabBar()
        self.tabs.setUsesScrollButtons(
            True
        )  # labels keep their full text at narrow widths: the bar scrolls
        self.tabs.setElideMode(Qt.ElideNone)
        self.tabs.setProperty("tabs", "underline")
        self.tabs.setDrawBase(False)
        for name in ("Обзор", "Проблемы", "Время"):
            self.tabs.addTab(tr(name))
        head_layout.addWidget(self.tabs, 1)
        self.hide_button = icon_button("right_panel_close", "Скрыть сводку")
        self.hide_button.clicked.connect(self.hide_requested.emit)
        head_layout.addWidget(self.hide_button)
        root.addWidget(head)
        self.pages = QStackedWidget()
        root.addWidget(self.pages, 1)
        self.tabs.currentChanged.connect(self.pages.setCurrentIndex)
        self._overview()
        self._issues()
        self._time()
        self.scan = None
        self.counts = {}

    @staticmethod
    def _scrolled(layout):
        body = QWidget()
        body.setLayout(layout)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        scroll.setWidget(body)
        return scroll

    def _overview(self):
        layout = QVBoxLayout()
        layout.setContentsMargins(16, 12, 16, 12)
        layout.setSpacing(16)
        self.partial = QFrame()
        self.partial.setProperty("note", "warn")
        partial_layout = QHBoxLayout(self.partial)
        partial_layout.setContentsMargins(8, 8, 8, 8)
        partial_layout.addWidget(
            MaterialIconLabel("warning", 18, color="role:warning"), 0, Qt.AlignTop
        )
        self.partial_text = QLabel()
        self.partial_text.setWordWrap(True)
        partial_layout.addWidget(self.partial_text, 1)
        layout.addWidget(self.partial)
        layout.addWidget(section_label("Ответы"))
        self.responses = QLabel()
        self.responses.setProperty("text_style", "meta")
        layout.addWidget(self.responses)
        self.bar = StackBar()
        layout.addWidget(self.bar)
        grid = QGridLayout()
        grid.setHorizontalSpacing(8)
        grid.setVerticalSpacing(2)
        grid.setColumnStretch(0, 1)
        self.class_buttons, self.class_counts = {}, {}
        for row, (value, label, role) in enumerate(self.CLASSES):
            button = QToolButton()
            button.setProperty("pill", "group")
            button.setText(tr(label))
            button.setToolButtonStyle(Qt.ToolButtonTextOnly)
            button.setToolTip(tr("Показать эти URL в таблице"))
            button.clicked.connect(lambda _c=False, v=value: self.class_requested.emit(v))
            count = QLabel()
            count.setAlignment(Qt.AlignRight | Qt.AlignVCenter)
            grid.addWidget(button, row, 0, Qt.AlignLeft)
            grid.addWidget(count, row, 1)
            self.class_buttons[value], self.class_counts[value] = button, count
        layout.addLayout(grid)
        self.other = QLabel()
        self.other.setProperty("text_style", "meta")
        self.other.setWordWrap(True)
        layout.addWidget(self.other)
        layout.addWidget(section_label("Индексация"))
        self.index_pairs = Pairs(("Индексируемые", "Не индексируемые", "Без canonical"))
        layout.addWidget(self.index_pairs)
        layout.addWidget(section_label("Охват запуска"))
        self.coverage = QLabel()
        self.coverage.setWordWrap(True)
        layout.addWidget(self.coverage)
        self.coverage_bar = StackBar(8)
        layout.addWidget(self.coverage_bar)
        self.coverage_note = QLabel()
        self.coverage_note.setProperty("text_style", "meta")
        self.coverage_note.setWordWrap(True)
        layout.addWidget(self.coverage_note)
        layout.addStretch(1)
        self.pages.addWidget(self._scrolled(layout))

    def _issues(self):
        layout = QVBoxLayout()
        layout.setContentsMargins(16, 12, 16, 12)
        layout.setSpacing(8)
        layout.addWidget(section_label("Находки по важности"))
        self.severity = Pairs(("Критичные", "Важные", "Советы"))
        layout.addWidget(self.severity)
        layout.addWidget(
            StatePanel(
                "waiting",
                "Топ проверок со счётчиками",
                "Счётчики по каждой проверке ядро отдаёт только вместе с полным аудитом",
                issue=980,
            )
        )
        button = QPushButton(tr("Открыть «Проблемы»"))
        button.setIcon(material_icon("rule"))
        button.clicked.connect(self.issues_requested.emit)
        layout.addWidget(button)
        layout.addStretch(1)
        self.pages.addWidget(self._scrolled(layout))

    def _time(self):
        layout = QVBoxLayout()
        layout.setContentsMargins(16, 12, 16, 12)
        layout.setSpacing(8)
        layout.addWidget(section_label("Время ответа"))
        self.time_rows = {}
        for key, label in self.BUCKETS:
            line = QHBoxLayout()
            caption = QLabel(tr(label))
            caption.setProperty("text_style", "meta")
            caption.setMinimumWidth(84)
            bar = StackBar(10)
            count = QLabel()
            count.setAlignment(Qt.AlignRight | Qt.AlignVCenter)
            count.setMinimumWidth(48)
            line.addWidget(caption)
            line.addWidget(bar, 1)
            line.addWidget(count)
            layout.addLayout(line)
            self.time_rows[key] = (bar, count)
        self.time_note = QLabel()
        self.time_note.setProperty("text_style", "meta")
        self.time_note.setWordWrap(True)
        layout.addWidget(self.time_note)
        layout.addStretch(1)
        self.pages.addWidget(self._scrolled(layout))

    # ---- data -------------------------------------------------------------------------------------------------
    def set_scan(self, scan):
        self.scan = scan
        scan = scan or {}
        evidence = scan.get("evidence") or {}
        outcomes = evidence.get("committed_page_outcomes") or {}
        counts = {
            key: outcomes.get(key) if type(outcomes.get(key)) is int else None
            for key in ("2xx", "3xx", "4xx", "5xx", "other", "no_response")
        }
        classes = [counts[v] for v, _l, _r in self.CLASSES]
        measured = all(c is not None for c in classes)
        data = theming.theme()["data"]
        total = sum(c for c in classes if c is not None) + sum(
            counts[k] or 0 for k in ("other", "no_response")
        )
        self.responses.setText(trf("{n} URL", n=number(total)) if measured else tr("Нет данных"))
        self.bar.set_parts(
            [(counts[v], data[role]) for v, _l, role in self.CLASSES] if measured else []
        )
        for value, _label, _role in self.CLASSES:
            self.class_counts[value].setText(number(counts[value]) or tr("Нет данных"))
            self.class_buttons[value].setEnabled(measured and counts[value] is not None)
        extra = [(counts["other"], "прочие ответы"), (counts["no_response"], "без ответа")]
        self.other.setText(joined(" · ", [f"{tr(label)}: {number(n)}" for n, label in extra if n]))
        self.other.setVisible(bool(self.other.text()))
        lifecycle, crawl_partial = scan.get("lifecycle"), scan.get("crawl_partial")
        frontier = (evidence.get("frontier") or {}).get("counts") or {}
        queued = frontier.get("queued")
        reasons = []
        if crawl_partial is True or lifecycle in ("interrupted", "running"):
            reasons.append(tr("Запуск частичный."))
            if type(queued) is int and queued > 0:
                reasons.append(trf("В очереди остались необработанные URL: {n}.", n=number(queued)))
        elif crawl_partial is False and scan.get("corpus_partial") is True:
            reasons.append(
                tr("Обход полный, но корпус неполный: часть сохранённых тел страниц отсутствует.")
            )
        self.partial.setVisible(bool(reasons))
        self.partial_text.setText(" ".join(reasons))
        done, inflight = frontier.get("done"), frontier.get("inflight")
        known = all(type(v) is int and v >= 0 for v in (done, queued, inflight))
        if known and done + queued + inflight > 0:
            found = done + queued + inflight
            self.coverage.setText(
                trf(
                    "Обработано известных URL: {done} из {found}",
                    done=number(done),
                    found=number(found),
                )
            )
            self.coverage_bar.set_parts(
                [
                    (done, theming.theme()["data"]["progress"]),
                    (found - done, theming.roles()["disabled_bg"]),
                ]
            )
            self.coverage_note.setText(tr("Лимит скана не равен размеру сайта"))
        else:
            self.coverage.setText(tr("Нет данных"))
            self.coverage_bar.set_parts([])
            self.coverage_note.setText(tr("Счётчики очереди ядро не вернуло"))
        findings = evidence.get("findings") or {}
        by_severity = findings.get("by_severity") if findings.get("state") == "available" else None
        for key, label in (("critical", "Критичные"), ("warning", "Важные"), ("notice", "Советы")):
            value = by_severity.get(key, 0) if isinstance(by_severity, dict) else None
            self.severity.set(label, number(value) if type(value) is int else None)
        self.set_counts(self.counts)

    def set_counts(self, counts):
        """Answers of the counter queries: ``indexable_yes``, ``indexable_no`` and the response time buckets."""
        self.counts = dict(counts)
        self.index_pairs.set("Индексируемые", number(counts.get("indexable_yes")))
        self.index_pairs.set("Не индексируемые", number(counts.get("indexable_no")))
        self.index_pairs.set("Без canonical", None)
        bucket_values = [counts.get(key) for key, _label in self.BUCKETS]
        measured = all(type(v) is int for v in bucket_values)
        total = sum(v for v in bucket_values if type(v) is int)
        palette = ("s2xx", "s3xx", "s4xx", "s5xx", "s5xx")
        data = theming.theme()["data"]
        for (key, _label), role in zip(self.BUCKETS, palette):
            bar, label = self.time_rows[key]
            value = counts.get(key)
            label.setText(number(value) or tr("Нет данных"))
            bar.set_parts(
                [(value, data[role]), (max(0, total - value), theming.roles()["disabled_bg"])]
                if type(value) is int and total
                else []
            )
        self.time_note.setText(
            tr("Медиана и p95 ядро не считает — числа не показываем.")
            if measured
            else tr("Подсчёт по корзинам идёт или недоступен.")
        )
