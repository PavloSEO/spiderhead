"""Simple mode catalogue: the eight crawler tabs, their filters, columns, default sort and Overview nodes.

Every filter is one AND list of ``scan-url-query`` filters. A filter or tab without a core source carries the reason
instead (``reason``); the screen shows it as «Not collected» and never computes its rows or counts from the page.
Ids are stable: they key saved column layouts (``simple/columns/<tab>/<filter>``).
"""

from __future__ import annotations

from dataclasses import dataclass, field


def flt(column, op, value=None):
    return {"column": column, "op": op, **({} if value is None else {"value": value})}


HTML_2XX = (flt("status_class", "eq", "2xx"), flt("content_type", "contains", "html"))
LOW_WORDS = 200  # the thin-content threshold the analyzer uses; shown in the filter label


@dataclass(frozen=True)
class Filter:
    id: str
    label: str
    filters: tuple = ()
    columns: tuple = ()
    sort: tuple | None = None  # (key, "asc" | "desc"); None = crawl order
    facet: str | None = None  # facet of ``--facets all`` that equals this filter, when one exists
    html_facet: str | None = None  # facet that equals this filter when counted over HTML_2XX
    reason: str = ""  # non-empty: not collected by this version
    needs_sitemap: bool = False


@dataclass(frozen=True)
class Tab:
    id: str
    title: str
    filters: tuple = field(default_factory=tuple)
    reason: str = ""  # non-empty: the whole tab is not collected


ALL_COLUMNS = (
    "url", "content_type", "status_code", "status_text", "indexable", "title", "title_length",
    "meta_description", "h1", "word_count", "crawl_depth", "inlinks", "response_time",
)  # fmt: skip
TITLE_COLUMNS = ("url", "status_code", "indexable", "title", "title_length", "h1", "inlinks", "crawl_depth")
DESCRIPTION_COLUMNS = ("url", "indexable", "meta_description", "meta_description_length", "title", "inlinks")
H1_COLUMNS = ("url", "indexable", "h1", "h1_2", "inlinks")
CODE_COLUMNS = ("url", "content_type", "status_code", "status_text", "indexable", "inlinks", "response_time")
ERROR_COLUMNS = ("url", "status_code", "status_text", "inlinks", "crawl_depth")
LINKED_FIRST = ("inlinks", "desc")

