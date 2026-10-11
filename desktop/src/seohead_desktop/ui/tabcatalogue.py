"""Panel contracts observed in SF 19.8 and explicit SEOHEAD projections.

The catalogue describes presentation, not implemented analyzer capabilities.
An adapter must supply rows, coverage, and supported filters for each panel.
Reference thresholds are the inspected SF configuration, never SEOHEAD defaults.
"""

import re
from dataclasses import dataclass


@dataclass(frozen=True)
class TabSpec:
    id: str
    title: str
    pane: str
    columns: tuple[tuple[str, str], ...]
    filters: tuple[str, ...]
    evidence: str
    intent: str = "select_record"


def filter_id(label):
    return re.sub(r"[^a-z0-9]+", "_", label.lower()).strip("_")


def _tab(id, title, pane, columns, filters="All", evidence="", intent="select_record"):
    return TabSpec(
        id,
        title,
        pane,
        tuple(tuple(value.split("=", 1)) for value in columns.split("|")),
        tuple(filters.split("|")),
        evidence,
        intent,
    )


URL = "url=Address"
HTTP = URL + "|type=Content Type|status=Status Code|status_text=Status"
INDEX = "indexability=Indexability|indexability_status=Indexability Status"
CANONICAL = "canonical=Canonical Link Element 1|http_canonical=HTTP Canonical"
ROBOTS = "meta_robots=Meta Robots 1|x_robots_tag=X-Robots-Tag 1"
PAGINATION = 'next_url=rel="next" 1|prev_url=rel="prev" 1|http_next=HTTP rel="next" 1|http_prev=HTTP rel="prev" 1'
MIME_FILTERS = "All|HTML|JavaScript|CSS|Images|PDF|Flash|Other|Unknown"
LINK_COLUMNS = (
    "type=Type|from_url=From|to_url=To|anchor_text=Anchor Text|alt=Alt Text|follow=Follow|"
    "target=Target|rel=Rel|status=Status Code|status_text=Status|path_type=Path Type|"
    "link_path=Link Path|position=Link Position|origin=Link Origin"
)
NATIVE = (
    "Bounded retained scan-inspect pages; additional fields require retained evidence."
)
FINDINGS = "Retained findings and matched URL evidence; never calculate global findings from a page."
LINKS = "Retained link/resource evidence and direction-aware core query."
PROJECT = "Existing project CLI projection; no replacement project store."


