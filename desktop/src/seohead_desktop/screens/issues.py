"""Findings for the selected saved scan, with whole-scan counters from core evidence.

The scan row supplies severity totals, a bounded findings sample, and skipped checks. A separate
core query supplies exact per-check totals; URL examples remain limited to the saved sample. The
remediation path still depends on the task ledger and confirmation workflow.
"""

from __future__ import annotations

import re

from PyQt5.QtCore import QAbstractTableModel, Qt
from PyQt5.QtWidgets import (
    QApplication,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLineEdit,
    QMenu,
    QPushButton,
    QScrollArea,
    QStackedWidget,
    QTableView,
    QToolButton,
    QVBoxLayout,
    QWidget,
)
from seohead.sf.core.registry import CHECKS as CORE_CHECKS

from .. import i18n, theming
from ..i18n import tr, trf
from ..ui import menus
from ..ui.icons import MaterialIconLabel, material_icon
from ..ui.kit import (
    BADGE_ROLE,
    BadgeDelegate,
    StatePanel,
    no_project_panel,
    set_panel_state,
    style_table,
    unavailable_tip,
    waiting_badge,
)
from .base import Screen
from .scan_common import number, parse_time, selected_scan
from .url_query import CoreJob
from .url_widgets import StackBar, section_label

SEVERITIES = (
    ("critical", "Критичные", "error", "error"),
    ("warning", "Важные", "warning", "warning"),
    ("notice", "Советы", "lightbulb", "text_2"),
)
CHECK_NAMES = {
    "SERVER_ERROR_5XX": "Ответ 5xx",
    "BROKEN_PAGE_4XX": "Страницы с ответом 4xx",
    "HTTP_URL": "Адрес по HTTP, а не HTTPS",
    "DESC_MISSING": "Без meta description",
    "DESC_DUPLICATE": "Дубли meta description",
    "TITLE_MISSING": "Без title",
    "TITLE_DUPLICATE": "Дубли title",
    "DEEP_CRAWL_DEPTH": "Большая глубина вложенности",
    "H1_MULTIPLE": "Несколько H1",
    "H1_MISSING": "Без H1",
    "CANONICAL_MISSING": "Без canonical",
    "CANONICAL_CHAIN": "Цепочка canonical",
    "CANONICAL_NON_INDEXABLE": "Canonical на неиндексируемый URL",
    "HREFLANG_INVALID_CODE": "Неверный код hreflang",
    "HREFLANG_INCONSISTENT_CONFIRMATION": "Hreflang без обратного подтверждения",
    "REDIRECT_CHAIN": "Цепочки редиректов",
    "IMG_MISSING_ALT": "Изображения без alt",
    "IMG_OVER_KB": "Тяжёлые изображения",
    "IMG_MISSING_DIMENSIONS": "Изображения без width и height",
    "MIXED_CONTENT": "Смешанный контент HTTP на HTTPS",
    "MISSING_HSTS": "Нет заголовка HSTS",
    "STRUCTURED_DATA_MISSING": "Нет микроразметки",
    "HREFLANG_ERROR": "Ошибки hreflang",
    "HREFLANG_BROKEN_TARGET": "Hreflang ведёт на битый URL",
    "HREFLANG_NOINDEX_TARGET": "Hreflang ведёт на noindex",
    "HREFLANG_MISSING_RETURN_LINK": "Hreflang без обратной ссылки",
    "SITEMAP_URL_4XX_5XX": "В sitemap URL с ответом 4xx/5xx",
    "SITEMAP_URL_3XX": "В sitemap URL с редиректом",
    "SITEMAP_URL_NON_INDEXABLE": "В sitemap неиндексируемые URL",
    "SITEMAP_URL_DUPLICATED": "Дубли URL в sitemap",
    "SITEMAP_NOT_IN_ROBOTS": "Sitemap не указан в robots.txt",
    "SITEMAP_TOO_MANY_URLS": "В sitemap больше 50 000 URL",
    "SITEMAP_TOO_LARGE": "Sitemap тяжелее 50 МБ",
    "SITEMAP_STALE_LASTMOD": "Устаревший lastmod в sitemap",
    "SITEMAP_DESYNC": "Sitemap расходится со сканом",
    "SITEMAP_ORPHAN": "URL из sitemap без внутренних ссылок",
    "SITEMAP_FETCH_INCOMPLETE": "Sitemap загружен не полностью",
    "URL_NOT_IN_SITEMAP": "Индексируемый URL вне sitemap",
    "H2_MISSING": "Без H2",
    "META_KEYWORDS_PRESENT": "Заполнен meta keywords",
    "PAGINATION_CANONICAL_POLICY": "Canonical на страницах пагинации",
    "FILTER_CANONICAL_POLICY": "Canonical на страницах фильтров",
    "SCHEMA_VALIDATION_ERROR": "Ошибки валидации микроразметки",
    "DUPLICATE_BY_HASH": "Точные дубли страниц",
    "NEAR_DUPLICATE": "Почти дубли страниц",
    "READABILITY_DIFFICULT": "Текст трудно читать",
    "LONG_SENTENCES": "Длинные предложения",
    "SPELLING_ERRORS": "Орфографические ошибки",
    "GRAMMAR_ERRORS": "Грамматические ошибки",
    "CANONICAL_MULTIPLE": "Несколько canonical",
    "CANONICAL_TARGET_ERROR": "Canonical на URL с ошибкой",
    "PAGINATION_LOOP": "Петля в пагинации",
    "UNLINKED_PAGINATION_SERIES": "Пагинация без связи между страницами",
    "PAGINATION_SEQUENCE_ERROR": "Нарушен порядок пагинации",
    "PAGINATION_MULTIPLE": "Несколько rel=next/prev",
    "PAGINATION_URL_NOT_IN_ANCHOR": "Страница пагинации без ссылки в тексте",
    "HTTP1_ONLY": "Только HTTP/1.1",
    "AMPHTML_PRESENT": "Есть AMP-версия",
    "DECLARED_MIME_MISMATCH": "Тип контента не совпадает с расширением",
    "TITLE_MULTIPLE": "Несколько title",
    "BROKEN_INTERNAL_LINK": "Битые внутренние ссылки",
    "BROKEN_EXTERNAL_LINK": "Битые внешние ссылки",
    "LINK_TO_5XX": "Ссылки на страницы с ответом 5xx",
    "INTERNAL_LINK_TO_REDIRECT": "Внутренние ссылки на редирект",
    "EXTERNAL_LINK_TO_REDIRECT": "Внешние ссылки на редирект",
    "HTTP_LINK_ON_HTTPS": "Ссылка по HTTP на странице HTTPS",
    "INSECURE_SUBRESOURCE": "Небезопасные ресурсы страницы",
    "ROBOTS_BLOCKS_RESOURCES": "robots.txt закрывает JS и CSS",
    "OG_MISSING": "Нет разметки Open Graph",
    "NO_AUTHOR_BYLINE": "У статьи нет автора",
    "NO_CONTENT_DATES": "У статьи нет дат",
    "FEW_CITATIONS": "В статье мало источников",
    "DOM_TOO_DEEP": "Слишком глубокий DOM",
    "DOM_TOO_MANY_NODES": "Слишком много узлов DOM",
    "NO_RESPONSE": "Нет ответа сервера",
    "BLOCKED_BY_ROBOTS": "Закрыто в robots.txt",
    "NON_INDEXABLE_LINKED": "Неиндексируемая страница со внутренними ссылками",
    "IMPORTANT_URL_BLOCKED_BY_ROBOTS": "Важный URL закрыт в robots.txt при внутренних ссылках",
    "REDIRECT_LOOP": "Петля редиректов",
    "BAD_REDIRECT_TYPE": "Неподходящий тип редиректа",
    "TITLE_TOO_LONG": "Слишком длинный title",
    "TITLE_TOO_SHORT": "Слишком короткий title",
    "TITLE_EQUALS_H1": "Title совпадает с H1",
    "DESC_MULTIPLE": "Несколько meta description",
    "DESC_TOO_LONG": "Слишком длинный meta description",
    "DESC_TOO_SHORT": "Слишком короткий meta description",
    "H1_DUPLICATE": "Дубли H1 у разных страниц",
    "H1_TOO_LONG": "Слишком длинный H1",
    "H1_ALT_TEXT_ONLY": "H1 только из alt изображения",
    "H2_DUPLICATE": "Дубли H2",
    "H2_TOO_LONG": "Слишком длинный H2",
    "HEADING_BEFORE_H1": "Заголовок перед H1",
    "HEADING_IN_PAGE_CHROME": "Заголовок в шаблоне страницы",
    "HEADING_SKIP": "Пропущен уровень заголовка",
    "CANONICALISED": "Страница canonicalised на другой URL",
    "NOINDEX": "Директива noindex",
    "NOFOLLOW_PAGE": "Директива nofollow на странице",
    "THIN_CONTENT": "Мало текста на странице",
    "CONTENT_IN_IFRAME": "Контент в iframe",
    "LOW_TEXT_RATIO": "Низкая доля текста в HTML",
    "LOREM_IPSUM_PLACEHOLDER": "Текст-заглушка Lorem ipsum",
    "IMG_MISSING_ALT_ATTRIBUTE": "Изображения без атрибута alt",
    "IMG_ALT_TOO_LONG": "Слишком длинный alt изображения",
    "STRUCTURED_DATA_PARSE_ERROR": "Микроразметка не разбирается",
    "URL_TOO_LONG": "Слишком длинный URL",
    "URL_HAS_PARAMS": "URL с параметрами",
    "URL_NON_ASCII": "URL с не-ASCII символами",
    "URL_UPPERCASE": "URL с заглавными буквами",
    "ORPHAN_PAGE": "Страница без внутренних ссылок",
    "SLOW_RESPONSE": "Медленный ответ сервера",
    "LARGE_HTML": "Большой HTML",
    "HTML_BLOAT": "Раздутый HTML",
    "TITLE_TEMPLATED": "Шаблонный title",
    "URL_UNDERSCORES": "Подчёркивания в URL",
    "URL_MULTIPLE_SLASHES": "Несколько слешей подряд в URL",
    "URL_CONTAINS_SPACE": "Пробел в URL",
    "URL_REPETITIVE_PATH": "Повторяющиеся сегменты пути в URL",
    "URL_TRACKING_PARAMS": "Метки отслеживания в URL",
    "NOARCHIVE": "Директива noarchive",
    "NOSNIPPET": "Директива nosnippet",
    "NOIMAGEINDEX": "Директива noimageindex",
    "META_REFRESH_REDIRECT": "Редирект через meta refresh",
    "HTTP_REFRESH_REDIRECT": "Редирект через HTTP-заголовок refresh",
    "NOTRANSLATE": "Директива notranslate",
    "UNAVAILABLE_AFTER": "Директива unavailable_after",
    "CANONICAL_RELATIVE": "Относительный canonical",
    "CANONICAL_FRAGMENT": "Canonical с фрагментом",
    "CANONICAL_TO_REDIRECT": "Canonical на редирект",
    "CANONICAL_HOMEPAGE_GROUP": "Canonical группы страниц на главную",
    "UNLINKED_CANONICAL": "Canonical на URL без ссылок",
    "HREFLANG_MULTIPLE_ENTRIES": "Несколько hreflang для одного языка",
    "HREFLANG_MISSING_SELF_REFERENCE": "Hreflang без ссылки на себя",
    "HREFLANG_MISSING_XDEFAULT": "Hreflang без x-default",
    "HREFLANG_NOT_CANONICAL": "Hreflang на не-canonical URL",
    "PAGINATION_NONINDEXABLE": "Неиндексируемая страница пагинации",
    "NO_INTERNAL_OUTLINKS": "Нет внутренних исходящих ссылок",
    "HIGH_EXTERNAL_OUTLINKS": "Много внешних исходящих ссылок",
    "HIGH_OUTLINKS": "Много исходящих ссылок",
    "GENERIC_ANCHOR_TEXT": "Общий текст ссылок",
    "LOW_LINK_SCORE": "Низкий внутренний вес страницы",
    "ONLY_NOFOLLOW_INLINKS": "Входящие ссылки только nofollow",
    "ONLY_NONINDEXABLE_SOURCE_INLINKS": "Входящие ссылки только с неиндексируемых страниц",
    "DEEP_DISCOVERY_PATH": "Глубокий путь обнаружения",
    "DEEP_CLICK_DEPTH": "Большая глубина по кликам",
    "DUPLICATE_INTERNAL_LINK": "Повторяющиеся внутренние ссылки",
    "MISSING_CHARSET": "Нет кодировки charset",
    "MISSING_DOCTYPE": "Нет DOCTYPE",
    "VIEWPORT_MISSING": "Нет viewport",
    "UNSUPPORTED_PLUGIN": "Плагины в разметке не поддерживаются",
    "AJAX_CRAWLING_SCHEME_URL": "URL по устаревшей схеме AJAX-краулинга",
    "AJAX_CRAWLING_SCHEME_META_FRAGMENT": "Meta fragment устаревшей схемы AJAX-краулинга",
    "NO_COMPRESSION": "Нет сжатия ответа",
    "TITLE_OUTSIDE_HEAD": "Title вне head",
    "DESC_OUTSIDE_HEAD": "Meta description вне head",
    "CANONICAL_OUTSIDE_HEAD": "Canonical вне head",
    "DIRECTIVES_OUTSIDE_HEAD": "Директивы роботов вне head",
    "HREFLANG_OUTSIDE_HEAD": "Hreflang вне head",
    "HEAD_MISSING": "Нет элемента head",
    "HEAD_MULTIPLE": "Несколько элементов head",
    "BODY_MISSING": "Нет элемента body",
    "BODY_MULTIPLE": "Несколько элементов body",
    "INVALID_HEAD_ELEMENT": "Недопустимый элемент в head",
    "HEAD_NOT_FIRST": "Head не первый элемент",
    "INLINK_BOILERPLATE_ONLY": "Входящие ссылки только из шаблона",
    "LINK_INSIDE_HEADING": "Ссылка внутри заголовка",
    "IMAGE_LINK_WITHOUT_TEXT": "Ссылка-изображение без текста",
    "UNSAFE_CROSS_ORIGIN_LINK": "Небезопасная ссылка target=_blank",
    "PROTOCOL_RELATIVE_LINK": "Ссылка с protocol-relative адресом",
    "OUTLINK_TO_LOCALHOST": "Ссылка на localhost",
    "FOLLOW_AND_NOFOLLOW_INLINKS": "Входящие ссылки и follow, и nofollow",
    "FORM_URL_INSECURE": "Форма отправляется на небезопасный URL",
    "FORM_ON_HTTP_URL": "Форма на HTTP-странице",
    "BROKEN_BOOKMARK": "Битая ссылка на якорь",
    "DUPLICATE_ID": "Дубли id в HTML",
    "URL_SESSION_ID": "Идентификатор сессии в URL",
    "URL_TRAILING_SLASH_INCONSISTENT": "Непоследовательный слеш в конце URL",
    "PLACEHOLDER_MARKER": "Маркер-заглушка на странице",
    "MISSING_ABOUT_PAGE": "Нет страницы «О компании»",
    "MISSING_CONTACT_PAGE": "Нет страницы контактов",
    "MISSING_PRIVACY_POLICY": "Нет политики конфиденциальности",
    "MISSING_TERMS_PAGE": "Нет пользовательского соглашения",
    "YMYL_REVIEW_CANDIDATE": "Страница YMYL, нужна проверка экспертом",
}
# Core finding message (English, may embed values) -> Russian; matched fully, case-insensitive. Groups are re-inserted as {1}, {2}.
MESSAGES = (
    (r"Page returns a 4xx response \(broken page\)", "Битая страница, ответ 4xx"),
    (r"Page returns a 5xx response \(server error\)", "Ошибка сервера, ответ 5xx"),
    (r"Page has excessive crawl depth", "Страница слишком глубоко от главной по кликам"),
    (r"Duplicate meta description", "Дубль meta description"),
    (r"Meta description is missing", "Нет meta description"),
    (r"Multiple H1 headings on the page", "На странице несколько заголовков H1"),
    (
        r"This page declares a counterpart under a language and region code the counterpart does not confirm for itself",
        "Страница указывает альтернативу с языком и регионом, которые сама альтернатива не подтверждает",
    ),
    (
        r"Hreflang value is not a valid ISO 639-1 language / ISO 3166-1 region code",
        "Значение hreflang не является корректным кодом языка ISO 639-1 и региона ISO 3166-1",
    ),
    (r"URL uses HTTP instead of HTTPS", "Адрес использует HTTP вместо HTTPS"),
)
NO_TRANSLATION = "описание на языке ядра — в блоке «Исходный ответ ядра»"
# Core reason of a skipped check (English, free text) -> Russian. First matching pattern wins; unknown stays «причина не указана ядром».
SKIP_REASONS = (
    (r"missing export|export \S+ not available|no \S+ export", "нет выгрузки в источнике"),
    (r"requirements\.\S+ is false|disabled", "отключено в профиле"),
    (r"policy configured", "политика не задана в профиле"),
    (r"no stored html", "нет сохранённого HTML"),
    (r"sitemap", "нет данных sitemap"),
    (r"article-scope", "в скане нет страниц-статей"),
    (
        r"no .*column|has no \S+ column|link inventory carries no|link_attributes",
        "в выгрузке нет нужной колонки",
    ),
    (r"partial|unmeasured|incomplete", "данные скана неполные"),
    (r"no url with", "нет подходящих URL"),
    (r"not captured", "ответ цели не сохранён в скане"),
)
GROUP_ROWS = 10  # checks listed in «Крупнейшие группы»
COLUMNS = ("Адрес", "HTTP", "Доказательство", "Состояние")