TABS = (
    Tab(
        "internal",
        "Internal",
        (
            Filter("all", "All", (), ALL_COLUMNS, facet="all"),
            Filter("html", "HTML", (flt("content_type", "contains", "html"),), ALL_COLUMNS),
            Filter(
                "title_missing",
                "Missing title",
                (*HTML_2XX, flt("title", "empty")),
                TITLE_COLUMNS,
                LINKED_FIRST,
                html_facet="title_missing",
            ),
            Filter(
                "title_duplicate",
                "Duplicate title",
                (*HTML_2XX, flt("title_duplicate", "eq", True)),
                TITLE_COLUMNS,
                ("title", "asc"),
                html_facet="title_duplicate",
            ),
            Filter(
                "title_long",
                "Title over 60 characters",
                (*HTML_2XX, flt("title_length", "gt", 60)),
                TITLE_COLUMNS,
                LINKED_FIRST,
                html_facet="title_long",
            ),
            Filter(
                "description_missing",
                "Missing description",
                (*HTML_2XX, flt("meta_description", "empty")),
                DESCRIPTION_COLUMNS,
                LINKED_FIRST,
            ),
            Filter(
                "description_long",
                "Description over 160 characters",
                (*HTML_2XX, flt("meta_description_length", "gt", 160)),
                DESCRIPTION_COLUMNS,
                LINKED_FIRST,
            ),
            Filter(
                "description_duplicate",
                "Duplicate description",
                reason="Duplicate descriptions are not detected by this version of the core.",
            ),
            Filter("h1_missing", "Missing H1", (*HTML_2XX, flt("h1", "empty")), H1_COLUMNS, LINKED_FIRST),
            Filter("h1_multiple", "Multiple H1", (*HTML_2XX, flt("h1_2", "not_empty")), H1_COLUMNS, LINKED_FIRST),
            Filter(
                "non_indexable",
                "Non-indexable",
                (flt("indexable", "eq", False),),
                ("url", "status_code", "indexable", "meta_robots", "x_robots", "canonical", "inlinks"),
                LINKED_FIRST,
            ),
            Filter(
                "low_words",
                f"Low word count (under {LOW_WORDS})",
                (*HTML_2XX, flt("word_count", "lt", LOW_WORDS)),
                ("url", "indexable", "word_count", "title", "inlinks"),
                LINKED_FIRST,
            ),
        ),
    ),
    Tab(
        "external",
        "External",
        reason=(
            "External link targets are not stored as a site-wide list yet. The links of each page are in the "
            "details pane: select a URL and open Outlinks."
        ),
    ),
    Tab(
        "response_codes",
        "Response Codes",
        (
            Filter("all", "All", (), CODE_COLUMNS, ("status_code", "desc"), facet="all"),
            Filter("2xx", "Success (2xx)", (flt("status_class", "eq", "2xx"),), CODE_COLUMNS, facet="status_2xx"),
            Filter(
                "3xx",
                "Redirection (3xx)",
                (flt("status_class", "eq", "3xx"),),
                ("url", "status_code", "redirect_url", "inlinks"),
                LINKED_FIRST,
                facet="status_3xx",
            ),
            Filter(
                "4xx",
                "Client Error (4xx)",
                (flt("status_class", "eq", "4xx"),),
                ERROR_COLUMNS,
                LINKED_FIRST,
                facet="status_4xx",
            ),
            Filter(
                "5xx",
                "Server Error (5xx)",
                (flt("status_class", "eq", "5xx"),),
                ERROR_COLUMNS,
                LINKED_FIRST,
                facet="status_5xx",
            ),
            Filter("none", "No response", (flt("status_class", "eq", "none"),), ERROR_COLUMNS, LINKED_FIRST),
        ),
    ),
    Tab(
        "redirects",
        "Redirects",
        (
            Filter(
                "all",
                "All",
                (flt("status_class", "eq", "3xx"),),
                ("url", "status_code", "redirect_url", "final_url", "inlinks"),
                LINKED_FIRST,
                facet="status_3xx",
            ),
            Filter(
                "permanent",
                "Permanent",
                (flt("status_code", "in", [301, 308]),),
                ("url", "status_code", "redirect_url", "final_url", "inlinks"),
                LINKED_FIRST,
            ),
            Filter(
                "temporary",
                "Temporary",
                (flt("status_code", "in", [302, 303, 307]),),
                ("url", "status_code", "redirect_url", "final_url", "inlinks"),
                LINKED_FIRST,
            ),
            Filter(
                "other",
                "Other 3xx",
                (flt("status_class", "eq", "3xx"), flt("status_code", "not_in", [301, 302, 303, 307, 308])),
                ("url", "status_code", "redirect_url", "final_url", "inlinks"),
                LINKED_FIRST,
            ),
            Filter("chain", "Redirect chain", reason="Redirect chains are only readable per URL (URL Details)."),
            Filter("loop", "Redirect loop", reason="Redirect loops are only readable per URL (URL Details)."),
            Filter("to_error", "Redirect to error", reason="The status of the final URL is not queryable yet."),
        ),
    ),
    Tab(
        "canonicals",
        "Canonicals",
        (
            Filter(
                "all",
                "All",
                HTML_2XX,
                ("url", "indexable", "canonical", "final_canonical", "meta_robots", "inlinks"),
                ("url", "asc"),
                html_facet="all",
            ),
            Filter(
                "self",
                "Self-referencing",
                (flt("canonical_self", "eq", True),),
                ("url", "indexable", "canonical", "inlinks"),
                ("url", "asc"),
                facet="canonical_self",
            ),
            Filter(
                "canonicalised",
                "Canonicalised",
                (flt("canonical_other", "eq", True),),
                ("url", "indexable", "canonical", "final_canonical", "inlinks"),
                LINKED_FIRST,
                facet="canonical_other",
            ),
            Filter(
                "missing",
                "Missing",
                (*HTML_2XX, flt("canonical", "empty")),
                ("url", "indexable", "canonical", "inlinks"),
                LINKED_FIRST,
                html_facet="canonical_missing",
            ),
            Filter(
                "non_indexable",
                "Non-indexable canonical",
                reason="The indexability of a canonical target is not joined by this version of the core.",
            ),
        ),
    ),
    Tab(
        "blocked",
        "Blocked by Robots",
        reason="URLs blocked by robots.txt are recorded only as crawl decisions; no list is available yet.",
    ),
    Tab(
        "sitemap",
        "Sitemap",
        (
            Filter(
                "in",
                "In sitemap",
                (flt("in_sitemap", "eq", True),),
                ("url", "status_code", "indexable", "inlinks"),
                ("url", "asc"),
                facet="in_sitemap",
                needs_sitemap=True,
            ),
            Filter(
                "not_in",
                "Not in sitemap",
                (flt("in_sitemap", "eq", False),),
                ("url", "status_code", "indexable", "inlinks"),
                LINKED_FIRST,
                needs_sitemap=True,
            ),
            Filter(
                "non_indexable",
                "Non-indexable in sitemap",
                (flt("in_sitemap", "eq", True), flt("indexable", "eq", False)),
                ("url", "status_code", "indexable", "inlinks"),
                ("url", "asc"),
                needs_sitemap=True,
            ),
            Filter(
                "orphan",
                "Orphan (in sitemap, no inlinks)",
                (flt("in_sitemap", "eq", True), flt("inlinks", "eq", 0)),
                ("url", "status_code", "indexable", "inlinks"),
                ("url", "asc"),
                needs_sitemap=True,
            ),
        ),
    ),
    Tab(
        "images",
        "Images",
        (
            Filter(
                "missing_alt_pages",
                "Pages with images missing alt",
                (flt("images_missing_alt_attr", "gt", 0),),
                ("url", "images_total", "images_missing_alt_attr", "inlinks"),
                ("images_missing_alt_attr", "desc"),
            ),
            Filter("per_image", "Per-image checks", reason="Images are not stored one row per image yet."),
        ),
    ),
)