MAIN_TABS = (
    _tab(
        "internal",
        "Internal",
        "main",
        HTTP
        + "|"
        + INDEX
        + "|title=Title 1|issues=Findings|crawl_depth=Crawl Depth|inlinks=Inlinks|outlinks=Outlinks",
        MIME_FILTERS,
        NATIVE,
        "select_url",
    ),
    _tab(
        "external",
        "External",
        "main",
        HTTP + "|crawl_depth=Crawl Depth|inlinks=Inlinks",
        MIME_FILTERS,
        LINKS,
        "select_url",
    ),
    _tab(
        "security",
        "Security",
        "main",
        HTTP + "|http_version=HTTP Version|" + INDEX + "|" + CANONICAL + "|" + ROBOTS,
        "All|HTTP URLs|HTTPS URLs|Mixed Content|Form URL Insecure|Form on HTTP URL|Unsafe Cross-Origin Links|"
        "Protocol-Relative Resource Links|Missing HSTS Header|Missing Content-Security-Policy Header|"
        "Missing X-Content-Type-Options Header|Missing X-Frame-Options Header|Missing Secure Referrer-Policy Header|Bad Content Type",
        "Retained security findings and response headers; response headers are dynamic columns.",
        "select_url",
    ),
    _tab(
        "response_codes",
        "Response Codes",
        "main",
        HTTP
        + "|"
        + INDEX
        + "|inlinks=Inlinks|response_time=Response Time|redirect_url=Redirect URL|redirect_type=Redirect Type",
        "All|Blocked by Robots.txt|Blocked Resource|No Response|Success (2xx)|Redirection (3xx)|"
        "Redirection (JavaScript)|Redirection (Meta Refresh)|Client Error (4xx)|Server Error (5xx)|"
        "Internal All|Internal Blocked by Robots.txt|Internal Blocked Resource|Internal No Response|"
        "Internal Success (2xx)|Internal Redirection (3xx)|Internal Redirection (JavaScript)|"
        "Internal Redirection (Meta Refresh)|Internal Redirect Chain|Internal Redirect Loop|"
        "Internal Client Error (4xx)|Internal Server Error (5xx)|External All|External Blocked by Robots.txt|"
        "External Blocked Resource|External No Response|External Success (2xx)|External Redirection (3xx)|"
        "External Redirection (JavaScript)|External Redirection (Meta Refresh)|External Client Error (4xx)|External Server Error (5xx)",
        NATIVE,
        "select_url",
    ),
    _tab(
        "url",
        "URL",
        "main",
        HTTP
        + "|"
        + INDEX
        + "|hash=Hash|length=Length|canonical=Canonical Link Element 1|encoded_url=URL Encoded Address",
        "All|Non ASCII Characters|Underscores|Uppercase|Multiple Slashes|Repetitive Path|Contains Space|"
        "Internal Search|Parameters|Broken Bookmark|GA Tracking Parameters|Over 115 Characters",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "page_titles",
        "Page Titles",
        "main",
        URL
        + "|occurrences=Occurrences|title=Title 1|title_length=Title 1 Length|title_pixels=Title 1 Pixel Width|"
        + INDEX,
        "All|Missing|Duplicate|Over 60 Characters|Below 30 Characters|Over 561 Pixels|Below 200 Pixels|Same as H1|Multiple|Outside <head>",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "meta_description",
        "Meta Description",
        "main",
        URL
        + "|occurrences=Occurrences|description=Meta Description 1|description_length=Meta Description 1 Length|description_pixels=Meta Description 1 Pixel Width|"
        + INDEX,
        "All|Missing|Duplicate|Over 155 Characters|Below 70 Characters|Over 985 Pixels|Below 400 Pixels|Multiple|Outside <head>",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "meta_keywords",
        "Meta Keywords",
        "main",
        URL
        + "|occurrences=Occurrences|meta_keywords=Meta Keywords 1|meta_keywords_length=Meta Keywords 1 Length|"
        + INDEX,
        "All|Missing|Duplicate|Multiple",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "h1",
        "H1",
        "main",
        URL + "|occurrences=Occurrences|h1=H1-1|h1_length=H1-1 Length|" + INDEX,
        "All|Missing|Duplicate|Over 70 Characters|Multiple|Alt Text in H1|Non-Sequential",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "h2",
        "H2",
        "main",
        URL
        + "|occurrences=Occurrences|h2=H2-1|h2_length=H2-1 Length|h2_second=H2-2|h2_second_length=H2-2 Length|"
        + INDEX,
        "All|Missing|Duplicate|Over 70 Characters|Multiple|Non-Sequential",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "content",
        "Content",
        "main",
        URL
        + "|word_count=Word Count|sentence_count=Sentence Count|words_per_sentence=Average Words Per Sentence|reading_ease=Flesch Reading Ease Score|readability=Readability|closest_match=Closest Similarity Match|near_duplicates=No. Near Duplicates|language_errors=Total Language Errors|spelling_errors=Spelling Errors|grammar_errors=Grammar Errors|language=Language|hash=Hash|"
        + INDEX,
        "All|Exact Duplicates|Near Duplicates|Low Content Pages|Soft 404 Pages|Spelling Errors|Grammar Errors|Readability Difficult|Readability Very Difficult|Lorem Ipsum Placeholder",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "images",
        "Images",
        "main",
        URL
        + "|type=Content Type|size=Size|image_inlinks=IMG Inlinks|"
        + INDEX
        + "|dimensions=Dimensions",
        "All|Over 100 KB|Missing Alt Text|Missing Alt Attribute|Alt Text Over 100 Characters|Incorrectly Sized Images|Missing Size Attributes|Background Images",
        LINKS,
        "select_url",
    ),
    _tab(
        "canonicals",
        "Canonicals",
        "main",
        URL
        + "|occurrences=Occurrences|"
        + INDEX
        + "|"
        + CANONICAL
        + "|"
        + ROBOTS
        + "|"
        + PAGINATION,
        "All|Contains Canonical|Self Referencing|Canonicalised|Missing|Multiple|Multiple Conflicting|Non-Indexable Canonical|Canonical Is Relative|Unlinked|Outside <head>",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "pagination",
        "Pagination",
        "main",
        URL + "|" + INDEX + "|" + PAGINATION + "|" + CANONICAL + "|" + ROBOTS,
        "All|Contains Pagination|First Page|Paginated 2+ Pages|Pagination URL Not in Anchor Tag|Non-200 Pagination URLs|Unlinked Pagination URLs|Non-Indexable|Multiple Pagination URLs|Pagination Loop|Sequence Error",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "directives",
        "Directives",
        "main",
        URL
        + "|occurrences=Occurrences|"
        + ROBOTS
        + "|meta_refresh=Meta Refresh 1|"
        + CANONICAL,
        "All|Index|Noindex|Follow|Nofollow|None|NoArchive|NoSnippet|Max-Snippet|Max-Image-Preview|Max-Video-Preview|NoODP|NoYDIR|NoImageIndex|NoTranslate|Unavailable_After|Refresh|Outside <head>",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "hreflang",
        "Hreflang",
        "main",
        URL
        + "|title=Title 1|occurrences=Occurrences|html_hreflang=HTML hreflang|http_hreflang=HTTP hreflang|sitemap_hreflang=Sitemap hreflang|"
        + INDEX,
        "All|Contains hreflang|Non-200 hreflang URLs|Unlinked hreflang URLs|Missing Return Links|Non-Canonical Return Links|Inconsistent Language & Region Return Links|Noindex Return Links|Incorrect Language & Region Codes|Multiple Entries|Missing Self Reference|Not Using Canonical|Missing X-Default|Missing|Outside <head>",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "links",
        "Links",
        "main",
        URL
        + "|"
        + INDEX
        + "|crawl_depth=Crawl Depth|link_score=Link Score|inlinks=Inlinks|unique_inlinks=Unique Inlinks|unique_js_inlinks=Unique JS Inlinks|percent_total=% of Total|outlinks=Outlinks|unique_outlinks=Unique Outlinks|unique_js_outlinks=Unique JS Outlinks|external_outlinks=External Outlinks|unique_external_outlinks=Unique External Outlinks|unique_external_js_outlinks=Unique External JS Outlinks",
        "All|Pages With High Crawl Depth|Pages Without Internal Outlinks|Internal Nofollow Outlinks|Internal Outlinks With No Anchor Text|Non-Descriptive Anchor Text In Internal Outlinks|Pages With High External Outlinks|Pages With High Internal Outlinks|Follow & Nofollow Internal Inlinks To Page|Internal Nofollow Inlinks Only|Outlinks To Localhost|Non-Indexable Page Inlinks Only",
        LINKS,
        "select_url",
    ),
    _tab(
        "amp",
        "AMP",
        "main",
        HTTP
        + "|"
        + INDEX
        + "|title=Title 1|title_length=Title 1 Length|title_pixels=Title 1 Pixel Width|h1=H1-1|h1_length=H1-1 Length|canonical=Canonical Link Element 1|size=Size|word_count=Word Count|text_ratio=Text Ratio|crawl_depth=Crawl Depth|link_score=Link Score|response_time=Response Time",
        "All|Non-200 Response|Missing Non-AMP Return Link|Missing Canonical to Non-AMP|Non-Indexable Canonical|Indexable|Non-Indexable|Missing <html amp> Tag|Missing/Invalid <!doctype html> Tag|Missing <head> Tag|Missing <body> Tag|Missing Canonical|Missing/Invalid <meta charset> Tag|Missing/Invalid <meta viewport> Tag|Missing/Invalid AMP Script|Missing/Invalid AMP Boilerplate|Contains Disallowed HTML|Other Validation Errors",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "structured_data",
        "Structured Data",
        "main",
        URL
        + "|errors=Errors|warnings=Warnings|total_types=Total Types|unique_types=Unique Types|types=Types|"
        + INDEX,
        "All|Contains Structured Data|Missing|Parse Errors|Validation Errors|Validation Warnings|Microdata URLs|JSON-LD URLs|RDFa URLs",
        "Retained schema extraction/validation evidence; do not infer rich-result eligibility.",
        "select_url",
    ),
    _tab(
        "sitemaps",
        "Sitemaps",
        "main",
        HTTP + "|" + INDEX,
        "All|URLs in Sitemap|URLs not in Sitemap|Orphan URLs|Non-Indexable URLs in Sitemap|URLs in Multiple Sitemaps|XML Sitemap with over 50k URLs|XML Sitemap over 50MB",
        "Retained sitemap reconciliation; scope and completeness are required.",
        "select_url",
    ),
    _tab(
        "validation",
        "Validation",
        "main",
        HTTP + "|" + INDEX,
        "All|Invalid HTML Elements in Head|<body> Element Preceding <html>|<head> Not First In <html> Element|Missing <head> Tag|Multiple <head> Tags|Missing <body> Tag|Multiple <body> Tags|HTML Document Over 15MB",
        FINDINGS,
        "select_url",
    ),
)