def check_name(check, _message=None):
    """Use localized labels or the registry message; an unknown id stays identifiable."""
    return tr(
        CHECK_NAMES.get(check)
        or CORE_CHECKS.get(check, {}).get("message")
        or str(check).replace("_", " ").title()
    )


def issue_summary_arguments(scan_path):
    if not isinstance(scan_path, str) or not scan_path:
        raise ValueError("A retained scan is required")
    return [
        "scan-url-query",
        "--scan",
        scan_path,
        "--columns",
        "url",
        "--limit",
        "1",
        "--offset",
        "0",
        "--include-check-counts",
    ]


def message_ru(message):
    """Russian text of a core finding message, or the neutral fallback; the raw text lives only in the «Исходный ответ ядра» block."""
    text = (message or "").strip()
    for pattern, ru in MESSAGES:
        match = re.fullmatch(pattern, text, re.IGNORECASE)
        if match:
            return tr(ru).format(*match.groups()) if match.groups() else tr(ru)
    return tr(NO_TRANSLATION) if text else tr("Нет данных")


def skip_reason(reason):
    text = (reason or "").casefold()
    return tr(
        next(
            (ru for pattern, ru in SKIP_REASONS if re.search(pattern, text)),
            "причина не указана ядром",
        )
    )


def sample_checks(findings):
    """Checks present in the bounded sample: {check: {severity, message, items}} in first-seen order."""
    checks = {}
    for item in findings.get("items") or []:
        if isinstance(item, dict) and item.get("check"):
            entry = checks.setdefault(
                item["check"],
                {"severity": item.get("severity"), "message": item.get("message"), "items": []},
            )
            entry["items"].append(item)
    return checks