TAB_INDEX = {tab.id: index for index, tab in enumerate(TABS)}


def find(tab_id, filter_id):
    """(tab index, filter index) of a pair, or None."""
    t = TAB_INDEX.get(tab_id)
    if t is None:
        return None
    for e, item in enumerate(TABS[t].filters):
        if item.id == filter_id:
            return t, e
    return None


# Overview groups: (title, ((label, tab id, filter id, badge kind), ...)); a None filter id = not collected
OVERVIEW = (
    ("Summary", (("Crawled URLs", "internal", "all", "mut"), ("External", "external", None, "mut"))),
    (
        "Response Codes",
        (
            ("Success (2xx)", "response_codes", "2xx", "ok"),
            ("Redirection (3xx)", "response_codes", "3xx", "info"),
            ("Client Error (4xx)", "response_codes", "4xx", "err"),
            ("Server Error (5xx)", "response_codes", "5xx", "err"),
            ("No response", "response_codes", "none", "mut"),
        ),
    ),
    (
        "Page Titles",
        (
            ("Missing", "internal", "title_missing", "err"),
            ("Duplicate", "internal", "title_duplicate", "warn"),
            ("Over 60 characters", "internal", "title_long", "warn"),
        ),
    ),
    (
        "Meta Description",
        (
            ("Missing", "internal", "description_missing", "warn"),
            ("Over 160 characters", "internal", "description_long", "warn"),
            ("Duplicate", "internal", "description_duplicate", "mut"),
        ),
    ),
    ("H1", (("Missing", "internal", "h1_missing", "err"), ("Multiple", "internal", "h1_multiple", "warn"))),
    (
        "Canonicals",
        (
            ("Missing", "canonicals", "missing", "warn"),
            ("Canonicalised", "canonicals", "canonicalised", "mut"),
            ("Non-indexable canonical", "canonicals", "non_indexable", "mut"),
        ),
    ),
    ("Sitemap", (("In sitemap", "sitemap", "in", "mut"), ("Not in sitemap", "sitemap", "not_in", "warn"))),
    ("Blocked by Robots", (("Blocked URLs", "blocked", None, "mut"),)),
    ("Images", (("Pages missing alt", "images", "missing_alt_pages", "warn"),)),
)
ERROR_FILTERS = (("response_codes", "4xx"), ("response_codes", "5xx"), ("response_codes", "none"))