DETAIL_TABS = (
    _tab(
        "url_details", "URL Details", "detail", "name=Name|value=Value", evidence=NATIVE
    ),
    _tab("inlinks", "Inlinks", "detail", LINK_COLUMNS, "All|Internal|External", LINKS),
    _tab(
        "outlinks", "Outlinks", "detail", LINK_COLUMNS, "All|Internal|External", LINKS
    ),
    _tab(
        "image_details",
        "Image Details",
        "detail",
        "from_url=From|to_url=To|alt=Alt Text|real_dimensions=Real Dimensions (WxH)|attribute_dimensions=Dimensions in Attributes (WxH)|display_dimensions=Display Dimensions (WxH)|potential_savings=Potential Savings|path_type=Path Type|link_path=Link Path|position=Link Position|origin=Link Origin",
        evidence=LINKS,
    ),
    _tab(
        "resources", "Resources", "detail", LINK_COLUMNS, "All|Internal|External", LINKS
    ),
    _tab(
        "serp_snippet",
        "SERP Snippet",
        "detail",
        URL + "|title=Title|description=Description",
        evidence="Retained title/description preview; actual search appearance is not measured.",
    ),
    _tab(
        "view_source",
        "View Source",
        "detail",
        "source=HTML source",
        evidence="Bounded retained source text only; never executes HTML/JavaScript.",
    ),
    _tab(
        "http_headers",
        "HTTP Headers",
        "detail",
        "direction=Direction|name=Header Name|value=Header Value",
        evidence="Retained request/response header allowlist; credentials and cookies stay redacted.",
    ),
    _tab(
        "cookies",
        "Cookies",
        "detail",
        "name=Cookie Name|value=Cookie Value|domain=Domain|path=Path|expires=Expiration Time|secure=Secure|http_only=HttpOnly|same_site=SameSite|url=Address",
        evidence="Retained redacted cookie metadata only; never echo session values.",
    ),
    _tab(
        "duplicate_details",
        "Duplicate Details",
        "detail",
        URL
        + "|near_duplicate_url=Near Duplicate Address|similarity=Similarity|"
        + INDEX,
        evidence=FINDINGS,
    ),
    _tab(
        "structured_data_details",
        "Structured Data Details",
        "detail",
        "property=Property|value=Value|validation_type=Validation Type|severity=Issue Severity|details=Details",
        evidence="Retained schema validation; nested values stay inspectable plain text.",
    ),
    _tab(
        "spelling_grammar_details",
        "Spelling & Grammar Details",
        "detail",
        "text=Text|error_type=Error Type|detail=Detail|suggestions=Suggestions|section=Page Section|url=Address",
        evidence="Retained language-check evidence; unavailable without a compatible source.",
    ),
)