class FindingModel(QAbstractTableModel):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.rows = []

    def set_rows(self, rows):
        self.beginResetModel()
        self.rows = list(rows)
        self.endResetModel()

    def rowCount(self, parent=None):
        return 0 if parent is not None and parent.isValid() else len(self.rows)

    def columnCount(self, parent=None):
        return len(COLUMNS)

    def headerData(self, section, orientation, role=Qt.DisplayRole):
        return (
            tr(COLUMNS[section])
            if orientation == Qt.Horizontal and role == Qt.DisplayRole
            else None
        )

    def data(self, index, role=Qt.DisplayRole):
        if not index.isValid():
            return None
        item, column = self.rows[index.row()], index.column()
        if column == 3 and role == BADGE_ROLE:
            return "mut", tr("Найдена в скане")
        if role == Qt.DisplayRole:
            return (
                item.get("target_url") or tr("Нет данных"),
                "—",
                message_ru(item.get("message")),
                tr("Найдена в скане"),
            )[column]
        if role == Qt.ToolTipRole:
            return (
                item.get("target_url")
                if column == 0
                else tr("Нет данных")
                if column == 1
                else item.get("message")
                if column == 2
                else item.get("fingerprint")
                if column == 3
                else None
            )
        return None


class LaneStrip(QWidget):
    """Path of a finding: found → task → fixed → confirmed (canvas «lane»). The core reports only the first step, so the rest stay pending."""

    STEPS = (
        "Найдена в скане",
        "Задача программисту",
        "Исправлено по словам исполнителя",
        "Подтверждено перепроверкой",
    )

    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(8)
        self.chips = []
        for step_index, text in enumerate(self.STEPS, 1):
            if step_index > 1:
                rule = QFrame()
                rule.setFixedSize(24, 1)
                layout.addWidget(rule)
            chip = QLabel()
            chip.setFixedSize(22, 22)
            chip.setAlignment(Qt.AlignCenter)
            caption = QLabel(tr(text))
            caption.setProperty("text_style", "meta")
            if step_index > 1:
                chip.setToolTip(tr("Появится, когда будут задачи и журнал исправлений"))
            layout.addWidget(chip)
            layout.addWidget(caption)
            self.chips.append((step_index, chip, caption, rule if step_index > 1 else None))
        layout.addStretch(1)
        self.refresh()

    def refresh(self):
        roles = theming.roles()
        for step_index, chip, caption, rule in self.chips:
            if rule is not None:
                rule.setStyleSheet(f"background: {roles['outline']};")
            if step_index == 1:  # the only step the core reports
                chip.setPixmap(material_icon("check", roles["on_primary"]).pixmap(16, 16))
                chip.setStyleSheet(f"background: {roles['success']}; border-radius: 11px;")
                caption.setStyleSheet(f"color: {roles['success']}; font-weight: 500;")
            else:
                chip.setText(str(step_index))
                chip.setStyleSheet(
                    f"background: {roles['disabled_bg']}; color: {roles['text_muted']}; border-radius: 11px;"
                )
                caption.setStyleSheet(f"color: {roles['text_muted']};")


