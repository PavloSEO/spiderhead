"""SpiderHead's desktop panel catalogue: the main table, URL detail and summary panes.

Main tabs follow the audit areas of the core check registry (``CHECKS``): responses and
indexing, links, titles, headings, canonicals, content, images, markup, mobile, rendering, URL
hygiene, performance, security and sitemaps. Filter labels name the condition a core check
reports; length and size limits are the project's configured audit thresholds, so labels carry
no fixed numbers. The catalogue describes presentation, not implemented analyzer capabilities:
an adapter must supply rows, coverage and supported filters for each panel.
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


URL = "url=URL"
HTTP = URL + "|type=Content type|status=HTTP status|status_text=Status text"
INDEX = "indexability=Indexable|indexability_status=Indexability reason"
CANONICAL = "canonical=Canonical URL|http_canonical=Canonical in HTTP header"
ROBOTS = "meta_robots=Meta robots|x_robots_tag=X-Robots-Tag"
PAGINATION = "next_url=Next page|prev_url=Previous page"
MIME_FILTERS = (
    "All|HTML pages|Scripts|Stylesheets|Images|PDF documents|Fonts|Other types|Unknown type"
)
LINK_COLUMNS = (
    "type=Link type|from_url=Source|to_url=Destination|anchor_text=Anchor|alt=Image alt|"
    "follow=Followed|rel=rel|target=Target attribute|status=HTTP status|status_text=Status text|"
    "position=Placement|origin=Found in|path_type=Path form|link_path=DOM path"
)
NATIVE = "Bounded retained scan-inspect pages; additional fields require retained evidence."
FINDINGS = (
    "Retained findings and matched URL evidence; never calculate global findings from a page."
)
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
        + "|title=Title|issues=Findings|crawl_depth=Crawl depth|inlinks=Inlinks|outlinks=Outlinks",
        MIME_FILTERS,
        NATIVE,
        "select_url",
    ),
    _tab(
        "external",
        "External",
        "main",
        HTTP + "|crawl_depth=Crawl depth|inlinks=Inlinks",
        MIME_FILTERS,
        LINKS,
        "select_url",
    ),
    _tab(
        "response_codes",
        "Responses & indexing",
        "main",
        HTTP + "|" + INDEX + "|inlinks=Inlinks|response_time=Response time, s",
        "All|Indexable|Not indexable|Blocked by robots.txt|Linked but blocked by robots.txt|"
        "No response|2xx success|3xx redirect|4xx client error|5xx server error|"
        "Linked page is not indexable|Internal search route",
        NATIVE,
        "select_url",
    ),
    _tab(
        "redirects",
        "Redirects",
        "main",
        URL + "|status=HTTP status|redirect_type=Redirect kind|redirect_url=Redirect target|"
        "final_url=Final URL|hops=Hops|inlinks=Inlinks",
        "All|Permanent redirect|Temporary redirect where permanent is expected|Meta refresh|"
        "Refresh header|JavaScript redirect|Two or more hops|Redirect loop|Internal link to a redirect|"
        "External link to a redirect",
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
        + "|crawl_depth=Crawl depth|link_score=Link score|inlinks=Inlinks|unique_inlinks=Unique inlinks|"
        "outlinks=Outlinks|unique_outlinks=Unique outlinks|external_outlinks=External outlinks",
        "All|Internal link to a 4xx page|Internal link to a 5xx page|Broken external link|"
        "Empty anchor text|Generic anchor text|Image link without text|Nofollow internal link|"
        "Only nofollow inlinks|Only inlinks from non-indexable pages|Linked only from page chrome|"
        "No internal outlinks|Too many outlinks|Too many external outlinks|Too deep from the start URL|"
        "Low link score|Orphan page|Link to localhost",
        LINKS,
        "select_url",
    ),
    _tab(
        "page_titles",
        "Titles & descriptions",
        "main",
        URL + "|title=Title|title_length=Title length|title_pixels=Title width, px|"
        "description=Meta description|description_length=Description length|"
        "description_pixels=Description width, px|" + INDEX,
        "All|Title missing|Title duplicated|Title too long|Title shorter than recommended|"
        "Same text as the H1|Several title elements|Templated title|Title set by JavaScript|"
        "Description missing|Description duplicated|Description too long|"
        "Description shorter than recommended|Several meta descriptions|Description set by JavaScript|"
        "Obsolete meta keywords present",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "headings",
        "Headings",
        "main",
        URL + "|h1=H1|h1_length=H1 length|h1_count=H1 count|h2=First H2|h2_count=H2 count|" + INDEX,
        "All|H1 missing|H1 duplicated|H1 too long|Several H1 headings|H1 is only image alt text|"
        "H2 missing|H2 duplicated|H2 too long|Heading before the H1|Heading level skipped|"
        "Heading in page chrome|Link inside a heading",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "canonicals",
        "Canonicals & directives",
        "main",
        URL + "|" + INDEX + "|" + CANONICAL + "|" + ROBOTS + "|meta_refresh=Meta refresh",
        "All|Canonical to itself|Canonical to another URL|Canonical missing|Several canonicals|"
        "Canonical to a non-indexable URL|Canonical to a redirect|Canonical to an error page|"
        "Relative canonical URL|Canonical chain|Canonical to another origin|Canonical not linked|"
        "Canonical differs after rendering|Noindex|Nofollow|Snippet or archive controls|"
        "Obsolete robots directive",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "pagination",
        "Pagination",
        "main",
        URL + "|" + INDEX + "|" + PAGINATION + "|" + CANONICAL,
        "All|Part of a paginated series|Series not linked|Pagination URL without an anchor|"
        "Non-indexable page in a series|Several next or previous links|Broken sequence|Pagination loop|"
        "Canonical against the pagination policy",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "content",
        "Content",
        "main",
        URL + "|word_count=Word count|text_ratio=Text ratio, %|reading_ease=Readability score|"
        "words_per_sentence=Words per sentence|closest_match=Most similar page|similarity=Similarity|"
        "language=Language|spelling_errors=Spelling errors|grammar_errors=Grammar errors|" + INDEX,
        "All|Thin content|Low text-to-HTML ratio|Exact duplicate|Near duplicate|Hard to read|"
        "Long sentences|Spelling errors|Grammar errors|Placeholder text|Content inside an iframe|"
        "Possible soft 404",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "images",
        "Images",
        "main",
        URL
        + "|type=Content type|size=Size|dimensions=Width × height|image_inlinks=Used on pages|"
        + INDEX,
        "All|Alt text missing|No alt attribute|Alt text too long|Larger than the size limit|"
        "Broken image|No width or height attributes|Served larger than displayed|Upscaled in the layout",
        LINKS,
        "select_url",
    ),
    _tab(
        "structured_data",
        "Structured data",
        "main",
        URL + "|types=Types|formats=Formats|errors=Errors|warnings=Warnings|" + INDEX,
        "All|Has structured data|No structured data|JSON-LD does not parse|Schema errors|"
        "Schema warnings|Rich result property missing|Open Graph missing",
        "Retained schema extraction/validation evidence; do not infer rich-result eligibility.",
        "select_url",
    ),
    _tab(
        "hreflang",
        "Hreflang",
        "main",
        URL
        + "|html_hreflang=In HTML|http_hreflang=In HTTP header|sitemap_hreflang=In sitemap|"
        + INDEX,
        "All|Has hreflang|Target returns an error|Target not linked|Target is noindex|"
        "Target is not canonical|No return link|Return link disagrees|Invalid language or region code|"
        "Same code listed twice|No self reference|No x-default",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "mobile",
        "Mobile",
        "main",
        URL + "|viewport=Viewport|" + INDEX,
        "All|Viewport missing|Content wider than the screen|Text too small|Tap targets too small|"
        "Separate mobile URL",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "rendering",
        "JavaScript rendering",
        "main",
        URL + "|title=Title|h1=H1|canonical=Canonical URL|" + INDEX,
        "All|Title only after rendering|Description only after rendering|H1 only after rendering|"
        "Canonical only after rendering|Noindex only in raw HTML|Blocked render resources|"
        "Console errors",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "url",
        "URL",
        "main",
        HTTP + "|" + INDEX + "|length=Length|canonical=Canonical URL",
        "All|Longer than the limit|Query parameters|Tracking parameters|Session ID|Non-ASCII characters|"
        "Uppercase letters|Underscores|Spaces|Repeated slashes|Repeated path segment|"
        "Inconsistent trailing slash|Broken fragment link",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "performance",
        "Performance",
        "main",
        URL + "|response_time=Response time, s|html_size=HTML size|page_weight=Page weight|"
        "dom_nodes=DOM nodes|dom_depth=DOM depth|http_version=HTTP version",
        "All|Slow response|Large HTML|Heavy page|Too many DOM nodes|DOM too deep|No compression|"
        "Render-blocking resources|Short cache lifetime|HTTP/1.1 only",
        FINDINGS,
        "select_url",
    ),
    _tab(
        "security",
        "Security",
        "main",
        HTTP + "|http_version=HTTP version|" + INDEX,
        "All|Served over HTTP|Mixed content|HTTP link on an HTTPS page|Form submits over HTTP|"
        "Form on an HTTP page|Cross-origin link without noopener|Protocol-relative link|No HSTS|"
        "No Content-Security-Policy|No X-Content-Type-Options|No X-Frame-Options|No Referrer-Policy|"
        "Security header sent twice|Declared content type mismatch",
        "Retained security findings and response headers; response headers are dynamic columns.",
        "select_url",
    ),
    _tab(
        "sitemaps",
        "Sitemaps",
        "main",
        HTTP + "|" + INDEX,
        "All|Listed in a sitemap|Indexable but not listed|Listed but not linked|Listed URL redirects|"
        "Listed URL returns an error|Listed URL is not indexable|Listed URL is not canonical|"
        "Listed URL blocked by robots.txt|Listed in several sitemaps|Stale lastmod|"
        "Sitemap over the URL limit|Sitemap over the size limit",
        "Retained sitemap reconciliation; scope and completeness are required.",
        "select_url",
    ),
    _tab(
        "validation",
        "HTML structure",
        "main",
        HTTP + "|" + INDEX,
        "All|Head missing|Several head elements|Body missing|Several body elements|Head not first|"
        "Invalid element in head|Tags outside the head|Doctype missing|Charset missing|Duplicate IDs|"
        "AMP version problem|AMP markup invalid",
        FINDINGS,
        "select_url",
    ),
)

DETAIL_TABS = (
    _tab("url_details", "Facts", "detail", "name=Name|value=Value", evidence=NATIVE),
    _tab("inlinks", "Inlinks", "detail", LINK_COLUMNS, "All|Internal|External", LINKS),
    _tab(
        "outlinks",
        "Outlinks",
        "detail",
        LINK_COLUMNS,
        "All|Internal|External|Hyperlinks|Resources",
        LINKS,
    ),
    _tab(
        "http_headers",
        "HTTP headers",
        "detail",
        "direction=Direction|name=Header|value=Value",
        evidence="Retained request/response header allowlist; credentials and cookies stay redacted.",
    ),
    _tab(
        "cookies",
        "Cookies",
        "detail",
        "name=Name|value=Value|domain=Domain|path=Path|expires=Expires|secure=Secure|http_only=HttpOnly|same_site=SameSite|url=URL",
        evidence="Retained redacted cookie metadata only; never echo session values.",
    ),
    _tab(
        "view_source",
        "Source HTML",
        "detail",
        "source=HTML source",
        evidence="Bounded retained source text only; never executes HTML/JavaScript.",
    ),
    _tab(
        "serp_snippet",
        "Search preview",
        "detail",
        URL + "|title=Title|description=Description",
        evidence="Retained title/description preview; actual search appearance is not measured.",
    ),
    _tab(
        "image_details",
        "Page images",
        "detail",
        "to_url=Image|alt=Alt text|real_dimensions=Intrinsic size|attribute_dimensions=Size in attributes|display_dimensions=Displayed size|potential_savings=Possible saving|position=Placement|origin=Found in",
        evidence=LINKS,
    ),
    _tab(
        "structured_data_details",
        "Structured data",
        "detail",
        "property=Property|value=Value|validation_type=Check|severity=Severity|details=Details",
        evidence="Retained schema validation; nested values stay inspectable plain text.",
    ),
    _tab(
        "duplicate_details",
        "Similar pages",
        "detail",
        URL + "|near_duplicate_url=Similar page|similarity=Similarity|" + INDEX,
        evidence=FINDINGS,
    ),
    _tab(
        "spelling_grammar_details",
        "Language check",
        "detail",
        "text=Text|error_type=Kind|detail=Explanation|suggestions=Suggestions|section=Page area|url=URL",
        evidence="Retained language-check evidence; unavailable without a compatible source.",
    ),
)

RIGHT_TABS = (
    _tab(
        "overview",
        "Summary",
        "right",
        "name=Category|urls=URLs|percent=Share of URLs",
        evidence="Core aggregates with measured coverage, never counts from a bounded page.",
    ),
    _tab(
        "issues",
        "Findings",
        "right",
        "title=Check|kind=Severity|urls=URLs|percent=Share of URLs",
        "All|Critical|Warning|Notice",
        FINDINGS,
        "select_issue",
    ),
    _tab(
        "site_structure",
        "Folders",
        "right",
        "path=Folder|urls=URLs|indexable=Indexable|non_indexable=Not indexable",
        evidence="Bounded core structure projection; no synthetic tree totals.",
    ),
    _tab(
        "response_times",
        "Response time",
        "right",
        "bucket=Seconds|urls=URLs|percent=Share of URLs",
        evidence="Core aggregate response-time histogram; unavailable for absent timings.",
    ),
    _tab(
        "spelling_grammar",
        "Language",
        "right",
        "text=Text|error_type=Kind|count=Occurrences|urls=URLs|coverage=Coverage|detail=Explanation|language=Language|url=Example URL",
        evidence="Bounded retained top-error projection; not a live language-check run.",
    ),
    _tab(
        "api",
        "Data sources",
        "right",
        "provider=Provider|state=State|reason=Reason|observed_at=Observed at",
        evidence="Provider readiness projection; opening this tab does not connect or spend.",
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