RIGHT_TABS = (
    _tab(
        "overview",
        "Overview",
        "right",
        "name=Category / filter|urls=URLs|percent=% of Total",
        evidence="Core aggregates with measured coverage, never counts from a bounded page.",
    ),
    _tab(
        "issues",
        "Issues",
        "right",
        "title=Issue Name|kind=Issue Type|priority=Issue Priority|urls=URLs|percent=% of Total",
        "All|Issues|Warnings|Opportunities",
        FINDINGS,
        "select_issue",
    ),
    _tab(
        "site_structure",
        "Site Structure",
        "right",
        "path=Path|urls=URLs|indexable=Indexable|non_indexable=Non-Indexable",
        evidence="Bounded core structure projection; no synthetic tree totals.",
    ),
    _tab(
        "response_times",
        "Response Times",
        "right",
        "bucket=Response Time (In Seconds)|urls=URLs|percent=% of Total",
        evidence="Core aggregate response-time histogram; unavailable for absent timings.",
    ),
    _tab(
        "api",
        "API",
        "right",
        "provider=Provider|state=State|reason=Reason|observed_at=Observed at",
        evidence="Provider readiness projection; opening this tab does not connect or spend.",
    ),
    _tab(
        "spelling_grammar",
        "Spelling & Grammar",
        "right",
        "text=Text (Top 100)|error_type=Error Type|count=Error Count|urls=URLs Affected|coverage=Coverage|detail=Error Detail|language=Language|url=Sample URL",
        evidence="Bounded retained top-error projection; not a live language-check run.",
    ),
)