class IssuesScreen(Screen):
    slot = "issues"
    watches = ()

    def __init__(self, host):
        super().__init__(host)
        self.severity = "all"
        self.selected = None
        self.checks = {}
        self.check_counts = {}
        self.signature = None
        self.panel_state = None
        self.issue_summary = None
        self._issue_summary_key = None
        self._issue_summary_pending = None
        self.issue_summary_job = CoreJob(host, self)
        self.issue_summary_job.done.connect(self._issue_summary_done)
        self.issue_summary_job.failed.connect(self._issue_summary_failed)
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        self.stack = QStackedWidget()
        root.addWidget(self.stack)
        self.content = QWidget()
        self.stack.addWidget(self.content)
        self.state_holder = QWidget()
        self.state_layout = QVBoxLayout(self.state_holder)
        self.stack.addWidget(self.state_holder)
        body = QHBoxLayout(self.content)
        body.setContentsMargins(0, 0, 0, 0)
        body.setSpacing(0)
        body.addWidget(self._build_list())
        body.addWidget(self._build_detail(), 1)
        i18n.signals.changed.connect(self._language_changed)
        self.refresh()

    def _data_changed(self, kind):
        if kind in {"project", "scans", "scan_status"} and self.isVisible():
            self.refresh()

    def showEvent(self, event):
        super().showEvent(event)
        self.refresh()

    @staticmethod
    def _scan_key(scan):
        evidence = scan.get("evidence") or {}
        return (
            scan.get("path"),
            scan.get("uuid"),
            scan.get("evidence_revision") or evidence.get("evidence_revision"),
        )

    def _request_issue_summary(self, scan):
        if not self.isVisible():
            return
        key = self._scan_key(scan)
        if key == self._issue_summary_key or (
            self._issue_summary_pending is not None and self._issue_summary_pending[1] == key
        ):
            return
        token = object()
        self._issue_summary_pending = (token, key)
        self.issue_summary = None
        self.issue_summary_job.start(issue_summary_arguments(scan.get("path")), token)

    def _issue_summary_done(self, token, payload):
        if self._issue_summary_pending is None or token is not self._issue_summary_pending[0]:
            return
        _token, key = self._issue_summary_pending
        self._issue_summary_pending = None
        summary = payload.get("issue_summary") or (payload.get("evidence") or {}).get(
            "issue_summary"
        )
        if payload.get("ok") is not True or not isinstance(summary, dict):
            self._issue_summary_key = None
            return
        self._issue_summary_key = key
        self.issue_summary = summary
        scan = selected_scan(self.host) or {}
        if self._scan_key(scan) == key:
            self._merge_issue_summary()
            self._fill_all()

    def _issue_summary_failed(self, token, _reason):
        if self._issue_summary_pending is not None and token is self._issue_summary_pending[0]:
            self._issue_summary_pending = None
            self._issue_summary_key = None

    def _merge_issue_summary(self):
        summary = self.issue_summary or {}
        by_check = summary.get("by_check")
        if not isinstance(by_check, dict):
            scan = selected_scan(self.host) or {}
            by_check = ((scan.get("evidence") or {}).get("findings") or {}).get("by_check")
        self.check_counts = (
            {
                key: value
                for key, value in by_check.items()
                if isinstance(key, str) and type(value) is int and value >= 0
            }
            if isinstance(by_check, dict)
            else {}
        )
        for check, count in self.check_counts.items():
            if count > 0 and check not in self.checks:
                self.checks[check] = {
                    "severity": CORE_CHECKS.get(check, {}).get("severity") or "notice",
                    "message": "",
                    "items": [],
                }
        for row in summary.get("checks") or []:
            if not isinstance(row, dict) or not isinstance(row.get("id"), str):
                continue
            check = row["id"]
            count = row.get("findings")
            if type(count) is not int or count <= 0:
                continue
            entry = self.checks.setdefault(
                check,
                {
                    "severity": row.get("severity") or "notice",
                    "message": row.get("message") or "",
                    "items": [],
                },
            )
            entry["severity"] = row.get("severity") or entry["severity"]
            entry["state"] = row.get("state")
            entry["findings"] = count
            entry["reason"] = row.get("reason")

    # ---- construction ----------------------------------------------------------------------------------------
    def _build_list(self):
        side = QFrame()
        side.setProperty("aside", False)
        side.setFixedWidth(330)
        layout = QVBoxLayout(side)
        layout.setContentsMargins(12, 12, 12, 8)
        layout.setSpacing(8)
        self.search = QLineEdit()
        self.search.setPlaceholderText(tr("Найти проверку…"))
        self.search.setAccessibleName(tr("Поиск проблемы"))
        self.search.setClearButtonEnabled(True)
        self.search.textChanged.connect(lambda _t: self._fill_list())
        layout.addWidget(self.search)
        pills = QGridLayout()
        pills.setSpacing(4)
        self.pills = {}
        for slot, (key, label) in enumerate(
            (("all", "Все"), *[(k, n) for k, n, _i, _c in SEVERITIES])
        ):
            button = QToolButton()
            button.setProperty("pill", "group")
            button.setCheckable(True)
            button.setChecked(key == "all")
            button.clicked.connect(lambda _c=False, k=key: self._set_severity(k))
            self.pills[key] = (button, label)
            pills.addWidget(button, slot // 2, slot % 2)
        layout.addLayout(pills)
        self.list_note = QLabel()
        self.list_note.setProperty("text_style", "meta")
        self.list_note.setWordWrap(True)
        layout.addWidget(self.list_note)
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        holder = QWidget()
        self.list_layout = QVBoxLayout(holder)
        self.list_layout.setContentsMargins(0, 0, 0, 0)
        self.list_layout.setSpacing(1)
        self.list_layout.addStretch(1)
        scroll.setWidget(holder)
        layout.addWidget(scroll, 1)
        return side

    def _build_detail(self):
        pane = QWidget()
        layout = QVBoxLayout(pane)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        head = QFrame()
        head.setProperty("page_bar", True)
        head_layout = QVBoxLayout(head)
        head_layout.setContentsMargins(20, 16, 20, 12)
        head_layout.setSpacing(10)
        top = QHBoxLayout()
        top.setSpacing(10)
        self.icon = MaterialIconLabel("rule", 24, color="role:text_2")
        top.addWidget(self.icon)
        self.title = QLabel()
        self.title.setProperty("text_style", "dialog")
        self.title.setWordWrap(True)
        top.addWidget(self.title, 1)
        self.open_url = QPushButton(tr("Открыть в URL"))
        self.open_url.setIcon(material_icon("table_view"))
        self.open_url.clicked.connect(self._open_in_url)
        self.to_task = QPushButton(tr("В задачу"))
        self.to_task.setProperty("role", "primary")
        self.to_task.setIcon(material_icon("assignment_add", theming.roles()["on_primary"]))
        self.to_task.setEnabled(False)
        self.to_task.setToolTip(unavailable_tip("Создание задач из проверок"))
        top.addWidget(self.open_url)
        top.addWidget(self.to_task)
        head_layout.addLayout(top)
        self.desc = QLabel()
        self.desc.setWordWrap(True)
        head_layout.addWidget(self.desc)
        self.lane_found = LaneStrip()
        head_layout.addWidget(self.lane_found)
        lane = QHBoxLayout()
        lane.setSpacing(8)
        lane.addWidget(
            waiting_badge(926, "Путь находки: задача, исправление и подтверждение перепроверкой")
        )
        lane.addStretch(1)
        self.recheck = QPushButton(tr("Перепроверить"))
        self.recheck.setIcon(material_icon("replay"))
        self.recheck.setEnabled(False)
        self.recheck.setToolTip(unavailable_tip("Перепроверка по проверке"))
        lane.addWidget(self.recheck)
        head_layout.addLayout(lane)
        layout.addWidget(head)
        self.pages = QStackedWidget()
        layout.addWidget(self.pages, 1)
        self.overview = self._build_overview()
        self.pages.addWidget(self.overview)
        self.model = FindingModel(self)
        self.table = style_table(QTableView())
        self.table.setModel(self.model)
        self.table.setItemDelegateForColumn(3, BadgeDelegate(self.table))
        self.table.verticalHeader().setDefaultSectionSize(40)
        self.table.setFrameShape(QFrame.NoFrame)
        self.table.horizontalHeader().setSectionResizeMode(0, QHeaderView.Stretch)
        self.table.horizontalHeader().setSectionResizeMode(2, QHeaderView.Stretch)
        self.table.horizontalHeader().setStretchLastSection(False)
        for column, width in ((1, 90), (3, 150)):
            self.table.horizontalHeader().setSectionResizeMode(column, QHeaderView.Fixed)
            self.table.setColumnWidth(column, width)
        self.table.doubleClicked.connect(lambda index: self._open_row(index.row()))
        self.pages.addWidget(self.table)
        self.raw_toggle = QToolButton()
        self.raw_toggle.setCheckable(True)
        self.raw_toggle.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
        self.raw_toggle.setStyleSheet(
            "QToolButton { border: none; background: transparent; padding: 4px 12px; }"
        )
        self.raw_toggle.toggled.connect(lambda _on: self._sync_raw())
        layout.addWidget(self.raw_toggle, 0, Qt.AlignLeft)
        self.raw = QLabel()
        self.raw.setWordWrap(True)
        self.raw.setContentsMargins(12, 0, 12, 4)
        self.raw.setTextInteractionFlags(Qt.TextSelectableByMouse)
        self.raw.setVisible(False)
        layout.addWidget(self.raw)
        self.foot = QLabel()
        self.foot.setProperty("text_style", "meta")
        self.foot.setContentsMargins(12, 8, 12, 8)
        self.foot.setWordWrap(True)
        layout.addWidget(self.foot)
        return pane

    def _build_overview(self):
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.NoFrame)
        body = QWidget()
        layout = QVBoxLayout(body)
        layout.setContentsMargins(20, 16, 20, 16)
        layout.setSpacing(10)
        self.partial_note = QFrame()
        self.partial_note.setProperty("note", "warn")
        note_layout = QHBoxLayout(self.partial_note)
        note_layout.addWidget(
            MaterialIconLabel("warning", 18, color="role:warning"), 0, Qt.AlignTop
        )
        self.partial_text = QLabel()
        self.partial_text.setWordWrap(True)
        note_layout.addWidget(self.partial_text, 1)
        self.resume = QPushButton(tr("Продолжить скан"))
        self.resume.clicked.connect(lambda _c=False: self.host.resume_selected_scan())
        note_layout.addWidget(self.resume)
        layout.addWidget(self.partial_note)
        layout.addWidget(section_label("Находки по важности"))
        self.bars = {}
        for key, label, icon, colour in SEVERITIES:
            line = QHBoxLayout()
            line.setSpacing(10)
            line.addWidget(MaterialIconLabel(icon, 18, color=f"role:{colour}"))
            caption = QLabel(tr(label))
            caption.setMinimumWidth(110)
            line.addWidget(caption)
            bar = StackBar(10)
            line.addWidget(bar, 1)
            count = QLabel()
            count.setMinimumWidth(80)
            count.setAlignment(Qt.AlignRight | Qt.AlignVCenter)
            line.addWidget(count)
            layout.addLayout(line)
            self.bars[key] = (bar, count, caption)
        self.total = QLabel()
        self.total.setProperty("text_style", "meta")
        layout.addWidget(self.total)
        layout.addWidget(section_label("Крупнейшие группы по проверкам"))
        self.groups_box = QVBoxLayout()
        self.groups_box.setSpacing(6)
        layout.addLayout(self.groups_box)
        self.skipped_toggle = QToolButton()
        self.skipped_toggle.setCheckable(True)
        self.skipped_toggle.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
        self.skipped_toggle.setStyleSheet(
            "QToolButton { border: none; background: transparent; text-align: left; padding: 4px 0; }"
        )
        self.skipped_toggle.toggled.connect(lambda _on: self._sync_skipped())
        layout.addWidget(self.skipped_toggle, 0, Qt.AlignLeft)
        self.skipped = QLabel()
        self.skipped.setWordWrap(True)
        self.skipped.setTextInteractionFlags(Qt.TextSelectableByMouse)
        self.skipped.setVisible(False)
        layout.addWidget(self.skipped)
        layout.addStretch(1)
        scroll.setWidget(body)
        return scroll

    def resizeEvent(self, event):
        super().resizeEvent(event)
        narrow = event.size().width() < 1000
        for button, text in (
            (self.open_url, "Открыть в URL"),
            (self.to_task, "В задачу"),
            (self.recheck, "Перепроверить"),
        ):
            button.setText("" if narrow else tr(text))
            button.setToolTip(button.toolTip() or tr(text))
        for step_index, chip, caption, rule in self.lane_found.chips:
            if step_index > 1:
                for widget in (chip, caption, rule):
                    widget.setVisible(not narrow)
        for column in (
            1,
            3,
        ):  # constant «—» and «Найдена в скане» only steal width from address and evidence when narrow
            self.table.setColumnHidden(column, narrow)

    # ---- state -----------------------------------------------------------------------------------------------
    def refresh(self):
        host = self.host
        scan = selected_scan(host)
        if not host.project_directory:
            return set_panel_state(
                self,
                "none",
                no_project_panel(host, "Откройте проект, чтобы увидеть проблемы его сканов."),
            )
        if scan is None:
            self._issue_summary_key = None
            self.issue_summary = None
            if host._project_loading:
                return set_panel_state(
                    self,
                    "loading",
                    StatePanel(
                        "loading", "Чтение проекта", "Находки появятся после чтения сканов."
                    ),
                )
            return set_panel_state(
                self,
                "noscan",
                StatePanel(
                    "empty",
                    "Нет выбранного скана",
                    "Проблемы показываются по сохранённому скану проекта.",
                    action=("Новый скан", host.scan_preview),
                ),
            )
        self._request_issue_summary(scan)
        findings = (scan.get("evidence") or {}).get("findings") or {}
        if findings.get("state") != "available":
            reason = findings.get("reason") or "Ядро не вернуло находок для этого скана"
            return set_panel_state(
                self,
                "partial",
                StatePanel(
                    "partial", "Находки недоступны", f"{reason}. {tr('Это не «0 проблем».')}"
                ),
            )
        if findings.get("total") == 0:
            skipped = (scan.get("evidence") or {}).get("skipped_checks") or []
            skipped = list(skipped)
            skipped.extend(
                row
                for row in (self.issue_summary or {}).get("checks", [])
                if isinstance(row, dict)
                and row.get("state") in {"skipped", "disabled", "unavailable"}
            )
            unique_skipped = {}
            for row in skipped:
                if isinstance(row, dict) and isinstance(row.get("id"), str):
                    unique_skipped[row["id"]] = row
            panel = StatePanel(
                "empty",
                "Проблем не найдено",
                "Находок нет по выбранному скану. Это измеренный ноль: проверки, которые не запускались, перечислены отдельно.",
            )
            panel.findChild(MaterialIconLabel).set_material_icon("verified", "role:success")
            if unique_skipped:  # skipped states are measured separately from the zero finding count
                badge = QLabel(trf("{n} проверок не измерялось", n=len(unique_skipped)))
                badge.setProperty("badge", "mut")
                panel.layout().addWidget(badge, 0, Qt.AlignHCenter)
            return set_panel_state(self, "zero", panel)
        set_panel_state(self, None)
        signature = (
            scan.get("path"),
            findings.get("total"),
            tuple((findings.get("by_severity") or {}).items()),
            scan.get("crawl_partial"),
        )
        if signature != self.signature:
            self.signature = signature
            self.checks = sample_checks(findings)
            if self.selected not in self.checks:
                self.selected = None
        self._merge_issue_summary()
        self._fill_all()

    def _language_changed(self, _language):
        try:
            self.signature = None
            self.refresh()
        except RuntimeError:  # the screen was deleted (tests, shutdown)
            pass

    def _set_severity(self, key):
        self.severity = key
        for name, (button, _label) in self.pills.items():
            button.setChecked(name == key)
        self._fill_list()

    def _fill_all(self):
        scan = selected_scan(self.host) or {}
        evidence = scan.get("evidence") or {}
        findings = evidence.get("findings") or {}
        by_severity = findings.get("by_severity") or {}
        total = findings.get("total")
        for key, (button, label) in self.pills.items():
            count = total if key == "all" else by_severity.get(key, 0)
            button.setText(f"{tr(label)}  {number(count)}")
        peak = max([v for v in by_severity.values() if type(v) is int] or [0])
        for key, (bar, count, _caption) in self.bars.items():
            value = by_severity.get(key, 0)
            bar.set_parts(
                [
                    (
                        value,
                        theming.roles()[
                            {"critical": "error", "warning": "warning", "notice": "text_3"}[key]
                        ],
                    ),
                    (max(0, peak - value), theming.roles()["disabled_bg"]),
                ]
                if peak
                else []
            )
            count.setText(number(value))
        self.total.setText(trf("Всего находок: {n}", n=number(total)))
        partial = scan.get("crawl_partial") is True or scan.get("lifecycle") in (
            "interrupted",
            "running",
        )
        self.partial_note.setVisible(partial)
        self.partial_text.setText(
            tr(
                "Скан частичный. Находки — только по обойдённой части; «нет данных» не равно нулю. Не закрывайте задачи по неполному скану."
            )
        )
        self.resume.setVisible(partial and hasattr(self.host, "resume_selected_scan"))
        skipped = [item for item in evidence.get("skipped_checks") or [] if isinstance(item, dict)]
        self.skipped_rows = [
            f"{check_name(item.get('id'))} — {skip_reason(item.get('reason'))}" for item in skipped
        ]
        self._sync_skipped()
        if self.check_counts:
            self.list_note.setText(
                trf(
                    "Проверки скана. Число рядом с проверкой — находки по всему скану от ядра; список URL — из первых {n} находок.",
                    n=len(findings.get("items") or []),
                )
            )
        else:
            self.list_note.setText(
                trf(
                    "Проверки из первых {n} находок. Число рядом с проверкой — находки в этой выборке, не во всём скане.",
                    n=len(findings.get("items") or []),
                )
            )
        self._fill_groups()
        self._fill_list()
        self._fill_detail()

    def _fill_groups(self):
        """Top checks of the whole scan by finding count (the core's per-check counters), as bars."""
        while self.groups_box.count():
            item = self.groups_box.takeAt(0)
            if item.widget() is not None:
                item.widget().deleteLater()
        ranked = sorted(self.check_counts.items(), key=lambda pair: (-pair[1], pair[0]))
        ranked = [pair for pair in ranked if pair[1] > 0][:GROUP_ROWS]
        if not ranked:
            note = QLabel(tr("Счётчики по проверкам ядро не вернуло"))
            note.setProperty("text_style", "meta")
            self.groups_box.addWidget(note)
            return
        peak = ranked[0][1]
        for check, count in ranked:
            line = QWidget()
            row = QHBoxLayout(line)
            row.setContentsMargins(0, 0, 0, 0)
            row.setSpacing(10)
            caption = QLabel(check_name(check))
            caption.setMinimumWidth(220)
            caption.setToolTip(check)
            row.addWidget(caption)
            bar = StackBar(10)
            severity = (self.checks.get(check) or {}).get("severity") or "notice"
            role = {"critical": "error", "warning": "warning"}.get(severity, "text_3")
            bar.set_parts(
                [(count, theming.roles()[role]), (peak - count, theming.roles()["disabled_bg"])]
            )
            row.addWidget(bar, 1)
            value = QLabel(number(count))
            value.setMinimumWidth(60)
            value.setAlignment(Qt.AlignRight | Qt.AlignVCenter)
            row.addWidget(value)
            self.groups_box.addWidget(line)

    def _sync_raw(self):
        entry = self.checks.get(self.selected)
        self.raw_toggle.setVisible(entry is not None)
        opened = self.raw_toggle.isChecked() and entry is not None
        self.raw_toggle.setText(tr("Исходный ответ ядра"))
        self.raw_toggle.setArrowType(Qt.DownArrow if opened else Qt.RightArrow)
        lines = []
        if entry is not None:
            lines = [
                f"check: {self.selected}",
                f"severity: {entry['severity']}",
                f"message: {entry['message']}",
            ]
            lines += [
                f"{item.get('target_url')} — {item.get('message')} [{item.get('fingerprint')}]"
                for item in entry["items"][:20]
            ]
        self.raw.setText("\n".join(lines))
        self.raw.setVisible(opened)

    def _sync_skipped(self):
        rows = getattr(self, "skipped_rows", [])
        opened = self.skipped_toggle.isChecked() and bool(rows)
        self.skipped_toggle.setText(
            trf("Не выполнялось: {n}", n=len(rows)) if rows else tr("Не выполнялось: нет данных")
        )
        self.skipped_toggle.setEnabled(bool(rows))
        self.skipped_toggle.setArrowType(Qt.DownArrow if opened else Qt.RightArrow)
        self.skipped.setText("\n".join(rows))
        self.skipped.setVisible(opened)

    def _fill_list(self):
        while self.list_layout.count() > 1:
            item = self.list_layout.takeAt(0)
            if item.widget():
                item.widget().hide()  # deleteLater alone leaves it painted until the event loop runs
                item.widget().deleteLater()
        needle = self.search.text().strip().casefold()
        self.row_buttons = {}
        for check, entry in self.checks.items():
            if self.severity != "all" and entry["severity"] != self.severity:
                continue
            name = check_name(check, entry["message"])
            if needle and needle not in f"{name} {check}".casefold():
                continue
            icon, colour = next(
                ((i, c) for k, _n, i, c in SEVERITIES if k == entry["severity"]),
                ("help", "text_muted"),
            )
            button = QToolButton()
            button.setProperty("pill", "group")
            button.setCheckable(True)
            button.setChecked(check == self.selected)
            button.setToolButtonStyle(Qt.ToolButtonTextBesideIcon)
            button.setIcon(material_icon(icon, f"role:{colour}"))
            count = self.check_counts.get(check, len(entry["items"]))
            button.setText(f"{name}  ·  {count}")
            button.setToolTip(
                f"{check} · {tr('число находок этой проверки в скане') if check in self.check_counts else tr('число находок этой проверки в выборке')}"
            )
            button.setSizePolicy(button.sizePolicy().Expanding, button.sizePolicy().Fixed)
            button.setMinimumHeight(40)
            button.clicked.connect(lambda _c=False, c=check: self._select(c))
            button.setContextMenuPolicy(Qt.CustomContextMenu)
            button.customContextMenuRequested.connect(
                lambda point, c=check, b=button: self._finding_menu(c, b, point)
            )
            self.list_layout.insertWidget(self.list_layout.count() - 1, button)
            self.row_buttons[check] = button
        if not self.row_buttons:
            empty = QLabel(tr("В первых находках нет проверок с такой важностью или названием."))
            empty.setProperty("text_style", "meta")
            empty.setWordWrap(True)
            self.list_layout.insertWidget(0, empty)

    def _finding_menu(self, check, button, point):
        menu = menus.fill_menu(
            QMenu(button),
            [
                menus.entry(
                    "open_in_new",
                    "Открыть проверку",
                    lambda: self._select(check) if self.selected != check else None,
                ),
                menus.entry(
                    "content_copy",
                    "Копировать код проверки",
                    lambda: QApplication.clipboard().setText(check),
                ),
                None,
                menus.unavailable("table_view", "Показать URL проверки"),
                menus.unavailable("assignment_add", "Создать задачу"),
                menus.unavailable("smart_toy", "Спросить агента"),
                None,
                menus.unavailable("visibility_off", "Скрыть: не применимо"),
                menus.unavailable("download", "Экспорт CSV"),
            ],
        )
        menu.exec_(button.mapToGlobal(point))

    def _select(self, check):
        self.selected = None if check == self.selected else check
        for key, button in self.row_buttons.items():
            button.setChecked(key == self.selected)
        self._fill_detail()

    def _fill_detail(self):
        entry = self.checks.get(self.selected)
        if entry is None:
            self.pages.setCurrentIndex(0)
            self.title.setText(tr("Проблемы скана"))
            self.icon.set_material_icon("rule", "role:text_2")
            self.desc.setText(
                tr("Выберите проверку слева: в списке проверки из первых находок скана.")
            )
            self.lane_found.refresh()
            self.open_url.setEnabled(False)
            self.open_url.setToolTip(tr("Выберите проверку"))
            self.foot.setText("")
            self.foot.setVisible(False)
            self._sync_raw()
            return
        self.pages.setCurrentIndex(1)
        self.foot.setVisible(True)
        name = check_name(self.selected, entry["message"])
        icon, colour = next(
            ((i, c) for k, _n, i, c in SEVERITIES if k == entry["severity"]), ("help", "text_muted")
        )
        self.title.setText(name)
        self.icon.set_material_icon(icon, f"role:{colour}")
        self.desc.setText(message_ru(entry["message"]))
        self._sync_raw()
        self.lane_found.refresh()
        self.model.set_rows(entry["items"])
        urls = [i.get("target_url") for i in entry["items"] if i.get("target_url")]
        self.open_url.setEnabled(bool(urls))
        self.open_url.setToolTip(
            trf(
                "Откроет URL из выборки находок этой проверки ({n}). Фильтр по проверке для всего скана пока недоступен",
                n=len(urls),
            )
        )
        count = self.check_counts.get(self.selected)
        if type(count) is int:
            self.foot.setText(
                trf(
                    "{total} находок по всему скану; {sample} URL показаны из выборки.",
                    total=number(count),
                    sample=len(entry["items"]),
                )
            )
        else:
            self.foot.setText(
                trf(
                    "{n} URL в выборке находок этой проверки; число по всему скану недоступно.",
                    n=len(entry["items"]),
                )
            )

    # ---- actions ---------------------------------------------------------------------------------------------
    def _open_in_url(self):
        entry = self.checks.get(self.selected)
        if not entry:
            return
        urls = [i["target_url"] for i in entry["items"] if i.get("target_url")][:100]
        label = trf(
            "Проверка «{name}» · выборка {n} URL",
            name=check_name(self.selected, entry["message"]),
            n=len(urls),
        )
        self.host.open_url_filtered(label, [{"column": "url", "op": "in", "value": urls}])

    def _open_row(self, row):
        item = self.model.rows[row] if 0 <= row < len(self.model.rows) else None
        if item and item.get("target_url"):
            self.host.open_url_filtered(
                trf("URL · {url}", url=item["target_url"]),
                [{"column": "url", "op": "eq", "value": item["target_url"]}],
            )


__all__ = ["IssuesScreen", "parse_time"]