PROJECT_TABS = (
    _tab(
        "work",
        "Работа",
        "project",
        "id=ID|title=Задача|kind=Тип|state=Состояние|complete=Завершено|stale=Устарело|attempt_status=Попытка|applicability=Применимость|blocked_by=Зависимости|reason=Причина",
        "All|Complete|Incomplete|Blocked|Stale",
        PROJECT + " project-progress.",
        "select_task",
    ),
    _tab(
        "scans",
        "Сохранённые сканы",
        "project",
        "uuid=ID|start_url=Начальный URL|source_kind=Источник|lifecycle=Состояние|finished_at=Завершён|partial=Неполный",
        "All|Native|Screaming Frog|Partial",
        PROJECT + " project-scans; paths only from trusted response.",
        "select_scan",
    ),
    _tab(
        "reanalyze",
        "Повторный анализ",
        "project",
        "uuid=ID скана|source_kind=Источник|lifecycle=Состояние|finished_at=Завершён|partial=Неполный",
        evidence="Preview intent for selected retained scan; core validates evidence and bounds.",
        intent="select_scan",
    ),
    _tab(
        "compare",
        "Сравнение",
        "project",
        "url=URL|check=Проверка|state=Изменение|before=Раньше|after=Сейчас|reason=Причина",
        "All|New|Resolved|Persistent|Unknown",
        "Core comparison requires compatible scope/config/provenance; absence is not resolution.",
    ),
    _tab(
        "tasks",
        "Задачи проекта",
        "project",
        "id=ID|title=Задача|kind=Тип|state=Состояние|attempt_status=Попытка|applicability=Применимость|blocked_by=Зависимости|reason=Причина",
        "All|Complete|Incomplete|Blocked|Stale",
        PROJECT + " project-checklist-page and project-task-detail.",
        "select_task",
    ),
    _tab(
        "scenarios",
        "Сценарии",
        "project",
        "id=ID|title=Название|scope=Область|state=Готовность|requires=Требуется|reason=Причина",
        evidence="Existing core scenario registry; selection only, no automatic execution.",
        intent="select_scenario",
    ),
    _tab(
        "skills",
        "Навыки",
        "project",
        "id=ID|title=Название|scope=Область|version=Версия|source=Источник|reason=Ограничения",
        evidence="Existing packaged skill metadata; no inferred availability.",
        intent="select_skill",
    ),
    _tab(
        "competitors",
        "Конкуренты",
        "project",
        "domain=Домен|source=Источник|observed_at=Дата|scope=Область|state=Состояние|reason=Ограничения",
        evidence="Retained authorized competitor records; no discovery or paid calls.",
    ),
    _tab(
        "inbox",
        "Входящие",
        "project",
        "id=ID|kind=Тип|state=Состояние|text=Сообщение|created_at=Создано",
        "All|Unread|Read",
        PROJECT + " project-inbox-list/unread; form emits explicit submit_note only.",
        "select_note",
    ),
    _tab(
        "remediation",
        "Исправления",
        "project",
        "id=ID|title=Проблема|state=Состояние|owner=Ответственный|recheck_state=Перепроверка|evidence=Доказательство",
        "All|Open|Fixed|Regressed|Unknown",
        "Core finding/work-item lifecycle; fixed requires recheck evidence.",
        "select_task",
    ),
    _tab(
        "schema",
        "Schema",
        "project",
        "url=URL|types=Типы|state=Валидация|errors=Ошибки|warnings=Предупреждения|evidence=Источник",
        evidence="Retained schema output; no CMS write or automatic markup publication.",
    ),
    _tab(
        "providers",
        "Источники данных",
        "project",
        "provider=Источник|state=Готовность|reason=Причина|observed_at=Проверено|scope=Доступ",
        evidence="Existing source readiness projection; secrets are never displayed.",
    ),
    _tab(
        "logs",
        "Журнал",
        "project",
        "timestamp=Время|level=Уровень|operation=Операция|state=Состояние|message=Сообщение",
        "All|Info|Warning|Error",
        "Bounded redacted core event page; no full log materialization.",
    ),
    _tab(
        "config",
        "Конфигурация",
        "project",
        "name=Параметр|value=Значение|origin=Источник|state=Состояние|reason=Ограничения",
        evidence="Effective validated config projection; no secret values or implicit writes.",
    ),
    _tab(
        "reports",
        "Отчёты",
        "project",
        "name=Артефакт|format=Формат|state=Состояние|created_at=Создан|coverage=Покрытие|omissions=Ограничения",
        evidence="Existing validated artifacts, bounded overview plus full machine-readable evidence.",
        intent="select_report",
    ),
)

ALL_TABS = MAIN_TABS + DETAIL_TABS + RIGHT_TABS + PROJECT_TABS
TAB_BY_ID = {tab.id: tab for tab in ALL_TABS}
