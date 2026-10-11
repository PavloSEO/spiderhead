"""Rule engine for checks derived from ``Internal:All``.

Each function reads the normalized pages and emits issues via ``ctx.add``.
Checks apply only where they make sense (HTML/indexable) and degrade quietly
when a column the data would need is absent.
"""

from __future__ import annotations

import itertools
import json
import mimetypes
import re
import urllib.parse
from collections import Counter, OrderedDict, defaultdict
from collections.abc import Iterable
from dataclasses import dataclass
from typing import Any

from seohead.checks.parser import robots_directives, uses_ajax_crawling_scheme
from seohead.core.canonical_policy import matching_canonical_rule

from .asset_checks import check_lighthouse_reports, check_render_assets, check_resource_assets
from .context import AuditContext
from .models import Page
from .normalize import norm_url

NON_ASCII = re.compile(r"[^\x00-\x7F]")

# Tracking/ad/click-ID parameters that create session-specific URLs and pollute
# the index when they leak into indexable pages. Whole-param-name match.
TRACKING_PARAM_RE = re.compile(
    r"^(?:"
    r"utm_[a-z_]+"  # utm_source/medium/campaign/term/content/id/...
    r"|gcl(?:id|src)"  # Google Ads click & source
    r"|fbclid"  # Facebook
    r"|msclkid"  # Microsoft
    r"|yclid"  # Yandex Direct
    r"|dclid"  # DoubleClick
    r"|w?braid"  # gbraid/wbraid (Google Ads new-format)
    r"|_hs(?:enc|mi)"  # HubSpot
    r"|mc_[ce]id"  # Mailchimp campaign/eid
    r"|pk_(?:campaign|kwd|source|medium|content)"  # Piwik/Matomo
    r"|vero_(?:id|conv)"  # Vero
    r"|trksid?"  # eBay/LinkedIn tracking
    r"|cmpid"  # generic campaign id
    r"|mbid"  # marketo/bid
    r")$",
    re.IGNORECASE,
)

SESSION_PARAM_RE = re.compile(
    r"^(?:sid|session(?:_?id)?|phpsessid|jsessionid|asp\.net_sessionid|cfid|cftoken)$",
    re.IGNORECASE,
)

# Deliberately literal editorial/template placeholders only. Broad words such
# as "coming soon" and "sample" describe legitimate content too often to be
# audit facts; these bracketed tokens are an observed unfinished declaration.
PLACEHOLDER_MARKER_RE = re.compile(
    r"(?:\{\{\s*(?:todo|tbd|placeholder)\s*\}\}|\[\s*(?:todo|tbd|placeholder)\s*\])",
    re.IGNORECASE,
)


def _tracking_params(url: str) -> list[str]:
    """Param names on ``url`` that look like tracking IDs (empty == clean)."""
    qs = urllib.parse.urlsplit(url).query
    if not qs:
        return []
    return [k for k in urllib.parse.parse_qs(qs) if TRACKING_PARAM_RE.match(k)]


def _session_params(url: str) -> list[str]:
    """Known session parameter names only; values must never enter audit output."""
    return [
        name
        for name, _value in urllib.parse.parse_qsl(urllib.parse.urlsplit(url).query)
        if SESSION_PARAM_RE.match(name)
    ]


def _redact_query_values(url: str) -> str:
    parts = urllib.parse.urlsplit(url)
    query = urllib.parse.urlencode(
        [
            (name, "[redacted]")
            for name, _value in urllib.parse.parse_qsl(parts.query, keep_blank_values=True)
        ]
    )
    return urllib.parse.urlunsplit((parts.scheme, parts.netloc, parts.path, query, parts.fragment))


def _slash_key(url: str) -> tuple[str, str, str, str] | None:
    parts = urllib.parse.urlsplit(url)
    if not parts.path or parts.path == "/":
        return None
    return (parts.scheme.lower(), parts.netloc.lower(), parts.path.rstrip("/"), parts.query)


@dataclass(frozen=True)
class ProbedTarget:
    """A canonical target no crawled page answered, as its stored target probe reports it."""

    url: str
    status_code: int
    redirect_url: str
    indexability: str | None = None
    indexability_status: str | None = None


def _probed_targets(ctx: AuditContext, key: str) -> list[ProbedTarget]:
    """Stored probe of an uncaptured target (#987); empty when none answered with a status."""
    payload = getattr(ctx, "target_probes", {}).get(key)
    if not payload or type(payload.get("status_code")) is not int:
        return []
    return [
        ProbedTarget(
            url=payload["url"],
            status_code=payload["status_code"],
            redirect_url=payload.get("redirect_url") or "",
        )
    ]


def _rec(page: Page) -> dict[str, Any]:
    return page.metrics.get("_record", {})


def _body_unavailable(rec: dict[str, Any]) -> bool:
    """Whether this row's HTML body was not parsed or retained.

    A blank title/description/h1/canonical on such a row means "never measured",
    not "observed absent" -- the same distinction ``_has_column`` draws for a
    column missing from the whole export, but here it is one row at a time. The
    marker covers both the existing size limit and an explicit crawl media-type
    filter.
    """
    return bool(rec.get("body_unavailable"))


def _body_unavailable_reason(pages: Iterable[Page]) -> str | None:
    reasons: dict[str, int] = {}
    for page in pages:
        reason = str(_rec(page).get("body_unavailable") or "")
        if reason:
            reasons[reason] = reasons.get(reason, 0) + 1
    count = sum(reasons.values())
    if set(reasons) == {"oversized"}:
        return (
            f"{count} page(s) with an oversized, unparsed HTML body "
            "-- this metadata was never measured"
        )
    elif count:
        labels = {
            "excluded_by_media_type": "excluded by media type",
            "not_included_by_media_type": "not included by media type",
            "media_type_unavailable": "missing or malformed Content-Type",
            "oversized": "oversized HTML body",
        }
        summary = ", ".join(
            f"{labels.get(reason, reason)}: {amount}" for reason, amount in sorted(reasons.items())
        )
        return f"{count} page(s) were not parsed ({summary}); this metadata was never measured"
    return None


def _skip_for_body_unavailable(ctx: AuditContext, check_id: str, pages: list[Page]) -> None:
    """Name why unparsed HTML cannot contribute a verdict to ``check_id``."""
    reason = _body_unavailable_reason(pages)
    if reason:
        ctx.skip(check_id, reason)


# A blank parser-derived column is not a negative result when HTML was not
# parsed. Keep this inventory next to the shared unavailable-body policy so
# new on-page rules can be checked against it during review. Checks that also
# have evidence elsewhere retain their normal verdict: AuditContext.add
# retracts a check's skip when that check actually fires.
_BODY_DERIVED_INDEXABLE_CHECKS = (
    "NON_INDEXABLE_LINKED",
    "TITLE_MISSING",
    "TITLE_TOO_LONG",
    "TITLE_TOO_SHORT",
    "TITLE_EQUALS_H1",
    "TITLE_DUPLICATE",
    "DESC_MULTIPLE",
    "DESC_MISSING",
    "DESC_TOO_LONG",
    "DESC_TOO_SHORT",
    "DESC_DUPLICATE",
    "H1_MISSING",
    "H1_MULTIPLE",
    "H1_TOO_LONG",
    "H1_DUPLICATE",
    "H1_ALT_TEXT_ONLY",
    "H2_MISSING",
    "H2_TOO_LONG",
    "H2_DUPLICATE",
    "TITLE_OUTSIDE_HEAD",
    "DESC_OUTSIDE_HEAD",
    "CANONICAL_MISSING",
    "HEADING_BEFORE_H1",
    "HEADING_IN_PAGE_CHROME",
    "HEADING_SKIP",
    "LINK_INSIDE_HEADING",
    "IMAGE_LINK_WITHOUT_TEXT",
    "NO_INTERNAL_OUTLINKS",
    "HIGH_OUTLINKS",
    "HIGH_EXTERNAL_OUTLINKS",
    "URL_HAS_PARAMS",
    "CONTENT_IN_IFRAME",
    "THIN_CONTENT",
    "LOW_TEXT_RATIO",
    "HTML_BLOAT",
    "TITLE_TEMPLATED",
    "NEAR_DUPLICATE",
    "OG_MISSING",
)

_BODY_DERIVED_HTML_CHECKS = (
    "CANONICALISED",
    "CANONICAL_NON_INDEXABLE",
    "CANONICAL_RELATIVE",
    "CANONICAL_FRAGMENT",
    "CANONICAL_CHAIN",
    "CANONICAL_TO_REDIRECT",
    "UNLINKED_CANONICAL",
    "DIRECTIVES_OUTSIDE_HEAD",
    "NOINDEX",
    "NOFOLLOW_PAGE",
    "NOARCHIVE",
    "NOSNIPPET",
    "NOIMAGEINDEX",
    "NOTRANSLATE",
    "NOODP",
    "NOYDIR",
    "UNAVAILABLE_AFTER",
    "HREFLANG_OUTSIDE_HEAD",
    "META_REFRESH_REDIRECT",
    "CANONICAL_OUTSIDE_HEAD",
    "STRUCTURED_DATA_PARSE_ERROR",
    "LOREM_IPSUM_PLACEHOLDER",
    "UNSUPPORTED_PLUGIN",
    "IMG_MISSING_ALT_ATTRIBUTE",
    "IMG_ALT_TOO_LONG",
    "AJAX_CRAWLING_SCHEME_URL",
    "AJAX_CRAWLING_SCHEME_META_FRAGMENT",
    "HREFLANG_BROKEN_TARGET",
    "HREFLANG_UNLINKED_TARGET",
    "HREFLANG_INVALID_CODE",
    "HREFLANG_MULTIPLE_ENTRIES",
    "HREFLANG_MISSING_SELF_REFERENCE",
    "HREFLANG_MISSING_XDEFAULT",
    "HREFLANG_NOT_CANONICAL",
    "HREFLANG_MISSING_RETURN_LINK",
    "BROKEN_INTERNAL_LINK",
    "LINK_TO_5XX",
    "INTERNAL_LINK_TO_REDIRECT",
    "BROKEN_EXTERNAL_LINK",
    "EXTERNAL_LINK_TO_REDIRECT",
    "GENERIC_ANCHOR_TEXT",
    "LOW_LINK_SCORE",
    "ONLY_NOFOLLOW_INLINKS",
    "ONLY_NONINDEXABLE_SOURCE_INLINKS",
    "DEEP_DISCOVERY_PATH",
    "DEEP_CLICK_DEPTH",
    "DUPLICATE_INTERNAL_LINK",
    "INSECURE_SUBRESOURCE",
    "PAGINATION_MULTIPLE",
    "PAGINATION_URL_NOT_IN_ANCHOR",
    "PAGINATION_SEQUENCE_ERROR",
    "PAGINATION_LOOP",
    "UNLINKED_PAGINATION_SERIES",
    "PAGINATION_NONINDEXABLE",
    "MISSING_DOCTYPE",
    "VIEWPORT_MISSING",
    "RENDER_BLOCKING",
    "HEAD_MISSING",
    "HEAD_MULTIPLE",
    "BODY_MISSING",
    "BODY_MULTIPLE",
    "INVALID_HEAD_ELEMENT",
    "HEAD_NOT_FIRST",
)


def _declare_unparsed_body_skips(ctx: AuditContext) -> None:
    populations = (
        (_BODY_DERIVED_INDEXABLE_CHECKS, ctx.indexable_html_pages()),
        (_BODY_DERIVED_HTML_CHECKS, ctx.html_pages()),
    )
    for check_ids, pages in populations:
        reason = _body_unavailable_reason(pages)
        if reason:
            for check_id in check_ids:
                if not ctx.enabled(check_id):
                    continue
                if check_id == "H2_MISSING" and not ctx.requirements.get("require_h2", False):
                    continue
                if check_id == "CANONICAL_MISSING" and not ctx.requirements.get(
                    "require_canonical", True
                ):
                    continue
                ctx.skip(check_id, reason)
    charset_pages = (
        page
        for page in ctx.html_pages()
        if not (page.content_type and _CHARSET_IN_HEADER_RE.search(page.content_type))
    )
    charset_reason = _body_unavailable_reason(charset_pages)
    if charset_reason and ctx.enabled("MISSING_CHARSET"):
        ctx.skip("MISSING_CHARSET", charset_reason)


def _canonical_policy_target_is_fetched(ctx: AuditContext, url: str) -> bool:
    """Whether this exact policy URL has a response row in Internal:All.

    The general URL index folds trailing slashes for link matching. A declared
    policy target is stricter: a slashless redirect does not prove that its
    distinct slash target was fetched. Rows without a status are placeholders,
    not target-access evidence.
    """
    try:
        target = urllib.parse.urlsplit(url)
    except ValueError:
        return False
    target_path = target.path or "/"
    target_identity = (
        target.scheme.lower(),
        target.netloc.lower(),
        target_path,
        target.query,
        target.fragment,
    )
    for page in ctx.pages_by_norm.get(norm_url(url), []):
        if page.status_code is None:
            continue
        try:
            candidate = urllib.parse.urlsplit(page.url)
        except ValueError:
            continue
        candidate_identity = (
            candidate.scheme.lower(),
            candidate.netloc.lower(),
            candidate.path or "/",
            candidate.query,
            candidate.fragment,
        )
        if candidate_identity == target_identity:
            return True
    return False


def _has_column(ctx: AuditContext, field: str) -> bool:
    """Whether ``Internal:All`` carries the source column for ``field`` at all.

    Same distinction as #205 (see check_titles): an absent column and a column
    full of blanks both read back as ``None``/falsy per page, but they are
    different facts — one means the run never measured this, the other that
    every page genuinely fails. Only the column's presence, never a passing
    value showing up somewhere in the corpus, may decide whether a check ran.
    """
    from .normalize import INTERNAL_FIELD_MAP, find_column

    return (
        ctx.internal_df is not None
        and find_column(ctx.internal_df, INTERNAL_FIELD_MAP[field]) is not None
    )


def _path_of(url: str) -> str:
    # the path only — query/fragment must not leak into path-based checks
    return urllib.parse.urlsplit(url).path


def _decoded_path(url: str) -> str:
    """The path as a reader sees it, with percent-escapes resolved.

    Exports carry the URL as crawled, so a path written in a non-Latin script
    arrives percent-encoded, one three-character escape per byte. RFC 3986
    prefers uppercase hex digits in those escapes, which makes the encoded form
    of every such URL look uppercase and none of them look non-ASCII — the
    exact opposite of the truth. Both questions are about the characters the
    path denotes, so both are asked of the decoded form.
    """
    return urllib.parse.unquote(_path_of(url))


# --------------------------------------------------------------------------
# 7.A — response codes & indexing
# --------------------------------------------------------------------------
def check_response_codes(ctx: AuditContext) -> None:
    for page in ctx.pages:
        code = page.status_code
        if code is None:
            continue
        if code == 0:
            ctx.add(
                "NO_RESPONSE", target_url=page.url, status_code=0, details={"status": page.status}
            )
        elif 400 <= code <= 499:
            ctx.add(
                "BROKEN_PAGE_4XX",
                target_url=page.url,
                status_code=code,
                details={"status": page.status, "inlinks": _rec(page).get("inlinks")},
            )
        elif 500 <= code <= 599:
            ctx.add(
                "SERVER_ERROR_5XX",
                target_url=page.url,
                status_code=code,
                details={"status": page.status},
            )


def check_indexability(ctx: AuditContext) -> None:
    for page in ctx.pages:
        status = (page.indexability_status or "").lower()
        if "blocked by robots" in status:
            inlinks = _rec(page).get("inlinks")
            ctx.add(
                "BLOCKED_BY_ROBOTS",
                target_url=page.url,
                details={"indexability_status": page.indexability_status, "inlinks": inlinks},
            )
            # blocked AND internally linked => robots blocks crawl of a live, linked page
            if inlinks and inlinks > 0:
                ctx.add(
                    "IMPORTANT_URL_BLOCKED_BY_ROBOTS",
                    target_url=page.url,
                    details={"inlinks": inlinks, "indexability_status": page.indexability_status},
                )
        if page.is_html and not page.is_indexable:
            inlinks = _rec(page).get("inlinks")
            if inlinks is not None and inlinks > 0 and page.status_code == 200:
                ctx.add(
                    "NON_INDEXABLE_LINKED",
                    target_url=page.url,
                    details={"indexability_status": page.indexability_status, "inlinks": inlinks},
                )


# Permanence is carried by the status code, not by the Redirect Type column:
# that column names the mechanism ("HTTP Redirect", "HSTS Policy",
# "JavaScript Redirect", "MetaRefresh Redirect") and never the word temporary.
TEMPORARY_REDIRECTS = (302, 303, 307)


def check_redirect_type(ctx: AuditContext) -> None:
    for page in ctx.pages:
        if page.status_code not in TEMPORARY_REDIRECTS:
            continue
        rec = _rec(page)
        ctx.add(
            "BAD_REDIRECT_TYPE",
            target_url=page.url,
            status_code=page.status_code,
            details={
                "redirect_type": rec.get("redirect_type"),
                "redirect_url": rec.get("redirect_url"),
            },
        )


# --------------------------------------------------------------------------
# 7.C — title & meta description
# --------------------------------------------------------------------------
def check_titles(ctx: AuditContext) -> None:
    from .normalize import INTERNAL_FIELD_MAP, find_column

    # An absent Title column and a present-but-blank cell both read as `None` off the
    # record, but they are different facts: one means the export never carried the
    # measurement, the other that the page really has no title. Reporting the former as
    # TITLE_MISSING manufactures a sitewide finding out of a column the run never had (#205).
    if ctx.internal_df is None or find_column(ctx.internal_df, INTERNAL_FIELD_MAP["title"]) is None:
        ctx.skip("TITLE_MISSING", "no Title column in Internal:All")
        return
    t = ctx.thresholds
    pages = ctx.indexable_html_pages()
    _skip_for_body_unavailable(ctx, "TITLE_MISSING", pages)
    for page in pages:
        rec = _rec(page)
        title = rec.get("title")
        if not title:
            # #243: an oversized, unparsed body left this blank too, but nobody looked --
            # reporting TITLE_MISSING here would fabricate a finding about metadata the
            # run never measured.
            if not _body_unavailable(rec):
                ctx.add("TITLE_MISSING", target_url=page.url)
            continue
        length = rec.get("title_length")
        if length is None:  # 0 is a valid length; only fall back when truly absent
            length = len(str(title))
        px = rec.get("title_px")
        if length > t["title_max_chars"] or (px and px > t["title_max_px"]):
            ctx.add(
                "TITLE_TOO_LONG",
                target_url=page.url,
                details={
                    "title": title,
                    "length": length,
                    "pixel_width": px,
                    "max_chars": t["title_max_chars"],
                },
            )
        elif length < t["title_min_chars"]:
            ctx.add(
                "TITLE_TOO_SHORT",
                target_url=page.url,
                details={"title": title, "length": length, "min_chars": t["title_min_chars"]},
            )
        h1 = rec.get("h1")
        if h1 and str(h1).strip() == str(title).strip():
            ctx.add("TITLE_EQUALS_H1", target_url=page.url, details={"value": title})


def check_descriptions(ctx: AuditContext) -> None:
    from .normalize import INTERNAL_FIELD_MAP, find_column

    # DESC_MULTIPLE (#385): a distinct piece of evidence from "meta_description" below --
    # a native crawl counts every live occurrence while still keeping the first one as the
    # authoritative value everything else here reads, so this is evaluated independently of
    # whether the Meta Description column itself is present in this run's evidence.
    if _has_column(ctx, "meta_description_count"):
        for page in ctx.indexable_html_pages():
            count = _rec(page).get("meta_description_count")
            if count is not None and count > 1:
                ctx.add("DESC_MULTIPLE", target_url=page.url, details={"count": count})
    else:
        ctx.skip("DESC_MULTIPLE", "no meta description count evidence (native crawl only)")

    # Same distinction as check_titles (#205): an absent Meta Description column is not
    # evidence that every page lacks one.
    if (
        ctx.internal_df is None
        or find_column(ctx.internal_df, INTERNAL_FIELD_MAP["meta_description"]) is None
    ):
        ctx.skip("DESC_MISSING", "no Meta Description column in Internal:All")
        return
    t = ctx.thresholds
    pages = ctx.indexable_html_pages()
    _skip_for_body_unavailable(ctx, "DESC_MISSING", pages)
    for page in pages:
        rec = _rec(page)
        desc = rec.get("meta_description")
        if not desc:
            if not _body_unavailable(rec):  # #243: unparsed, not genuinely absent
                ctx.add("DESC_MISSING", target_url=page.url)
            continue
        length = rec.get("desc_length")
        if length is None:
            length = len(str(desc))
        px = rec.get("desc_px")
        if length > t["desc_max_chars"] or (px and px > t["desc_max_px"]):
            ctx.add(
                "DESC_TOO_LONG",
                target_url=page.url,
                details={"length": length, "max_chars": t["desc_max_chars"]},
            )
        elif length < t["desc_min_chars"]:
            ctx.add(
                "DESC_TOO_SHORT",
                target_url=page.url,
                details={"length": length, "min_chars": t["desc_min_chars"]},
            )


# --------------------------------------------------------------------------
# 7.D — headings (incl. multiple H1)
# --------------------------------------------------------------------------
def check_headings(ctx: AuditContext) -> None:
    from .normalize import INTERNAL_FIELD_MAP, find_column

    t = ctx.thresholds
    require_h2 = ctx.requirements.get("require_h2", False)
    if not require_h2:
        # A requirement that is off makes its check unreachable, not clean. Without
        # this the run reports H2_MISSING as silent -- the bucket that means "ran over
        # every page and found nothing" -- on a site where thousands of pages have no
        # H2 at all (#635). The reason names the setting, so a reader can turn it on
        # rather than conclude the site passed.
        ctx.skip("H2_MISSING", "requirements.require_h2 is false; the check was not evaluated")
    # Same distinction as check_titles (#205): an absent H1-1 column means the run never
    # measured any page's H1, not that every page is missing one. H1_MULTIPLE/H1_TOO_LONG/
    # H2_MISSING all read the same `h1` value, so they stay correctly silent on their own —
    # only H1_MISSING turns "not measured" into a false page finding.
    has_h1_column = (
        ctx.internal_df is not None
        and find_column(ctx.internal_df, INTERNAL_FIELD_MAP["h1"]) is not None
    )
    pages = ctx.indexable_html_pages()
    if not has_h1_column:
        ctx.skip("H1_MISSING", "no H1-1 column in Internal:All")
    else:
        _skip_for_body_unavailable(ctx, "H1_MISSING", pages)
    for page in pages:
        rec = _rec(page)
        h1 = rec.get("h1")
        h1_2 = rec.get("h1_2")
        if has_h1_column and not h1 and not _body_unavailable(rec):  # #243
            ctx.add("H1_MISSING", target_url=page.url)
        if h1_2:
            # Preserve each H1 value so reports identify which headings caused
            # the multiple-H1 finding instead of merely returning a count.
            ctx.add(
                "H1_MULTIPLE",
                target_url=page.url,
                details={
                    "h1_count": page.metrics["h1_count"],
                    "h1_texts": [v for v in (h1, h1_2) if v],
                },
            )
        h1_len = rec.get("h1_length")
        if h1_len is None and h1:  # 0 is a valid length; only fall back when truly absent
            h1_len = len(str(h1))
        if h1 and h1_len and h1_len > t["h1_max_chars"]:
            ctx.add(
                "H1_TOO_LONG",
                target_url=page.url,
                details={"length": h1_len, "max_chars": t["h1_max_chars"]},
            )
        # H1_ALT_TEXT_ONLY (#385): additive to h1/h1_2 above, never a substitute for them --
        # an H1 whose only content is an alt-bearing image still reads as empty for
        # H1_MISSING/H1_MULTIPLE/H1_TOO_LONG exactly as before, matching how a text-only
        # reading of the page (and Google's own crawl of the raw HTML) sees it. A logo
        # sitting *beside* real heading text never reaches this: h1_alt_text is only ever
        # populated when the H1's own text was empty (see parser.h1_alt_only_text).
        alt_text = rec.get("h1_alt_text")
        if alt_text:
            ctx.add("H1_ALT_TEXT_ONLY", target_url=page.url, details={"alt_text": alt_text})
        h2 = rec.get("h2")
        if require_h2 and h1 and not h2:
            ctx.add("H2_MISSING", target_url=page.url, details={"h1": h1})
        # H2_TOO_LONG (#385): no dedicated length column exists for H2 on either an SF
        # export or a native crawl (unlike title/H1), so the length is measured directly
        # off the value itself -- the same fallback check_titles/check_descriptions use
        # when their own length column is absent.
        if h2 and len(str(h2)) > t.get("h2_max_chars", 70):
            ctx.add(
                "H2_TOO_LONG",
                target_url=page.url,
                details={"length": len(str(h2)), "max_chars": t.get("h2_max_chars", 70)},
            )
    if not _has_column(ctx, "h1_alt_text"):
        ctx.skip("H1_ALT_TEXT_ONLY", "no H1 alt-text evidence (native crawl only)")


# How many heading texts a finding quotes back. Enough to recognise the block in
# a template, short of pasting a mega-menu into the audit.
_HEADING_EVIDENCE_TEXTS = 5


def check_heading_outline(ctx: AuditContext) -> None:
    """The outline as a sequence and as a map of the page (#632).

    ``check_headings`` above reads h1/h1_2/h2 -- headings as an unordered set,
    which is all a Screaming Frog export carries. A page whose DOM order is
    ``H2, H2, ..., H1, H2`` satisfies every check there, and so does a page whose
    H2s are all menu labels in the masthead. Both need the stored outline, which
    only a native crawl records.
    """
    from seohead.checks.link_position import CHROME_POSITIONS

    if not _has_column(ctx, "heading_outline"):
        for check_id in ("HEADING_BEFORE_H1", "HEADING_IN_PAGE_CHROME", "HEADING_SKIP"):
            ctx.skip(check_id, "no heading outline evidence (native crawl only)")
        return
    pages = ctx.indexable_html_pages()
    for check_id in ("HEADING_BEFORE_H1", "HEADING_IN_PAGE_CHROME", "HEADING_SKIP"):
        _skip_for_body_unavailable(ctx, check_id, pages)
    unplaced = 0
    chrome_pages: dict[tuple[tuple[str, int, str], ...], list[str]] = defaultdict(list)
    chrome_details: dict[tuple[tuple[str, int, str], ...], dict[str, Any]] = {}
    for page in pages:
        outline = _rec(page).get("heading_outline")
        if not isinstance(outline, list) or not outline:
            continue
        first_h1 = next((i for i, h in enumerate(outline) if h.get("level") == 1), None)
        # None is a page with no H1 at all (H1_MISSING's finding, not this one) and
        # 0 is the H1 leading its own outline, which is the shape being asked for.
        if first_h1:
            preceding = outline[:first_h1]
            ctx.add(
                "HEADING_BEFORE_H1",
                target_url=page.url,
                details={
                    "count": len(preceding),
                    "levels": sorted({f"h{h['level']}" for h in preceding}),
                    "first_texts": [h["text"] for h in preceding[:_HEADING_EVIDENCE_TEXTS]],
                },
            )
        chrome = [h for h in outline if h.get("region") in CHROME_POSITIONS]
        if chrome:
            # One finding per page, not per heading: a masthead with eighteen
            # menu labels is one template to fix, and eighteen rows of it would
            # bury every other finding about the page. Pages that share the same
            # chrome headings become one finding with a page count, below.
            pattern = tuple((str(h["region"]), int(h["level"]), str(h["text"])) for h in chrome)
            chrome_pages[pattern].append(page.url)
            chrome_details.setdefault(
                pattern,
                {
                    "count": len(chrome),
                    "regions": sorted({str(h["region"]) for h in chrome}),
                    "first_headings": [
                        {"region": h["region"], "level": h["level"], "text": h["text"]}
                        for h in chrome[:_HEADING_EVIDENCE_TEXTS]
                    ],
                },
            )
        # A jump is meaningful only in the page's named content region. Header,
        # navigation, sidebar and footer headings are template furniture, and an
        # unplaced heading has no evidence that it belongs to the document body.
        # Keeping both out prevents a modal or masthead widget from producing a
        # false H2 -> H4 report about the page itself.
        content_outline = [h for h in outline if h.get("region") == "content"]
        jumps = [
            (previous, current)
            for previous, current in itertools.pairwise(content_outline)
            if current["level"] > previous["level"] + 1
        ]
        if jumps:
            ctx.add(
                "HEADING_SKIP",
                target_url=page.url,
                details={
                    "count": len(jumps),
                    "first_jumps": [
                        {
                            "from_level": previous["level"],
                            "from_text": previous["text"],
                            "to_level": current["level"],
                            "to_text": current["text"],
                        }
                        for previous, current in jumps[:_HEADING_EVIDENCE_TEXTS]
                    ],
                },
            )
        # A region of "" is a heading the parser could not place: the document
        # named no content landmark and no position rule matched it, so calling
        # it content would be a verdict nobody measured (see parser.heading_outline).
        if any(not h.get("region") for h in outline):
            unplaced += 1
    for pattern, urls in chrome_pages.items():
        ctx.add_site_wide("HEADING_IN_PAGE_CHROME", urls, chrome_details[pattern])
    if unplaced:
        ctx.skip(
            "HEADING_IN_PAGE_CHROME",
            f"{unplaced} page(s) with no content landmark and no matching position rule "
            "-- no heading region could be determined",
        )


# How many placement examples a finding quotes back. Enough to recognise the
# template block, short of pasting a mega-menu into the audit.
_PLACEMENT_EVIDENCE_ITEMS = 5


def check_link_placement(ctx: AuditContext) -> None:
    """LINK_INSIDE_HEADING / IMAGE_LINK_WITHOUT_TEXT — where a page's anchors sit (#634).

    Neither defect is answerable from a link's page region. ``position`` says a link
    is in ``content``; it cannot say the link is the page's own H1, and it says
    nothing about whether the anchor contains anything a reader could read. Both
    need the anchor's own ancestor chain and contents, which only a native crawl
    records (``tools.parser.link_placement``); a Screaming Frog export has no such
    column, so both checks skip there by name.

    One finding per page, not per link: a masthead logo wrapped in an H1 is one
    template to fix, and a row per occurrence would bury every other finding about
    the page -- the same reasoning ``check_heading_outline`` applies to a chrome
    heading.
    """
    if not _has_column(ctx, "link_placement"):
        for check_id in ("LINK_INSIDE_HEADING", "IMAGE_LINK_WITHOUT_TEXT"):
            ctx.skip(check_id, "no link-placement evidence (native crawl only)")
        return
    pages = ctx.indexable_html_pages()
    for check_id in ("LINK_INSIDE_HEADING", "IMAGE_LINK_WITHOUT_TEXT"):
        _skip_for_body_unavailable(ctx, check_id, pages)
    unmeasured = 0
    for page in pages:
        placement = _rec(page).get("link_placement")
        if not isinstance(placement, dict):
            # A page whose HTML this run never parsed, or a scan written before
            # placement was recorded. Neither is "no links in headings" -- count
            # it and declare the reason below rather than pass it as clean.
            unmeasured += 1
            continue
        in_heading = placement.get("in_heading") or []
        if in_heading:
            ctx.add(
                "LINK_INSIDE_HEADING",
                target_url=page.url,
                occurrences_count=int(placement.get("in_heading_total") or len(in_heading)),
                details={
                    "count": int(placement.get("in_heading_total") or len(in_heading)),
                    "levels": sorted({f"h{item['level']}" for item in in_heading}),
                    "first_links": [
                        {
                            "level": item["level"],
                            "anchor": item["anchor"],
                            "destination": item["destination"],
                        }
                        for item in in_heading[:_PLACEMENT_EVIDENCE_ITEMS]
                    ],
                },
            )
        image_links = placement.get("image_no_text") or []
        if image_links:
            ctx.add(
                "IMAGE_LINK_WITHOUT_TEXT",
                target_url=page.url,
                occurrences_count=int(placement.get("image_no_text_total") or len(image_links)),
                details={
                    "count": int(placement.get("image_no_text_total") or len(image_links)),
                    "first_destinations": [
                        item["destination"] for item in image_links[:_PLACEMENT_EVIDENCE_ITEMS]
                    ],
                },
            )
    if unmeasured:
        for check_id in ("LINK_INSIDE_HEADING", "IMAGE_LINK_WITHOUT_TEXT"):
            ctx.skip(
                check_id,
                f"{unmeasured} page(s) carry no link-placement evidence -- their anchors "
                "were never inspected for this",
            )


# --------------------------------------------------------------------------
# 7.E — canonical & directives
# --------------------------------------------------------------------------
def check_canonical_directives(ctx: AuditContext) -> None:
    require_canonical = ctx.requirements.get("require_canonical", True)
    has_meta_keywords = _has_column(ctx, "meta_keywords")
    if require_canonical:
        _skip_for_body_unavailable(
            ctx, "CANONICAL_MISSING", [p for p in ctx.html_pages() if p.is_indexable]
        )
    else:
        # Same shape as require_h2 above: the default is true, so this branch is rare,
        # but a run that turns it off must say the check was not evaluated rather than
        # let it read as a site with canonicals everywhere (#635).
        ctx.skip(
            "CANONICAL_MISSING",
            "requirements.require_canonical is false; the check was not evaluated",
        )
    for page in ctx.html_pages():
        rec = _rec(page)
        canonical = rec.get("canonical")
        # CANONICAL_MISSING stays tied to source indexability: a non-indexable page (e.g.
        # noindex'd, or itself already canonicalised elsewhere) is not expected to declare
        # its own canonical.
        if (
            page.is_indexable
            and require_canonical
            and not canonical
            and not _body_unavailable(rec)  # #243: unparsed, not genuinely absent
        ):
            ctx.add("CANONICAL_MISSING", target_url=page.url)
        # CANONICALISED / CANONICAL_NON_INDEXABLE evaluate independently of source
        # indexability (#333). A fetched page's own Indexability/Indexability Status often
        # already reads Non-Indexable/Canonicalised precisely *because* it carries this
        # cross-URL canonical — gating on is_indexable made the check swallow the very
        # relationship it exists to report, on both a native crawl's projection and an
        # equivalently-shaped SF export.
        if canonical and norm_url(canonical) != norm_url(page.url):
            ctx.add("CANONICALISED", target_url=page.url, details={"canonical": canonical})
            # Match the canonical target tolerant of trailing slash / case — and read
            # every page under that key, since a site serving both slash forms has two
            # (issue #95). "The canonical points at something non-indexable" is only true
            # when no page under the key is indexable.
            targets = ctx.pages_by_norm.get(norm_url(canonical)) or []
            target_codes = [t.status_code for t in targets]
            target_is_redirect = any(
                code is not None and 300 <= int(code) <= 399 for code in target_codes
            )
            target_is_http_error = bool(target_codes) and all(
                code is not None and 400 <= int(code) <= 599 for code in target_codes
            )
            if (
                targets
                and not any(t.is_indexable for t in targets)
                and (
                    any(code is not None and 200 <= int(code) <= 299 for code in target_codes)
                    or (not target_is_redirect and not target_is_http_error)
                )
            ):
                ctx.add(
                    "CANONICAL_NON_INDEXABLE",
                    target_url=page.url,
                    details={
                        "canonical": canonical,
                        "canonical_target_evidence": [
                            {
                                "url": target.url,
                                "status_code": target.status_code,
                                "indexability": target.indexability,
                                "indexability_status": target.indexability_status,
                            }
                            for target in targets[:20]
                        ],
                        "canonical_target_evidence_omitted": max(0, len(targets) - 20),
                    },
                )
        robots = robots_directives(rec.get("meta_robots"), rec.get("x_robots"))
        if "noindex" in robots:
            ctx.add("NOINDEX", target_url=page.url, details={"meta_robots": rec.get("meta_robots")})
        elif "nofollow" in robots and page.is_indexable:
            ctx.add(
                "NOFOLLOW_PAGE",
                target_url=page.url,
                details={"meta_robots": rec.get("meta_robots")},
            )
        if has_meta_keywords and rec.get("meta_keywords"):
            ctx.add(
                "META_KEYWORDS_PRESENT",
                target_url=page.url,
                details={"value": rec.get("meta_keywords")},
            )
    if not has_meta_keywords:
        ctx.skip("META_KEYWORDS_PRESENT", "no Meta Keywords 1 column in Internal:All")


def check_canonical_policy(ctx: AuditContext) -> None:
    """Compare configured pagination/filter canonicals with fetched URL evidence.

    An empty policy is explicitly unmeasured. If any matched source's declared
    or expected target is absent from Internal:All, withhold that category's
    findings: an unfetched target cannot establish the policy relationship.
    """
    policy = ctx.config.get("canonical_policy", {})
    for category, check_id in (
        ("pagination", "PAGINATION_CANONICAL_POLICY"),
        ("filters", "FILTER_CANONICAL_POLICY"),
    ):
        if not ctx.enabled(check_id):
            continue
        rules = policy.get(category, [])
        if not rules:
            ctx.skip(check_id, f"no {category} canonical policy configured")
            continue
        if not _has_column(ctx, "canonical"):
            ctx.skip(check_id, "no Canonical column in Internal:All")
            continue

        candidates: list[tuple[Page, dict[str, Any], str, str | None]] = []
        unavailable: list[tuple[str, str]] = []
        for page in ctx.html_pages():
            rule = matching_canonical_rule(policy, category, page.url)
            if rule is None:
                continue
            if _body_unavailable(_rec(page)):
                unavailable.append((page.url, "source HTML body unavailable"))
                continue
            expected = page.url if rule["policy"] == "self" else rule["target"]
            canonical = _rec(page).get("canonical") or None
            if not _canonical_policy_target_is_fetched(ctx, expected):
                unavailable.append(
                    (page.url, f"expected target absent from crawl evidence: {expected}")
                )
                continue
            if canonical and not _canonical_policy_target_is_fetched(ctx, canonical):
                unavailable.append(
                    (page.url, f"declared canonical absent from crawl evidence: {canonical}")
                )
                continue
            candidates.append((page, rule, expected, canonical))

        if not candidates and not unavailable:
            ctx.skip(check_id, f"no crawled URLs matched the configured {category} patterns")
            continue
        if unavailable:
            examples = "; ".join(f"{source}: {reason}" for source, reason in unavailable[:3])
            ctx.skip(
                check_id,
                f"canonical policy evidence unavailable for {len(unavailable)} "
                f"matched URL(s); examples: {examples}",
            )
            continue

        for page, rule, expected, canonical in candidates:
            if canonical and norm_url(canonical) == norm_url(expected):
                continue
            ctx.add(
                check_id,
                target_url=page.url,
                details={
                    "canonical_policy": {
                        "source_url": page.url,
                        "declared_canonical_url": canonical,
                        "expected_canonical_url": expected,
                        "policy": rule["policy"],
                        "matched_pattern": rule["pattern"],
                    }
                },
                evidence={
                    "frame": "Internal:All",
                    "canonical_column": "Canonical Link Element 1",
                    "expected_target_in_crawl": True,
                },
            )


# --------------------------------------------------------------------------
# 7.F — content: thin & near-duplicates (exact dupes handled in groups)
# --------------------------------------------------------------------------
def check_content(ctx: AuditContext) -> None:
    from .normalize import INTERNAL_FIELD_MAP, find_column

    t = ctx.thresholds
    # An absent frame inventory and a page that frames nothing both read as
    # falsy off the record, and they are different facts. Only a native crawl
    # produces these columns; a Screaming Frog export has no iframe data at all,
    # and a silent CONTENT_IN_IFRAME there would let "nobody looked" render as
    # "nothing framed" -- with a THIN_CONTENT finding standing beside it that
    # nothing checked against the framing explanation (#360).
    frames_known = ctx.internal_df is not None and (
        find_column(ctx.internal_df, INTERNAL_FIELD_MAP["content_frames_same_origin"]) is not None
    )
    if not frames_known:
        ctx.skip("CONTENT_IN_IFRAME", "no iframe inventory in this evidence")
    has_text_ratio = False
    low_ratio: list[tuple[str, float]] = []
    for page in ctx.indexable_html_pages():
        rec = _rec(page)
        if _body_unavailable(rec):
            continue
        wc = rec.get("word_count")
        if wc is not None and wc < t["thin_content_words"]:
            # A page can be below the threshold for two different reasons, and
            # they need opposite advice. If its content area frames a document
            # the site itself serves, the copy exists -- it is simply somewhere
            # a search engine credits to the framed URL rather than this one.
            # Reporting that page as thin names the wrong cause and asks the
            # operator to write copy they have already written (#360). Only a
            # same-origin frame counts: an embedded video or map is normal, and
            # a check that fired on every YouTube embed would be ignored.
            #
            # Nothing special is needed for rendering.browser.flatten_iframes.
            # It replaces the iframe element with the framed body before
            # capture, so the evidence this reads has no frame left to count
            # and a full word count besides -- neither finding fires.
            framed = rec.get("content_frames_same_origin") if frames_known else None
            if framed:
                ctx.add(
                    "CONTENT_IN_IFRAME",
                    target_url=page.url,
                    details={
                        "word_count": wc,
                        "threshold": t["thin_content_words"],
                        "same_origin_frames_in_content_area": framed,
                        "frames_in_content_area": rec.get("content_frames"),
                    },
                )
            else:
                ctx.add(
                    "THIN_CONTENT",
                    target_url=page.url,
                    details={"word_count": wc, "threshold": t["thin_content_words"]},
                )
        ratio = rec.get("text_ratio")
        if ratio is not None:
            has_text_ratio = True
            if ratio < t["low_text_ratio_pct"]:
                low_ratio.append((page.url, ratio))
        near = rec.get("near_duplicates")
        sim = rec.get("closest_similarity")
        if near is not None and near > 0:
            ctx.add(
                "NEAR_DUPLICATE",
                target_url=page.url,
                details={"near_duplicates": near, "closest_similarity": sim},
            )
    if low_ratio:
        # A template that is mostly markup puts every page under the line; one
        # finding with the page count and the spread says so without a row per page.
        ratios = [ratio for _, ratio in low_ratio]
        threshold = t["low_text_ratio_pct"]
        if len(low_ratio) == 1:
            details = {"text_ratio": ratios[0], "threshold": threshold}
        else:
            details = {
                "text_ratio_min": min(ratios),
                "text_ratio_max": max(ratios),
                "threshold": threshold,
            }
        ctx.add_site_wide("LOW_TEXT_RATIO", [url for url, _ in low_ratio], details)
    if not has_text_ratio:
        ctx.skip("LOW_TEXT_RATIO", "no Text Ratio column in Internal:All")


# --------------------------------------------------------------------------
# 7.I/7.J — URL hygiene, depth, performance, security
# --------------------------------------------------------------------------
def check_url_and_perf(ctx: AuditContext) -> None:
    t = ctx.thresholds
    has_depth = has_inlinks = has_response_time = False
    for page in ctx.html_pages():
        rec = _rec(page)
        url = page.url
        path = _decoded_path(url)
        if len(url) > t["url_max_chars"]:
            ctx.add(
                "URL_TOO_LONG",
                target_url=url,
                details={"length": len(url), "max_chars": t["url_max_chars"]},
            )
        if (
            "?" in url
            and page.is_indexable
            and not rec.get("canonical")
            and not _body_unavailable(rec)
        ):
            ctx.add("URL_HAS_PARAMS", target_url=url)
        if urllib.parse.urlsplit(url).query:
            ctx.add("URL_HAS_PARAMETERS", target_url=url)
        if NON_ASCII.search(path):
            ctx.add("URL_NON_ASCII", target_url=url)
        if path != path.lower():  # True iff the path has an uppercase letter
            ctx.add("URL_UPPERCASE", target_url=url)
        if url.startswith("http://"):
            ctx.add("HTTP_URL", target_url=url)
        depth = rec.get("crawl_depth")
        if depth is not None:
            has_depth = True
            if depth > t["crawl_depth_max"]:
                ctx.add(
                    "DEEP_CRAWL_DEPTH",
                    target_url=url,
                    details={"crawl_depth": depth, "max": t["crawl_depth_max"]},
                )
        inlinks = rec.get("inlinks")
        # depth != 0 excludes the homepage; missing depth (None) still counts
        if inlinks is not None:
            has_inlinks = True
            if inlinks < t["orphan_inlinks_min"] and page.is_indexable and depth != 0:
                ctx.add("ORPHAN_PAGE", target_url=url, details={"inlinks": inlinks})
        rt = rec.get("response_time")
        if rt is not None:
            has_response_time = True
            if rt > t["response_time_max_s"]:
                ctx.add(
                    "SLOW_RESPONSE",
                    target_url=url,
                    details={"response_time": rt, "max_s": t["response_time_max_s"]},
                )
    if not has_depth:
        ctx.skip("DEEP_CRAWL_DEPTH", "no Crawl Depth column in Internal:All")
    if not has_inlinks:
        ctx.skip("ORPHAN_PAGE", "no Inlinks column in Internal:All")
    if not has_response_time:
        ctx.skip("SLOW_RESPONSE", "no Response Time column in Internal:All")


def check_schema(ctx: AuditContext) -> None:
    found_any = False
    has_parse_evidence = _has_column(ctx, "structured_data_parsed")
    for page in ctx.html_pages():
        rec = _rec(page)
        ve = rec.get("validation_errors")
        if ve is not None:
            found_any = True
        if ve and ve > 0:
            ctx.add(
                "SCHEMA_VALIDATION_ERROR", target_url=page.url, details={"validation_errors": ve}
            )
        # STRUCTURED_DATA_PARSE_ERROR (#386): "found" and "parsed" are both already recorded
        # per page by a native crawl; nothing read them together until now. A block that is
        # merely absent reads found == parsed == 0 and never fires -- only found > parsed,
        # meaning at least one block existed in the markup and failed to parse as JSON, is
        # a defect distinct from STRUCTURED_DATA_MISSING.
        if has_parse_evidence:
            found = rec.get("structured_data") or 0
            parsed = rec.get("structured_data_parsed") or 0
            if found > parsed:
                ctx.add(
                    "STRUCTURED_DATA_PARSE_ERROR",
                    target_url=page.url,
                    details={"blocks_found": found, "blocks_parsed": parsed},
                )
    if not found_any:
        ctx.skip("SCHEMA_VALIDATION_ERROR", "no Structured Data validation columns in Internal:All")
    if not has_parse_evidence:
        ctx.skip(
            "STRUCTURED_DATA_PARSE_ERROR",
            "no JSON-LD found/parsed block counts (native crawl only)",
        )


# --------------------------------------------------------------------------
# Duplicate grouping (TITLE / DESC / HASH) — emits groups + per-URL issues
# --------------------------------------------------------------------------
def check_duplicates(ctx: AuditContext) -> None:
    by_title: dict[str, list[str]] = defaultdict(list)
    by_desc: dict[str, list[str]] = defaultdict(list)
    by_hash: dict[str, list[str]] = defaultdict(list)
    by_h1: dict[str, list[str]] = defaultdict(list)
    by_h2: dict[str, list[str]] = defaultdict(list)
    has_hash = False
    for page in ctx.indexable_html_pages():
        rec = _rec(page)
        if rec.get("title"):
            by_title[str(rec["title"]).strip()].append(page.url)
        if rec.get("meta_description"):
            by_desc[str(rec["meta_description"]).strip()].append(page.url)
        if rec.get("hash"):
            has_hash = True
            by_hash[str(rec["hash"]).strip()].append(page.url)
        if rec.get("h1"):
            by_h1[str(rec["h1"]).strip()].append(page.url)
        if rec.get("h2"):
            by_h2[str(rec["h2"]).strip()].append(page.url)

    def emit(groups: dict[str, list[str]], check_id: str) -> None:
        for value, urls in groups.items():
            if len(urls) < 2:
                continue
            group = ctx.add_group(check_id, value, sorted(urls))
            if group is None:
                continue
            for url in urls:
                ctx.add(
                    check_id,
                    target_url=url,
                    group_id=group.group_id,
                    details={
                        "value": value if check_id != "DUPLICATE_BY_HASH" else None,
                        "duplicate_count": len(urls),
                    },
                )

    emit(by_title, "TITLE_DUPLICATE")
    emit(by_desc, "DESC_DUPLICATE")
    emit(by_h1, "H1_DUPLICATE")
    emit(by_h2, "H2_DUPLICATE")
    if has_hash:
        emit(by_hash, "DUPLICATE_BY_HASH")
    else:
        ctx.skip("DUPLICATE_BY_HASH", "no Hash/Page Hash column in Internal:All")


# --------------------------------------------------------------------------
# Extension checks — squeeze more out of the Internal:All columns
# --------------------------------------------------------------------------
def check_url_extra(ctx: AuditContext) -> None:
    for page in ctx.html_pages():
        url = page.url
        path = _path_of(url)
        # An underscore is an underscore whether it is written literally or as its
        # unreserved percent-escape (%5F/%5f); NON_ASCII and UPPERCASE already decode
        # before judging the path (see _decoded_path), so this must too or an encoded
        # spelling of an otherwise identical path escapes the check (#207).
        if "_" in _decoded_path(url):
            ctx.add("URL_UNDERSCORES", target_url=url)
        if "//" in path:
            ctx.add("URL_MULTIPLE_SLASHES", target_url=url)
        if " " in url or "%20" in url:
            ctx.add("URL_CONTAINS_SPACE", target_url=url)
        # A repeated *word* means a duplicated prefix or a crawl trap
        # (/shop/shop/, /en/products/en/). A repeated *number* means a
        # coordinate: /2024/01/01/ is the default WordPress permalink and
        # /catalog/12/12/ a pair of ids, so numeric segments are not compared.
        segs = [s for s in path.split("/") if s and not s.isdigit()]
        if len(segs) >= 2 and len(segs) != len(set(segs)):
            ctx.add("URL_REPETITIVE_PATH", target_url=url, details={"path": path})
        # Tracking params matter only on indexable URLs (else they're not going
        # to be crawled/indexed anyway).
        if page.is_indexable:
            tp = _tracking_params(url)
            if tp:
                ctx.add("URL_TRACKING_PARAMS", target_url=url, details={"params": tp})


def check_content_quality(ctx: AuditContext) -> None:
    t = ctx.thresholds
    has_read = has_awps = has_spell = has_grammar = False
    for page in ctx.indexable_html_pages():
        rec = _rec(page)
        flesch = rec.get("flesch")
        readability = rec.get("readability")
        if flesch is not None or readability is not None:
            has_read = True
            # difficult by Flesch score OR by SF's text label ("Difficult"/"Very Difficult")
            difficult = (flesch is not None and flesch < t["readability_flesch_min"]) or (
                readability is not None and "difficult" in str(readability).lower()
            )
            if difficult:
                ctx.add(
                    "READABILITY_DIFFICULT",
                    target_url=page.url,
                    details={
                        "flesch": flesch,
                        "readability": readability,
                        "min": t["readability_flesch_min"],
                    },
                )
        awps = rec.get("avg_words_per_sentence")
        if awps is not None:
            has_awps = True
            if awps > t["avg_words_per_sentence_max"]:
                ctx.add(
                    "LONG_SENTENCES",
                    target_url=page.url,
                    details={
                        "avg_words_per_sentence": awps,
                        "max": t["avg_words_per_sentence_max"],
                    },
                )
        sp = rec.get("spelling_errors")
        if sp is not None:
            has_spell = True
            if sp > 0:
                ctx.add("SPELLING_ERRORS", target_url=page.url, details={"count": sp})
        gr = rec.get("grammar_errors")
        if gr is not None:
            has_grammar = True
            if gr > 0:
                ctx.add("GRAMMAR_ERRORS", target_url=page.url, details={"count": gr})
    if not has_read:
        ctx.skip("READABILITY_DIFFICULT", "no Readability/Flesch column")
    if not has_awps:
        ctx.skip("LONG_SENTENCES", "no Average Words Per Sentence column")
    if not has_spell:
        ctx.skip("SPELLING_ERRORS", "no Spelling Errors column (enable spell-check in SF)")
    if not has_grammar:
        ctx.skip("GRAMMAR_ERRORS", "no Grammar Errors column (enable grammar-check in SF)")


# A refresh needs a non-empty ``url=`` target to redirect. A delay alone (or an
# empty target) reloads the current page instead.
_REFRESH_TARGET_RE = re.compile(r"url\s*=\s*(?P<target>\"[^\"]*\"|'[^']*'|[^;\s]+)", re.IGNORECASE)


def _has_refresh_target(refresh: str) -> bool:
    match = _REFRESH_TARGET_RE.search(refresh)
    if not match:
        return False
    target = match.group("target").strip()
    if target[:1] in {'"', "'"}:
        target = target[1:-1].strip()
    return bool(target)


def check_directives_extra(ctx: AuditContext) -> None:
    for page in ctx.html_pages():
        rec = _rec(page)
        robots = robots_directives(rec.get("meta_robots"), rec.get("x_robots"))
        if "noarchive" in robots:
            ctx.add("NOARCHIVE", target_url=page.url)
        if "nosnippet" in robots:
            ctx.add("NOSNIPPET", target_url=page.url)
        if "noimageindex" in robots:
            ctx.add("NOIMAGEINDEX", target_url=page.url)
        if "notranslate" in robots:
            ctx.add("NOTRANSLATE", target_url=page.url)
        if "noodp" in robots:
            ctx.add("NOODP", target_url=page.url)
        if "noydir" in robots:
            ctx.add("NOYDIR", target_url=page.url)
        unavailable_after = next((t for t in robots if t.startswith("unavailable_after")), None)
        if unavailable_after:
            ctx.add(
                "UNAVAILABLE_AFTER",
                target_url=page.url,
                details={"directive": unavailable_after},
            )
        refresh = str(rec.get("meta_refresh") or "")
        # A refresh that names no target reloads this same page; calling that a
        # redirect is a wrong finding, and the fix text -- "replace it with a
        # 301" -- is advice that would break the page. The check reads the same
        # declaration whichever source supplied it, so this holds for a Screaming
        # Frog export and a native crawl alike.
        if refresh and _has_refresh_target(refresh):
            ctx.add(
                "META_REFRESH_REDIRECT",
                target_url=page.url,
                details={"meta_refresh": refresh},
            )
        http_refresh = str(rec.get("http_refresh") or "")
        if http_refresh and _has_refresh_target(http_refresh):
            ctx.add(
                "HTTP_REFRESH_REDIRECT",
                target_url=page.url,
                details={"refresh_header": http_refresh},
            )
    if not _has_column(ctx, "http_refresh"):
        ctx.skip("HTTP_REFRESH_REDIRECT", "no Refresh response header evidence (native crawl only)")


def check_canonical_extra(ctx: AuditContext) -> None:
    has_canonical_2 = _has_column(ctx, "canonical_2")
    for page in ctx.html_pages():
        rec = _rec(page)
        canonical = rec.get("canonical")
        if canonical and not canonical.lower().startswith(("http://", "https://", "//")):
            ctx.add("CANONICAL_RELATIVE", target_url=page.url, details={"canonical": canonical})
        if has_canonical_2 and rec.get("canonical_2"):
            ctx.add(
                "CANONICAL_MULTIPLE",
                target_url=page.url,
                details={"canonical_1": canonical, "canonical_2": rec.get("canonical_2")},
            )
        if canonical and urllib.parse.urlsplit(canonical).fragment:
            ctx.add("CANONICAL_FRAGMENT", target_url=page.url, details={"canonical": canonical})
    if not has_canonical_2:
        ctx.skip("CANONICAL_MULTIPLE", "no Canonical Link Element 2 column in Internal:All")


# --------------------------------------------------------------------------
# Canonical-graph checks build the canonical edge graph over
# Internal:All and flag multi-hop chains and canonicals onto redirects.
# --------------------------------------------------------------------------
def _canonical_edges(ctx: AuditContext) -> tuple[dict[str, str], bool]:
    """Return (norm_url -> norm_url edge map, has_any_canonical).

    An edge A→B exists when A's canonical differs from itself. Self-canonicals
    and absent canonicals produce no edge. ``has_any_canonical`` is True iff any
    page carried a canonical value (drives the honest skip).
    """
    edges: dict[str, str] = {}
    has_any = False
    for page in ctx.html_pages():
        canonical = _rec(page).get("canonical")
        if not canonical:
            continue
        has_any = True
        a = norm_url(page.url)
        b = norm_url(canonical)
        if a != b:
            edges[a] = b
    return edges, has_any


def check_canonical_chain(ctx: AuditContext) -> None:
    """CANONICAL_CHAIN — A→B where B itself canonicalizes onward (or back).

    A page is flagged when its canonical target has an outgoing canonical edge
    of its own (1-step lookahead over the edge graph). This covers both chains
    (A→B→C) and loops (A→B→A): in either case the target re-canonicalizes, so a
    search engine may resolve the canonical unpredictably. The full path is
    reconstructed for the report.
    """
    edges, has_any = _canonical_edges(ctx)
    if not has_any:
        ctx.skip("CANONICAL_CHAIN", "no Canonical column in Internal:All")
        return
    for page in ctx.html_pages():
        start = norm_url(page.url)
        b = edges.get(start)
        if b is None or b not in edges:
            continue  # no edge, or healthy single-step canonical to a terminal
        # target re-canonicalizes — reconstruct the path for context
        path = [start]
        seen = {start}
        cur = start
        is_loop = False
        for _ in range(8):  # bounded walk — guards against pathological graphs
            nxt = edges.get(cur)
            if nxt is None:
                break
            if nxt in seen:
                is_loop = True
                break
            path.append(nxt)
            seen.add(nxt)
            cur = nxt
        # #176 audit: a representative read, but only for display — the walk above already
        # decided the chain from the edge map alone, so which variant's URL string gets
        # printed here changes nothing about whether or how CANONICAL_CHAIN fires.
        chain = []
        for n in path:
            tgt = ctx.page_by_norm.get(n)
            chain.append(tgt.url if tgt else n)
        ctx.add(
            "CANONICAL_CHAIN",
            target_url=page.url,
            details={"chain": chain, "depth": len(path) - 1, "loop": is_loop},
        )


def check_canonical_to_redirect(ctx: AuditContext) -> None:
    """CANONICAL_TO_REDIRECT — canonical target is itself a 3xx redirect.

    Cross-references the canonical URL against the crawl: a canonical target
    that responds 3xx (or carries a Redirect URL) forces an extra hop and lets
    the search engine choose its own canonical. Only targets present in the
    crawl can be classified; unknown URLs are left alone.
    """
    _, has_any = _canonical_edges(ctx)
    if not has_any:
        ctx.skip("CANONICAL_TO_REDIRECT", "no Canonical column in Internal:All")
        return
    for page in ctx.html_pages():
        canonical = _rec(page).get("canonical")
        if not canonical or norm_url(canonical) == norm_url(page.url):
            continue
        # Every crawled page under that normalised key, not one: a site that serves both
        # /x (301) and /x/ (200) has two, and the canonical points at whichever one answers.
        # Reading a single record made this fire on 78 live pages whose canonical is a 200
        # (issue #95). The claim is only true when nothing under the key answered 2xx.
        targets = ctx.pages_by_norm.get(norm_url(canonical)) or _probed_targets(
            ctx, norm_url(canonical)
        )
        if not targets:
            continue  # external / not crawled — cannot classify
        if any(t.status_code is not None and 200 <= int(t.status_code) < 300 for t in targets):
            continue
        # #176: reading targets[0] made the verdict depend on crawl order — a 404 crawled
        # before its 301 twin under the same normalised key hid a real CANONICAL_TO_REDIRECT.
        # The normalised key can't say which literal variant the canonical tag actually named,
        # so any target that answers with a redirect is enough to report one, exactly the mirror
        # of the "any 2xx clears it" guard above.
        redirecting = [
            (t, ctx.redirect_map.get(t.url) or getattr(t, "redirect_url", "") or t.url)
            for t in targets
            if (t.status_code is not None and 300 <= int(t.status_code) <= 399)
            or ctx.redirect_map.get(t.url)
            or getattr(t, "redirect_url", "")
        ]
        if not redirecting:
            continue  # every target under the key is a plain non-2xx, non-redirect response
        target, redirect_url = redirecting[0]
        ctx.add(
            "CANONICAL_TO_REDIRECT",
            target_url=page.url,
            details={
                "canonical": canonical,
                "canonical_status_code": target.status_code,
                "redirect_url": redirect_url,
            },
        )


def check_canonical_target_error(ctx: AuditContext) -> None:
    """CANONICAL_TARGET_ERROR — a fetched canonical target answers 4xx or 5xx.

    This is the status-specific sibling of CANONICAL_NON_INDEXABLE and
    CANONICAL_TO_REDIRECT. It reuses Internal:All's canonical and response-code
    graph, and only makes an error claim when every row under the normalized
    target key has a known 4xx/5xx status. An absent target or a row with no
    status is unavailable evidence, not a broken destination. A 2xx variant
    clears the claim; 3xx remains CANONICAL_TO_REDIRECT.
    """
    if not _has_column(ctx, "canonical"):
        ctx.skip("CANONICAL_TARGET_ERROR", "no Canonical column in Internal:All")
        return
    if not _has_column(ctx, "status_code"):
        ctx.skip("CANONICAL_TARGET_ERROR", "no Status Code column in Internal:All")
        return

    has_any_canonical = any(_rec(page).get("canonical") for page in ctx.html_pages())
    if not has_any_canonical:
        ctx.skip("CANONICAL_TARGET_ERROR", "no canonical values in Internal:All")
        return

    issues: list[tuple[Page, str, list[Page]]] = []
    unavailable_count = 0
    for page in ctx.html_pages():
        canonical = _rec(page).get("canonical")
        if not canonical or norm_url(canonical) == norm_url(page.url):
            continue
        targets = ctx.pages_by_norm.get(norm_url(canonical)) or _probed_targets(
            ctx, norm_url(canonical)
        )
        if not targets:
            unavailable_count += 1
            continue
        codes = [target.status_code for target in targets]
        if any(code is None for code in codes):
            unavailable_count += 1
            continue
        if any(200 <= int(code) <= 299 for code in codes):
            continue
        if any(300 <= int(code) <= 399 for code in codes):
            continue  # reported by CANONICAL_TO_REDIRECT
        if all(400 <= int(code) <= 599 for code in codes):
            issues.append((page, canonical, targets))
        else:
            unavailable_count += 1

    if unavailable_count and not issues:
        ctx.skip(
            "CANONICAL_TARGET_ERROR",
            f"{unavailable_count} canonical target(s) were not captured with classifiable response status",
        )
        return

    for page, canonical, targets in issues:
        codes = sorted(
            {int(target.status_code) for target in targets if target.status_code is not None}
        )
        ctx.add(
            "CANONICAL_TARGET_ERROR",
            target_url=page.url,
            status_code=codes[0] if len(codes) == 1 else None,
            details={
                "source_url": page.url,
                "source_status_code": page.status_code,
                "source_indexability": page.indexability,
                "canonical_target_url": canonical,
                "canonical_target_responses": [
                    {
                        "url": target.url,
                        "status_code": target.status_code,
                        "indexability": target.indexability,
                        "indexability_status": target.indexability_status,
                    }
                    for target in targets[:20]
                ],
                "canonical_target_responses_omitted": max(0, len(targets) - 20),
                "status_coverage": "partial" if unavailable_count else "complete",
                "unclassified_target_count": unavailable_count,
            },
        )


_PAGINATION_PATH_RE = re.compile(r"(?:^|/)(?:page|paged)[/-]\d+(?:/|$)", re.IGNORECASE)


def _homepage_url(url: str) -> bool:
    """Whether a canonical names an HTTP(S) origin root without query/fragment."""
    try:
        parsed = urllib.parse.urlsplit(url)
        return (
            parsed.scheme.lower() in {"http", "https"}
            and bool(parsed.hostname)
            and parsed.path in {"", "/"}
            and not parsed.query
            and not parsed.fragment
            and parsed.username is None
            and parsed.password is None
        )
    except ValueError:
        return False


def _same_home_origin(source: str, target: str) -> bool:
    """Keep protocol/host aliases and cross-host language canonicals out of the group."""
    try:
        source_parts = urllib.parse.urlsplit(source)
        target_parts = urllib.parse.urlsplit(target)
        source_port = source_parts.port or {"http": 80, "https": 443}.get(
            source_parts.scheme.lower()
        )
        target_port = target_parts.port or {"http": 80, "https": 443}.get(
            target_parts.scheme.lower()
        )
        return (
            source_parts.scheme.lower() == target_parts.scheme.lower()
            and (source_parts.hostname or "").lower() == (target_parts.hostname or "").lower()
            and source_port == target_port
        )
    except ValueError:
        return False


def check_canonical_homepage_group(ctx: AuditContext) -> None:
    """Find only strongly evidenced groups of distinct pages canonicalized home.

    A group needs at least three fetched, indexable pages across two top-level
    path sections, each with a distinct non-empty title and H1 that differ from
    the fetched homepage. Query URLs and recognizable page-number paths are
    excluded. If response, indexability, title, H1, or homepage evidence is
    missing, the check says why instead of presenting a site-wide pattern.
    """
    fields = ("canonical", "status_code", "indexability", "title", "h1")
    missing = [field for field in fields if not _has_column(ctx, field)]
    if missing:
        ctx.skip(
            "CANONICAL_HOMEPAGE_GROUP",
            "missing supporting Internal:All columns: " + ", ".join(missing),
        )
        return

    pages = ctx.html_pages()
    if not any(_rec(page).get("canonical") for page in ctx.indexable_html_pages()):
        ctx.skip("CANONICAL_HOMEPAGE_GROUP", "no canonical values in fetched indexable HTML")
        return

    configured_policy = ctx.config.get("canonical_policy")
    matching_canonical_rule = None
    if isinstance(configured_policy, dict) and any(
        configured_policy.get(key) for key in ("pagination", "filters")
    ):
        try:
            from seohead.core.canonical_policy import matching_canonical_rule
        except ImportError:
            ctx.skip(
                "CANONICAL_HOMEPAGE_GROUP",
                "canonical_policy is configured but its shared matcher is unavailable",
            )
            return

    grouped: dict[str, list[tuple[Page, str]]] = defaultdict(list)
    unavailable_home_targets = 0
    unavailable_sources = 0
    for page in pages:
        canonical = _rec(page).get("canonical")
        if (
            not canonical
            or not _homepage_url(canonical)
            or not _same_home_origin(page.url, canonical)
        ):
            continue
        source_parts = urllib.parse.urlsplit(page.url)
        if source_parts.path in {"", "/"} or source_parts.query or source_parts.fragment:
            continue  # self/home and query-based filter/pagination URLs are expected exceptions
        if _PAGINATION_PATH_RE.search(source_parts.path):
            continue
        if matching_canonical_rule and any(
            matching_canonical_rule(configured_policy, category, page.url)
            for category in ("pagination", "filters")
        ):
            continue  # #727 owns configured pagination/filter URLs
        if not page.indexability or not page.indexability.strip():
            unavailable_sources += 1
            continue
        if not page.is_indexable:
            continue
        section = next((part for part in source_parts.path.split("/") if part), "")
        if not section:
            continue
        title = str(_rec(page).get("title") or "").strip()
        h1 = str(_rec(page).get("h1") or "").strip()
        if not title or not h1 or _body_unavailable(_rec(page)):
            unavailable_sources += 1
            continue
        home_key = norm_url(canonical)
        home_rows = ctx.pages_by_norm.get(home_key) or []
        home = next((target for target in home_rows if target.is_2xx and target.is_indexable), None)
        if home is None:
            if not home_rows or any(
                target.status_code is None
                or (
                    target.is_2xx
                    and (target.indexability is None or not target.indexability.strip())
                )
                for target in home_rows
            ):
                unavailable_home_targets += 1
            continue
        home_record = _rec(home)
        home_title = str(home_record.get("title") or "").strip().casefold()
        home_h1 = str(home_record.get("h1") or "").strip().casefold()
        if not home_title or not home_h1 or _body_unavailable(home_record):
            unavailable_home_targets += 1
            continue
        if title.casefold() == home_title or h1.casefold() == home_h1:
            continue
        grouped[home_key].append((page, section))

    found = False
    for home_key, candidates in sorted(grouped.items()):
        by_url = {page.url: (page, section) for page, section in candidates}
        candidates = [by_url[url] for url in sorted(by_url)]
        titles = {str(_rec(page).get("title") or "").strip().casefold() for page, _ in candidates}
        headings = {str(_rec(page).get("h1") or "").strip().casefold() for page, _ in candidates}
        sections = {section.casefold() for _, section in candidates}
        if len(candidates) < 3 or len(sections) < 2 or len(titles) < 3 or len(headings) < 3:
            continue
        home_rows = ctx.pages_by_norm.get(home_key) or []
        home = next(target for target in home_rows if target.is_2xx and target.is_indexable)
        for page, section in candidates:
            ctx.add(
                "CANONICAL_HOMEPAGE_GROUP",
                target_url=page.url,
                status_code=page.status_code,
                locations=[{"source_url": page.url, "canonical_target_url": home.url}],
                details={
                    "canonical_target_url": home.url,
                    "canonical_target_status_code": home.status_code,
                    "canonical_target_indexability": home.indexability,
                    "source_path_section": section,
                    "source_title": _rec(page).get("title"),
                    "source_h1": _rec(page).get("h1"),
                    "source_count": len(candidates),
                    "source_sections": sorted(sections),
                    "source_titles_distinct": len(titles),
                    "source_h1_distinct": len(headings),
                    "minimum_sources": 3,
                    "minimum_sections": 2,
                    "evidence_rule": (
                        "fetched indexable sources have distinct non-empty titles and H1s, "
                        "span multiple top-level path sections, and exclude query/pagination URLs"
                    ),
                    "unavailable_home_target_count": unavailable_home_targets,
                    "unavailable_source_evidence_count": unavailable_sources,
                },
            )
        found = True

    if (unavailable_home_targets or unavailable_sources) and not found:
        ctx.skip(
            "CANONICAL_HOMEPAGE_GROUP",
            f"{unavailable_home_targets} homepage target(s) and "
            f"{unavailable_sources} source page(s) lack complete evidence",
        )


def check_unlinked_canonical(ctx: AuditContext) -> None:
    """UNLINKED_CANONICAL — a canonical target no hyperlink ever points to.

    A URL that is only ever named as *someone else's* canonical is reachable
    by a search engine following the annotation, but never by a user or crawler
    following a link — and "never" is knowable only once the crawl is complete
    (issue #15, item 4: a set difference between canonical targets and hyperlink
    targets). This reuses the canonical edge graph ``check_canonical_chain``
    already builds and Internal:All's own Inlinks column — no new export is
    needed, since Inlinks already counts every hyperlink the finished crawl
    found pointing at that URL. On a partial crawl "never" cannot be proven,
    so ``aggregate.aggregate`` withholds this finding rather than report it.
    """
    edges, has_any = _canonical_edges(ctx)
    if not has_any:
        ctx.skip("UNLINKED_CANONICAL", "no Canonical column in Internal:All")
        return
    if not any(_rec(p).get("inlinks") is not None for p in ctx.pages):
        ctx.skip("UNLINKED_CANONICAL", "no Inlinks column in Internal:All")
        return
    sources_by_target: dict[str, list[str]] = defaultdict(list)
    for source_norm, target_norm in edges.items():
        sources_by_target[target_norm].append(source_norm)
    for target_norm, source_norms in sources_by_target.items():
        # #176 audit: correct by construction. The claim here is about the live page's own
        # inlink count, and the redirecting twin under a shared key never carries that count
        # (SF attributes Inlinks to the URL that actually receives them) — the 2xx-preferring
        # representative is the only variant this check could mean.
        target = ctx.page_by_norm.get(target_norm)
        if target is None:
            continue  # canonical points outside the crawl — cannot classify
        rec = _rec(target)
        if rec.get("crawl_depth") == 0:
            continue  # the homepage is never "unlinked"
        inlinks = rec.get("inlinks")
        if inlinks is None or inlinks > 0:
            continue
        sources = sorted(
            ctx.page_by_norm[n].url if n in ctx.page_by_norm else n for n in source_norms
        )
        ctx.add(
            "UNLINKED_CANONICAL",
            target_url=target.url,
            details={"canonicalized_from": sources},
        )


def check_pagination(ctx: AuditContext) -> None:
    """PAGINATION_NONINDEXABLE — a pagination page, or its declared neighbor, cannot be indexed.

    Two independent sources feed the same finding: a page that itself declares
    rel="next"/rel="prev" while being non-indexable (the original check), and a
    rel="next"/rel="prev" *target* that the crawl reached and found non-2xx —
    a 404/3xx/5xx page 2 is exactly as unreachable as a noindex one, even when
    page 1 itself is perfectly indexable (issue #203). Only a target the crawl
    actually captured is judged; an uncrawled URL named in the relation is
    left alone, since nothing here knows its status.
    """
    reported: set[str] = set()
    for page in ctx.html_pages():
        rec = _rec(page)
        if (rec.get("rel_next") or rec.get("rel_prev")) and not page.is_indexable:
            reported.add(page.url)
            ctx.add(
                "PAGINATION_NONINDEXABLE",
                target_url=page.url,
                details={"indexability_status": page.indexability_status},
            )
    for page in ctx.html_pages():
        rec = _rec(page)
        for relation in ("rel_next", "rel_prev"):
            value = rec.get(relation)
            if not value:
                continue
            target = ctx.page_by_norm.get(norm_url(value))
            if target is None or target.url in reported:
                continue  # not in the crawl — nothing here knows its status
            if target.status_code is None:
                # The row exists but its Status Code cell was blank or
                # unparseable. "Nobody measured it" is not "it is broken": the
                # same rule that forbids missing evidence reading as clean
                # forbids it reading as a defect.
                continue
            if target.is_2xx and target.is_indexable:
                continue
            reported.add(target.url)
            ctx.add(
                "PAGINATION_NONINDEXABLE",
                target_url=target.url,
                details={
                    "indexability_status": target.indexability_status,
                    "status_code": target.status_code,
                    "relation": relation.replace("rel_", 'rel="') + '"',
                    "source_url": page.url,
                },
            )


def pagination_loops(
    next_map: dict[str, str],
) -> tuple[list[tuple[str, list[str], str]], set[str]]:
    """Every rel="next" cycle in a functional graph, visiting each node once.

    Returns ``(loops, nodes_in_a_loop)``, where each loop is
    ``(start, path, loops_back_to)``.

    Module-level and pure so the property that matters can be tested directly:
    the total number of nodes appended across all walks equals the number of
    nodes in the graph. Bounding each walk by the graph's own size instead is
    correct and quadratic -- for one terminating series of n pages, each of the
    n nodes re-walks the rest of the tail. Measured before this shape:
    4 000 / 8 000 / 16 000 pages in a single series took 0.58 s / 2.13 s / 8.29 s,
    against 0.16 / 0.30 / 0.65 after. A news or catalogue site with thousands of
    pages in one series is the ordinary case, not the pathological one.
    """
    walked: set[str] = set()
    in_loop: set[str] = set()
    loops: list[tuple[str, list[str], str]] = []
    for start in next_map:
        if start in walked:
            continue
        path, loops_to = series_from(next_map, start)
        walked.update(path)
        if loops_to is None:
            continue
        in_loop.update(path)
        loops.append((start, path, loops_to))
    return loops, in_loop


def series_from(next_map: dict[str, str], start: str) -> tuple[list[str], str | None]:
    """The forward rel="next" chain from ``start``, and the node it loops back to.

    Terminates without a hop cap: the graph is functional -- one outgoing edge
    per node -- so a walk either runs out of edges or revisits a node already in
    its own path, and it cannot be longer than the number of distinct nodes.
    """
    path = [start]
    seen = {start}
    cur = start
    while True:
        nxt = next_map.get(cur)
        if nxt is None:
            return path, None
        if nxt in seen:
            return path, nxt
        path.append(nxt)
        seen.add(nxt)
        cur = nxt


def check_pagination_series(ctx: AuditContext) -> None:
    """PAGINATION_LOOP / UNLINKED_PAGINATION_SERIES — the rel="next" graph.

    rel="next"/rel="prev" edges form a discovery graph exactly like redirects
    and canonicals: a hop's role in the series is only known once every page's
    own rel="next" is on hand, so this is a post-crawl pass over the same
    Internal:All columns ``check_pagination`` already reads (issue #15, item
    5). A loop is provable when a next-pointer walk revisits a URL already in
    its own chain, mirroring ``check_canonical_chain``. "Unlinked" — a series
    whose first page has no hyperlink inlink, so it is reachable only by
    following rel="next" from itself — can only be trusted on a complete
    crawl, so ``aggregate.aggregate`` withholds ``UNLINKED_PAGINATION_SERIES``
    on a partial one.
    """
    next_map: dict[str, str] = {}
    for page in ctx.html_pages():
        nxt = _rec(page).get("rel_next")
        if nxt:
            next_map[norm_url(page.url)] = norm_url(nxt)
    if not next_map:
        ctx.skip("PAGINATION_LOOP", 'no rel="next" column in Internal:All')
        ctx.skip("UNLINKED_PAGINATION_SERIES", 'no rel="next" column in Internal:All')
        return

    # No hop cap here: the configured redirect_hop_cap exists to bound *redirect*
    # chains, which fan out across the whole site, and reusing it verbatim let a
    # 21-page rel="next" cycle outrun a 20-hop default and read as clean (#204).
    # series_from terminates on the graph's own structure instead.
    has_predecessor = set(next_map.values())

    # #176 audit: display only, like the equivalent helper in check_canonical_chain — the
    # walk over next_map already decided the series and the loop before any URL is printed.
    def _url_of(n: str) -> str:
        page = ctx.page_by_norm.get(n)
        return page.url if page is not None else n

    loops, in_loop = pagination_loops(next_map)
    for start, path, loops_to in loops:
        ctx.add(
            "PAGINATION_LOOP",
            target_url=_url_of(start),
            details={"series": [_url_of(n) for n in path], "loops_back_to": _url_of(loops_to)},
        )

    # The cheap per-page filters run before the walk, not after it, so a full
    # chain is only ever traversed for a head that can actually produce a
    # finding — a handful per crawl, against one traversal per page if the order
    # were reversed.
    for start in next_map:
        if start in has_predecessor or start in in_loop:
            continue  # not a head of its series, or already reported as a loop
        # #176 audit: correct by construction, same reasoning as check_unlinked_canonical —
        # is_indexable and inlinks are properties of the live page, and a redirecting twin
        # under this key would report neither, so the 2xx-preferring representative is the
        # only variant "is this series head unlinked" can mean.
        page = ctx.page_by_norm.get(start)
        if page is None or not page.is_indexable or _rec(page).get("crawl_depth") == 0:
            continue
        inlinks = _rec(page).get("inlinks")
        if inlinks is None or inlinks > 0:
            continue
        path, _ = series_from(next_map, start)
        if len(path) < 2:
            continue
        ctx.add(
            "UNLINKED_PAGINATION_SERIES",
            target_url=page.url,
            details={"series": [_url_of(n) for n in path], "length": len(path)},
        )


# A page number is only read from a token that says it is one. A bare trailing
# number in a path is a year, a product id or a date just as often as a page
# index, and guessing wrong here does not produce a near-miss -- it produces a
# sequence "error" on a series that is perfectly ordered. These three tokens
# are the ones that mean "page N" and nothing else. WordPress's ``?p=`` is
# deliberately absent: it carries a post id, not a page index.
_PAGE_NUMBER_PARAMS = frozenset({"page", "paged", "pg"})
_PAGE_NUMBER_PATH_RE = re.compile(r"/(?:page|paged|pg)/(\d{1,6})(?:/|$)", re.IGNORECASE)


def pagination_page_number(url: str) -> int | None:
    """The page number ``url`` states, or ``None`` when it does not state one.

    Module-level and pure so the one thing that decides whether a series can be
    judged at all is testable on its own. Two *disagreeing* numbers (``/page/2/``
    that also carries ``?page=5``) return ``None`` for the same reason a missing
    one does: the URL does not tell us which page it is, and a check that picks
    one of them is inventing the evidence it then reports on.
    """
    parts = urllib.parse.urlsplit(url)
    seen: set[int] = set()
    for key, value in urllib.parse.parse_qsl(parts.query):
        if key.strip().lower() in _PAGE_NUMBER_PARAMS and value.isdigit():
            seen.add(int(value))
    match = _PAGE_NUMBER_PATH_RE.search(parts.path)
    if match:
        seen.add(int(match.group(1)))
    return seen.pop() if len(seen) == 1 else None


def _display_url(ctx: AuditContext, norm: str) -> str:
    """The crawled URL behind a normalized key, or the key itself when uncrawled."""
    page = ctx.page_by_norm.get(norm)
    return page.url if page is not None else norm


def _expected_step(steps: list[int]) -> int | None:
    """The step a series is judged against, or ``None`` when the steps show none.

    A run that ever steps by one is judged on +1, as it always was. Otherwise the
    series is a stride, but only when one step is at least two and accounts for a
    strict majority of the steps: a tie or a lone step is not evidence of an
    offset scheme, so it is left unjudged rather than given a stride it may not have.
    """
    if not steps:
        return None
    if 1 in steps:
        return 1
    step, count = Counter(steps).most_common(1)[0]
    return step if step >= 2 and 2 * count > len(steps) else None


def _series_unjudged_reason(stated: list[int | None], cycles: bool) -> str | None:
    """Why this series cannot be judged for a broken run, or ``None`` when it can.

    One clause per cause, each a statement about *this* series, because the
    causes are not interchangeable: a stride, a two-page series and a cycle all
    state every page number they have, so telling their operator that some URL
    does not state its number sends them to rewrite a numbering scheme that was
    never the problem. The clause is written to follow "N series where ...".
    """
    if cycles:
        return (
            "the series cycles back on itself, which PAGINATION_LOOP reports instead of this check"
        )
    # Three numbered pages is the smallest series with two steps, and two steps
    # is the smallest run in which one of them can be "the odd one".
    if len(stated) < 3:
        return (
            f"the series is only {len(stated)} pages long, and a single step cannot be "
            "both the run and the break in it"
        )
    unnumbered = sum(1 for n in stated if n is None)
    if unnumbered:
        states = "states" if unnumbered == 1 else "state"
        return (
            f"{unnumbered} of its {len(stated)} URLs {states} no page number, and a "
            "missing number is not evidence of a gap"
        )
    numbers = [n for n in stated if n is not None]
    if _expected_step([b - a for a, b in itertools.pairwise(numbers)]) is None:
        return (
            "its page numbers neither step by one nor by one stride that covers most "
            "of the series, so there is no run to have been broken (an irregular offset "
            "scheme such as 0, 10, 25, 30 looks like this)"
        )
    return None


def _unjudged_series_skip_reason(unjudged: list[tuple[str, str]], judged: int) -> str:
    """One skip reason covering every series that went unjudged, grouped by cause.

    Grouped rather than listed one per line because the causes are bounded and
    the series are not: a crawl with four hundred WordPress blogs on it should
    say so once with an example, not four hundred times.
    """
    by_reason: OrderedDict[str, list[str]] = OrderedDict()
    for head, reason in unjudged:
        by_reason.setdefault(reason, []).append(head)
    parts = "; ".join(
        f"{len(heads)} where {reason} (e.g. {heads[0]})" for reason, heads in by_reason.items()
    )
    return (
        f'{len(unjudged)} of {len(unjudged) + judged} rel="next" series could not be '
        f"judged: {parts}"
    )


def check_pagination_sequence(ctx: AuditContext) -> None:
    """PAGINATION_SEQUENCE_ERROR — a break in an otherwise contiguous page-number run.

    Walks the same ``rel="next"`` graph as ``check_pagination_series`` and reads
    each URL's own page number. What it reports is narrow on purpose, because
    "the numbers are not 1..n" is not an error and saying it is would fire on
    most ordinary paginated sites:

    * **A series may start anywhere.** ``/blog/page/4/`` onwards is what a crawl
      of a subsection, or a series whose first page lives at an unnumbered URL,
      looks like. Only the *steps* between consecutive pages are judged, never
      the first number.
    * **A series may have a stride.** ``?page=0,10,20`` is an offset, not a
      broken sequence. A series with no ``+1`` step is judged against its stride
      only when one step covers most of its steps; ``?page=1,3,5,8`` breaks at
      5 -> 8, while an irregular run with no dominant step stays unjudged.
    * **Every URL in the series must state its number.** One unreadable URL and
      the series is not evaluated at all, since a missing number is not evidence
      of a gap.
    * **A cycling series belongs to PAGINATION_LOOP**, which already reports it;
      reporting the same series twice under two names is one finding, not two.

    With those in place the finding is exactly the row's own words: a run that
    increments by one somewhere and then does not — 1, 2, 3, 7 — where pages 4
    to 6 are in nobody's chain.

    Every series left unjudged is named with its own cause, and the count is per
    series rather than per run. A crawl usually holds more than one series, and
    the WordPress shape — page one at an unnumbered ``/blog/``, the rest at
    ``/blog/page/N/`` — is *always* of the unjudgeable kind, so a run-wide "did
    anything get judged?" guard would let the ordinary case disappear behind one
    judgeable series elsewhere on the same site.
    """
    next_map: dict[str, str] = {}
    for page in ctx.html_pages():
        nxt = _rec(page).get("rel_next")
        if nxt:
            next_map[norm_url(page.url)] = norm_url(nxt)
    if not next_map:
        ctx.skip("PAGINATION_SEQUENCE_ERROR", 'no rel="next" column in Internal:All')
        return

    has_predecessor = set(next_map.values())
    heads = [start for start in next_map if start not in has_predecessor]
    if not heads:
        # Every chain closes on itself, so not one of them has a first page to
        # walk from. PAGINATION_LOOP reports the cycles; this check reports that
        # it never got a series to read.
        ctx.skip(
            "PAGINATION_SEQUENCE_ERROR",
            'every rel="next" chain in the crawl cycles back on itself, so none of them '
            "has a first page to walk from (PAGINATION_LOOP reports the cycles)",
        )
        return

    judged = 0
    unjudged: list[tuple[str, str]] = []
    for start in heads:
        path, loops_to = series_from(next_map, start)
        stated = [pagination_page_number(_display_url(ctx, n)) for n in path]
        unjudged_reason = _series_unjudged_reason(stated, loops_to is not None)
        if unjudged_reason is not None:
            unjudged.append((_display_url(ctx, path[0]), unjudged_reason))
            continue
        judged += 1
        numbers = [n for n in stated if n is not None]
        steps = [b - a for a, b in itertools.pairwise(numbers)]
        expected = _expected_step(steps)
        breaks = [
            {
                "from": _display_url(ctx, path[i]),
                "to": _display_url(ctx, path[i + 1]),
                "from_page": numbers[i],
                "to_page": numbers[i + 1],
            }
            for i, step in enumerate(steps)
            if step != expected
        ]
        if not breaks:
            continue
        ctx.add(
            "PAGINATION_SEQUENCE_ERROR",
            target_url=_display_url(ctx, path[0]),
            details={
                "series": [_display_url(ctx, n) for n in path],
                "page_numbers": numbers,
                "breaks": breaks,
            },
        )
    if unjudged:
        ctx.skip("PAGINATION_SEQUENCE_ERROR", _unjudged_series_skip_reason(unjudged, judged))


def check_links_extra(ctx: AuditContext) -> None:
    t = ctx.thresholds
    for page in ctx.indexable_html_pages():
        rec = _rec(page)
        if _body_unavailable(rec):
            continue
        # The Outlinks column counts internal links only; External Outlinks is
        # a separate count, not a subset. Subtracting one from the other made
        # any page with more external than internal links read as having no
        # internal links at all.
        outlinks = rec.get("outlinks")
        external = rec.get("external_outlinks")
        if outlinks is not None:
            if outlinks <= 0:
                ctx.add(
                    "NO_INTERNAL_OUTLINKS",
                    target_url=page.url,
                    details={"outlinks": outlinks, "external_outlinks": external},
                )
            if outlinks > t["high_outlinks"]:
                ctx.add(
                    "HIGH_OUTLINKS",
                    target_url=page.url,
                    details={"outlinks": outlinks, "max": t["high_outlinks"]},
                )
        if external is not None and external > t["high_external_outlinks"]:
            ctx.add(
                "HIGH_EXTERNAL_OUTLINKS",
                target_url=page.url,
                details={"external_outlinks": external, "max": t["high_external_outlinks"]},
            )


# AMP pairing codes (#1020). Each reads the AMP target a desktop page declares with
# <link rel="amphtml">, and only the AMP pages the crawl actually captured.
_AMP_CODES = (
    "AMP_NON_200",
    "AMP_MISSING_CANONICAL",
    "AMP_MISSING_RETURN_LINK",
    "AMP_NON_INDEXABLE_CANONICAL",
    "AMP_INDEXABLE",
)


def check_amp_pairing(ctx: AuditContext) -> None:
    """AMP pairing checks (#1020): read each declared AMP target's own record.

    A target the crawl never captured is unavailable evidence, not a broken AMP
    page, so it is named in a skip rather than reported as a finding. Pages are
    grouped by normalized AMP URL: one AMP page reached from several desktop
    pages is reported once.
    """
    if not _has_column(ctx, "amphtml"):
        for code in _AMP_CODES:
            ctx.skip(code, "no amphtml Link Element column in Internal:All")
        return

    targets: dict[str, tuple[str, list[str]]] = {}
    for page in ctx.html_pages():
        amp = _rec(page).get("amphtml")
        if amp:
            targets.setdefault(norm_url(amp), (amp, []))[1].append(page.url)
    if not targets:
        for code in _AMP_CODES:
            ctx.skip(code, "no page declares an AMP target (rel=amphtml)")
        return

    found: set[str] = set()
    uncaptured = 0
    for key, (amp_url, desktop_urls) in targets.items():
        rows = ctx.pages_by_norm.get(key) or []
        if not rows:
            uncaptured += 1
            continue
        omitted = max(0, len(desktop_urls) - 20)
        base = {
            "amphtml": amp_url,
            "desktop_urls": desktop_urls[:20],
            "desktop_urls_omitted": omitted,
        }

        codes = [row.status_code for row in rows]
        if any(code is not None and int(code) != 200 for code in codes):
            ctx.add(
                "AMP_NON_200",
                target_url=amp_url,
                status_code=codes[0] if len(set(codes)) == 1 else None,
                details={**base, "status_codes": sorted({int(c) for c in codes if c is not None})},
            )
            found.add("AMP_NON_200")

        for row in rows:
            if row.status_code is None or int(row.status_code) != 200:
                continue
            rec = _rec(row)
            if _body_unavailable(rec):
                continue  # unparsed body: no canonical evidence either way
            canonical = rec.get("canonical")
            if not canonical:
                ctx.add("AMP_MISSING_CANONICAL", target_url=amp_url, details=dict(base))
                found.add("AMP_MISSING_CANONICAL")
            elif not any(norm_url(canonical) == norm_url(d) for d in desktop_urls):
                ctx.add(
                    "AMP_MISSING_RETURN_LINK",
                    target_url=amp_url,
                    details={**base, "canonical": canonical},
                )
                found.add("AMP_MISSING_RETURN_LINK")
            if canonical and norm_url(canonical) != norm_url(row.url):
                canonical_targets = ctx.pages_by_norm.get(norm_url(canonical)) or []
                if canonical_targets and not any(t.is_indexable for t in canonical_targets):
                    ctx.add(
                        "AMP_NON_INDEXABLE_CANONICAL",
                        target_url=amp_url,
                        details={**base, "canonical": canonical},
                    )
                    found.add("AMP_NON_INDEXABLE_CANONICAL")
            if row.is_indexable:
                ctx.add("AMP_INDEXABLE", target_url=amp_url, details=dict(base))
                found.add("AMP_INDEXABLE")

    if uncaptured:
        for code in _AMP_CODES:
            if code not in found:
                ctx.skip(
                    code,
                    f"{uncaptured} AMP target(s) were declared but not captured by the crawl",
                )


def check_tech_extra(ctx: AuditContext) -> None:
    has_http_version = _has_column(ctx, "http_version")
    has_amphtml = _has_column(ctx, "amphtml")
    for page in ctx.html_pages():
        rec = _rec(page)
        # SF emits HTTP Version as "HTTP/1.1", "HTTP/2" or bare "1.1"/"2"; str() because
        # an all-numeric column parses as float.
        hv = str(rec.get("http_version") or "").upper().replace("HTTP/", "").strip()
        if has_http_version and hv.startswith("1"):
            ctx.add(
                "HTTP1_ONLY", target_url=page.url, details={"http_version": rec.get("http_version")}
            )
        if has_amphtml and rec.get("amphtml"):
            ctx.add("AMPHTML_PRESENT", target_url=page.url, details={"amphtml": rec.get("amphtml")})
    if not has_http_version:
        ctx.skip("HTTP1_ONLY", "no HTTP Version column in Internal:All")
    if not has_amphtml:
        ctx.skip("AMPHTML_PRESENT", "no amphtml Link Element column in Internal:All")


# --------------------------------------------------------------------------
# Static Lighthouse audits (issue #59) — see seohead/sf/core/lighthouse.py for
# which audit id each check corresponds to and its documentation link. None
# of these run a browser or a performance trace, and none of them contributes
# to, or can be mistaken for, a Lighthouse Performance score: each is a
# from-the-description reimplementation of one narrow, documented rule
# against the response and markup a crawl already has.
#
# None of the four fields these checks read (Content-Encoding, Doctype,
# Viewport, Meta Charset) is a default Screaming Frog export column; a native
# seohead crawl (seohead.crawl.evidence) always populates them, an SF export
# only does with matching Custom Extraction configured. Each check follows
# check_og's honesty contract: if no page in the run carries the evidence at
# all, it skips by name rather than flag every page.
# --------------------------------------------------------------------------

_CHARSET_IN_HEADER_RE = re.compile(r"charset\s*=", re.IGNORECASE)


def check_charset(ctx: AuditContext) -> None:
    """Lighthouse `charset`: an encoding declared via Content-Type or an early <meta> tag."""
    pages = ctx.html_pages()
    # Content-Type is a base column every crawl already carries and is read for its own
    # sake below; Meta Charset is the one column gated here, present only via a native
    # crawl or Custom Extraction in SF. Gating on its presence rather than on any page
    # happening to carry a truthy value means an all-negative corpus (no header charset,
    # no meta charset anywhere) is a real finding, not missing evidence (#268) — while an
    # export that never carries the column at all still honestly skips.
    if not _has_column(ctx, "meta_charset"):
        # Without this column, absence is unmeasurable rather than absent. A page
        # whose Content-Type carries no charset may still declare <meta charset>
        # in its HTML, which an export without the column does not show, so
        # firing on the header alone reports a defect nobody measured. Gating on
        # any page *happening* to carry a header charset -- which is what this
        # check did before #396 -- decided that question with evidence that
        # cannot answer it.
        #
        # The reason says which column is missing, not that Content-Type carries
        # no charset: on a run where some pages do carry one, that sentence was
        # simply untrue, and a skip reason a reader cannot trust is worth less
        # than no reason at all.
        ctx.skip(
            "MISSING_CHARSET",
            "no Meta Charset column, so a page without a header charset cannot be "
            "distinguished from one declaring <meta charset> (needs a native seohead "
            "crawl or Custom Extraction in SF)",
        )
        return
    for page in pages:
        header_charset = bool(page.content_type and _CHARSET_IN_HEADER_RE.search(page.content_type))
        meta_charset = bool(_rec(page).get("meta_charset"))
        if not header_charset and not meta_charset and not _body_unavailable(_rec(page)):
            ctx.add(
                "MISSING_CHARSET", target_url=page.url, details={"content_type": page.content_type}
            )


_DOCTYPE_NAME_RE = re.compile(r"<!DOCTYPE\s+([a-zA-Z0-9]+)", re.IGNORECASE)


def check_doctype(ctx: AuditContext) -> None:
    """Lighthouse `doctype`: exactly ``<!DOCTYPE html>``, no PUBLIC/SYSTEM identifier."""
    pages = ctx.html_pages()
    if not _has_column(ctx, "doctype"):
        ctx.skip(
            "MISSING_DOCTYPE",
            "no Doctype column (needs a native seohead crawl or Custom Extraction in SF)",
        )
        return
    for page in pages:
        if _body_unavailable(_rec(page)):
            continue
        raw = _rec(page).get("doctype")
        if not raw:
            ctx.add("MISSING_DOCTYPE", target_url=page.url, details={"reason": "no doctype"})
            continue
        name_match = _DOCTYPE_NAME_RE.match(raw)
        name = name_match.group(1).lower() if name_match else ""
        has_legacy_identifier = "public" in raw.lower() or "system" in raw.lower()
        if name != "html" or has_legacy_identifier:
            ctx.add("MISSING_DOCTYPE", target_url=page.url, details={"doctype": raw})


_INITIAL_SCALE_RE = re.compile(r"initial-scale\s*=\s*([0-9.]+)", re.IGNORECASE)
_VIEWPORT_WIDTH_RE = re.compile(r"\bwidth\s*=", re.IGNORECASE)


def check_mobile_alternate(ctx: AuditContext) -> None:
    """MOBILE_ALTERNATE_LINK: a media-qualified rel=alternate that names no URL (#1016)."""
    if not _has_column(ctx, "mobile_alternate_broken"):
        ctx.skip("MOBILE_ALTERNATE_LINK", "no rel=alternate media evidence (native crawl only)")
        return
    for page in ctx.html_pages():
        rec = _rec(page)
        if _body_unavailable(rec):
            continue
        count = rec.get("mobile_alternate_broken") or 0
        if count > 0:
            ctx.add(
                "MOBILE_ALTERNATE_LINK",
                target_url=page.url,
                details={"occurrences": count, "reason": "rel=alternate with media has no href"},
            )


def check_viewport(ctx: AuditContext) -> None:
    """Lighthouse `viewport` (now `viewport-insight`): see lighthouse.LIGHTHOUSE_MAP."""
    pages = ctx.html_pages()
    if not _has_column(ctx, "viewport"):
        ctx.skip(
            "VIEWPORT_MISSING",
            "no Viewport column (needs a native seohead crawl or Custom Extraction in SF)",
        )
        return
    for page in pages:
        if _body_unavailable(_rec(page)):
            continue
        content = _rec(page).get("viewport")
        if not content:
            ctx.add(
                "VIEWPORT_MISSING", target_url=page.url, details={"reason": "no viewport meta tag"}
            )
            continue
        scale_match = _INITIAL_SCALE_RE.search(content)
        if scale_match and float(scale_match.group(1)) < 1:
            ctx.add(
                "VIEWPORT_MISSING",
                target_url=page.url,
                details={"viewport": content, "reason": "initial-scale below 1"},
            )
        elif not scale_match and not _VIEWPORT_WIDTH_RE.search(content):
            ctx.add(
                "VIEWPORT_MISSING",
                target_url=page.url,
                details={"viewport": content, "reason": "no width or initial-scale"},
            )


# Lighthouse's own ignore threshold for `uses-text-compression`: below this
# many bytes, compressing the response saves too little to report.
_COMPRESSION_IGNORE_BYTES = 1400
_COMPRESSED_ENCODINGS = frozenset({"gzip", "br", "deflate", "zstd"})


def check_compression(ctx: AuditContext) -> None:
    """Lighthouse `uses-text-compression` (now `document-latency-insight`): see lighthouse.py."""
    pages = ctx.html_pages()
    if not _has_column(ctx, "content_encoding"):
        ctx.skip(
            "NO_COMPRESSION",
            "no Content-Encoding column (needs a native seohead crawl or Custom Extraction in SF)",
        )
        return
    for page in pages:
        rec = _rec(page)
        # Content-Encoding is comma-separated when codings are stacked (e.g. "gzip, br"
        # from a CDN re-compressing an already-gzipped origin response) — comparing the
        # whole header string against a single-token set flagged every stacked value as
        # uncompressed even though each listed token is a real compression coding (#269).
        tokens = [t.strip().lower() for t in str(rec.get("content_encoding") or "").split(",")]
        tokens = [t for t in tokens if t]
        if any(t in _COMPRESSED_ENCODINGS for t in tokens):
            continue
        # A missing Size (bytes) column is not "0 bytes, too small to matter" -- it is
        # unmeasured evidence, and applying the ignore threshold to it would let an
        # uncompressed page hide behind a size nobody actually observed (#445). Only
        # apply the threshold when size was genuinely measured.
        size = rec.get("size_bytes")
        if size is not None and size < _COMPRESSION_IGNORE_BYTES:
            continue
        ctx.add(
            "NO_COMPRESSION",
            target_url=page.url,
            details={"content_encoding": rec.get("content_encoding"), "size_bytes": size},
        )


def _check_retained_render_blocking(ctx: AuditContext) -> None:
    """Read frozen native scan bodies without adding fields to scan.v1."""
    from bs4 import BeautifulSoup

    from seohead.checks.asset_weight import find_render_blocking_resources
    from seohead.checks.parser import document_base_url
    from seohead.storage import ScanError
    from seohead.storage.bodies import read_document

    metrics = ctx.asset_measurements["RENDER_BLOCKING"]
    rows = ctx.scan_con.execute(
        "WITH active_static AS ("
        "SELECT d.url_id,MAX(d.document_id) AS document_id FROM documents d "
        "JOIN context_items c ON c.kind='resource_inventory' AND c.item_key='document:'||d.document_id "
        "WHERE d.representation='static' GROUP BY d.url_id) "
        "SELECT u.url,d.document_id,d.body_state,COALESCE(final.url,u.url),p.status_code "
        "FROM pages p JOIN urls u USING(url_id) "
        "LEFT JOIN active_static a ON a.url_id=p.url_id "
        "LEFT JOIN documents d ON d.document_id=CASE WHEN p.representation='static' "
        "THEN p.document_id ELSE a.document_id END AND d.representation='static' "
        "LEFT JOIN responses r ON r.response_id=d.source_response_id "
        "LEFT JOIN urls final ON final.url_id=r.effective_url_id "
        "WHERE lower(p.content_type) LIKE '%html%' AND p.status_code BETWEEN 200 AND 299 "
        "ORDER BY p.page_ordinal"
    )
    for url, document_id, state, final_url, status in rows:
        metrics["eligible"] += 1
        if document_id is None or state != "complete":
            metrics["unavailable"] += 1
            continue
        try:
            html = read_document(ctx.scan_con, document_id, max_decoded_bytes=2 * 1024 * 1024)
            soup = BeautifulSoup(html, features="lxml")
            resources = find_render_blocking_resources(soup, document_base_url(soup, final_url))
        except (ScanError, ValueError):
            metrics["unavailable"] += 1
            continue
        metrics["measured"] += 1
        if resources:
            sample = []
            sample_bytes = 2
            for item in resources:
                size = len(json.dumps(item["url"], ensure_ascii=True)) + (2 if sample else 0)
                if len(sample) < 200 and sample_bytes + size <= 64 * 1024:
                    sample.append(item["url"])
                    sample_bytes += size
            if len(sample) < len(resources):
                metrics["partial"] += 1
            ctx.add(
                "RENDER_BLOCKING",
                target_url=url,
                status_code=status,
                details={
                    "resources": sample,
                    "resources_observed": len(resources),
                    "resources_omitted": len(resources) - len(sample),
                    "source": "native-retained-static-head",
                    "source_page": url,
                    "reason": "retained source head declares blocking scripts or stylesheets; runtime blocking time was not measured",
                },
            )
    metrics["state"] = (
        "unavailable"
        if not metrics["measured"]
        else "partial"
        if metrics["unavailable"] or metrics["partial"]
        else "complete"
    )
    if metrics["state"] != "complete":
        metrics["reason"] = (
            "not fully measured by this native crawl: static HTML is absent, incomplete or "
            "exceeds the 2 MiB per-document head-analysis budget, or resource samples were bounded"
        )
        ctx.skip(
            "RENDER_BLOCKING",
            metrics["reason"],
        )


def check_render_blocking(ctx: AuditContext) -> None:
    """Render Blocking Requests (SF PageSpeed): <head> scripts/stylesheets that hold first paint.

    Only a native seohead crawl measures this (#1022); an SF export without the column
    skips honestly rather than reporting every page as clean.
    """
    if not hasattr(ctx, "asset_measurements"):
        ctx.asset_measurements = {}
    metrics = ctx.asset_measurements["RENDER_BLOCKING"] = {
        "unit": "pages",
        "eligible": 0,
        "measured": 0,
        "partial": 0,
        "unavailable": 0,
        "state": "unavailable",
        "reason": "",
    }
    if ctx.scan_con is not None:
        _check_retained_render_blocking(ctx)
        return
    if not _has_column(ctx, "render_blocking"):
        metrics["eligible"] = metrics["unavailable"] = len(ctx.html_pages())
        metrics["reason"] = "no Render Blocking Resources column (needs a native seohead crawl)"
        ctx.skip(
            "RENDER_BLOCKING",
            metrics["reason"],
        )
        return
    for page in ctx.html_pages():
        metrics["eligible"] += 1
        rec = _rec(page)
        value = rec.get("render_blocking")
        if _body_unavailable(rec) or value is None:
            metrics["unavailable"] += 1
            continue
        try:
            payload = (
                json.loads(value) if str(value).startswith(("[", "{")) else str(value).split("\n")
            )
            if isinstance(payload, dict):
                urls, observed, omitted = (
                    payload["resources"],
                    payload["observed"],
                    payload["omitted"],
                )
                if (
                    type(observed) is not int
                    or type(omitted) is not int
                    or omitted < 0
                    or observed != len(urls) + omitted
                ):
                    raise ValueError("invalid retained resource counts")
            else:
                urls = payload
                observed, omitted = len(urls), 0
            if not isinstance(urls, list) or any(type(url) is not str for url in urls):
                raise ValueError("invalid retained resource URLs")
        except (ValueError, TypeError, KeyError):
            metrics["unavailable"] += 1
            continue
        metrics["measured"] += 1
        sample = []
        sample_bytes = 2
        for url in urls:
            size = len(json.dumps(url, ensure_ascii=True)) + (2 if sample else 0)
            if len(sample) < 200 and sample_bytes + size <= 64 * 1024:
                sample.append(url)
                sample_bytes += size
        omitted += len(urls) - len(sample)
        if omitted:
            metrics["partial"] += 1
        if observed:
            ctx.add(
                "RENDER_BLOCKING",
                target_url=page.url,
                status_code=page.status_code,
                details={
                    "resources": sample,
                    "resources_omitted": omitted,
                    "resources_observed": observed,
                    "source": "native-head-markup",
                    "source_page": page.url,
                    "reason": "head markup declares render-blocking scripts or stylesheets; runtime blocking time was not measured",
                },
            )
    metrics["state"] = (
        "unavailable"
        if not metrics["measured"]
        else "partial"
        if metrics["unavailable"] or metrics["partial"]
        else "complete"
    )
    if metrics["state"] != "complete":
        metrics["reason"] = (
            "retained render-blocking markup is absent, unreadable or resource samples were bounded"
        )
        ctx.skip("RENDER_BLOCKING", metrics["reason"])


# --------------------------------------------------------------------------
# Element position & document skeleton (issue #123)
#
# A browser closes <head> at the first element that does not belong there, and
# everything after that point is read from <body> instead — a canonical or a
# robots directive placed there silently stops applying, while the source text
# still looks fine. Screaming Frog has no notion of this at all: the signal
# exists only where seohead.checks.parser.parse_html resolved the tree (see its
# module docstring for what was verified against lxml directly), so — like the
# static Lighthouse audits just above — these need a native seohead crawl.
# --------------------------------------------------------------------------

_ELEMENT_POSITION_CHECKS: dict[str, str] = {
    "title_outside_head": "TITLE_OUTSIDE_HEAD",
    "meta_description_outside_head": "DESC_OUTSIDE_HEAD",
    "canonical_outside_head": "CANONICAL_OUTSIDE_HEAD",
    "directives_outside_head": "DIRECTIVES_OUTSIDE_HEAD",
    "hreflang_outside_head": "HREFLANG_OUTSIDE_HEAD",
}
# Title/description ask about the page's own indexable content, matching
# check_titles/check_descriptions; canonical/directives/hreflang matter on any
# HTML page, matching check_canonical_directives.
_ELEMENT_POSITION_ON_INDEXABLE_ONLY = frozenset(
    {"title_outside_head", "meta_description_outside_head"}
)

_SKELETON_CHECKS = (
    "HEAD_MISSING",
    "HEAD_MULTIPLE",
    "BODY_MISSING",
    "BODY_MULTIPLE",
    "INVALID_HEAD_ELEMENT",
    "HEAD_NOT_FIRST",
)
_SKELETON_FIELDS = ("head_count", "body_count", "head_not_first", "invalid_head_elements")

_NO_POSITION_EVIDENCE = (
    "no element-position evidence (needs a native seohead crawl; Screaming Frog has no "
    "notion of this on its own)"
)


def check_element_position(ctx: AuditContext) -> None:
    """Outside-<head> checks for title, description, canonical, directives, and hreflang."""
    from .normalize import INTERNAL_FIELD_MAP, find_column

    for field, check_id in _ELEMENT_POSITION_CHECKS.items():
        if (
            ctx.internal_df is None
            or find_column(ctx.internal_df, INTERNAL_FIELD_MAP[field]) is None
        ):
            ctx.skip(check_id, _NO_POSITION_EVIDENCE)
            continue
        pages = (
            ctx.indexable_html_pages()
            if field in _ELEMENT_POSITION_ON_INDEXABLE_ONLY
            else ctx.html_pages()
        )
        for page in pages:
            if _rec(page).get(field):
                ctx.add(check_id, target_url=page.url)


def check_document_skeleton(ctx: AuditContext) -> None:
    """Document-skeleton validity: <head>/<body> presence, count, and order.

    One finding per page, never one per stray element — a page with two
    <body> tags is a single BODY_MULTIPLE, not one per tag.
    """
    from .normalize import INTERNAL_FIELD_MAP, find_column

    has_evidence = ctx.internal_df is not None and all(
        find_column(ctx.internal_df, INTERNAL_FIELD_MAP[field]) is not None
        for field in _SKELETON_FIELDS
    )
    if not has_evidence:
        for check_id in _SKELETON_CHECKS:
            ctx.skip(check_id, _NO_POSITION_EVIDENCE)
        return
    for page in ctx.html_pages():
        rec = _rec(page)
        if _body_unavailable(rec):
            continue
        head_count = rec.get("head_count") or 0
        body_count = rec.get("body_count") or 0
        if head_count == 0:
            ctx.add("HEAD_MISSING", target_url=page.url)
        elif head_count > 1:
            ctx.add("HEAD_MULTIPLE", target_url=page.url, details={"head_count": head_count})
        if body_count == 0:
            ctx.add("BODY_MISSING", target_url=page.url)
        elif body_count > 1:
            ctx.add("BODY_MULTIPLE", target_url=page.url, details={"body_count": body_count})
        invalid = str(rec.get("invalid_head_elements") or "")
        if invalid:
            elements = [e.strip() for e in invalid.split(",") if e.strip()]
            ctx.add("INVALID_HEAD_ELEMENT", target_url=page.url, details={"elements": elements})
        if rec.get("head_not_first"):
            ctx.add("HEAD_NOT_FIRST", target_url=page.url)


def check_html_structure(ctx: AuditContext) -> None:
    """DUPLICATE_ID and DECLARED_MIME_MISMATCH from bounded observed facts (#828).

    Repeated ids need the parsed DOM, so an export without the native column
    is explicitly skipped. MIME is deliberately narrower: it only compares
    unambiguous filename extensions with a received Content-Type family and
    names absent/malformed headers as unmeasured instead of guessing.
    """
    has_duplicate_ids = _has_column(ctx, "duplicate_ids")
    if not has_duplicate_ids:
        ctx.skip("DUPLICATE_ID", "no duplicate-id evidence (native crawl only)")
    unmeasured_ids = 0
    if has_duplicate_ids:
        for page in ctx.html_pages():
            rec = _rec(page)
            values = rec.get("duplicate_ids")
            if _body_unavailable(rec) or not isinstance(values, list):
                unmeasured_ids += 1
                continue
            for item in values:
                if not isinstance(item, dict) or type(item.get("count")) is not int:
                    continue
                identifier = item.get("id")
                if isinstance(identifier, str) and item["count"] > 1:
                    ctx.add(
                        "DUPLICATE_ID",
                        target_url=page.url,
                        occurrences_count=item["count"],
                        details={"id": identifier, "count": item["count"]},
                    )
    if unmeasured_ids:
        ctx.skip(
            "DUPLICATE_ID",
            f"{unmeasured_ids} page(s) have no parsed duplicate-id evidence",
        )

    unmeasured_mime = 0
    observed_mime = 0
    for page in ctx.pages:
        path = urllib.parse.urlsplit(page.url).path
        expected, _ = mimetypes.guess_type(path)
        if not expected:
            continue
        content_type = _rec(page).get("content_type")
        if not isinstance(content_type, str) or not content_type.strip():
            unmeasured_mime += 1
            continue
        declared = content_type.split(";", 1)[0].strip().lower()
        if "/" not in declared:
            unmeasured_mime += 1
            continue
        observed_mime += 1
        if declared != expected.lower():
            ctx.add(
                "DECLARED_MIME_MISMATCH",
                target_url=page.url,
                details={"declared_content_type": declared, "extension_content_type": expected},
            )
    if not observed_mime:
        ctx.skip("DECLARED_MIME_MISMATCH", "no URL with a recognized extension and Content-Type")
    elif unmeasured_mime:
        ctx.skip(
            "DECLARED_MIME_MISMATCH",
            f"{unmeasured_mime} recognized-extension URL(s) have no usable Content-Type",
        )


def check_url_hygiene(ctx: AuditContext) -> None:
    """Known session parameters and observed slash pairs, without URL-policy guesses (#831)."""
    session_observed = 0
    indexable_pages = 0
    for page in ctx.indexable_html_pages():
        indexable_pages += 1
        names = sorted(set(_session_params(page.url)), key=str.lower)
        if not names:
            continue
        session_observed += 1
        ctx.add(
            "URL_SESSION_ID",
            target_url=_redact_query_values(page.url),
            details={"parameter_names": names, "url_values_redacted": True},
        )
    if not session_observed and not indexable_pages:
        ctx.skip("URL_SESSION_ID", "no indexable HTML pages to inspect for session parameters")

    # This check only needs five scalar facts.  Keeping ``Page`` objects in
    # groups makes a large legacy JSONL audit retain every parsed record until
    # the final slash pair is inspected (#817).
    pairs: dict[
        tuple[str, str, str, str],
        list[tuple[str, int | None, bool, Any, str | None]],
    ] = defaultdict(list)
    for page in ctx.pages:
        key = _slash_key(page.url)
        if key is not None:
            record = _rec(page)
            canonical = record.get("canonical")
            redirect_url = record.get("redirect_url")
            pairs[key].append(
                (
                    page.url,
                    page.status_code,
                    page.is_indexable,
                    canonical,
                    redirect_url if isinstance(redirect_url, str) else None,
                )
            )
    for group in pairs.values():
        slash = [
            variant for variant in group if urllib.parse.urlsplit(variant[0]).path.endswith("/")
        ]
        plain = [
            variant for variant in group if not urllib.parse.urlsplit(variant[0]).path.endswith("/")
        ]
        if not slash or not plain:
            continue
        observed = slash + plain
        urls = {url for url, _status, _indexable, _canonical, _redirect in observed}
        converged = any(
            status is not None and 300 <= int(status) <= 399 and redirect_url in urls
            for _url, status, _indexable, _canonical, redirect_url in observed
        ) or any(
            isinstance(canonical, str) and canonical in urls and canonical != url
            for url, _status, _indexable, canonical, _redirect_url in observed
        )
        if converged or not all(
            indexable and status == 200
            for _url, status, indexable, _canonical, _redirect_url in observed
        ):
            continue
        ctx.add(
            "URL_TRAILING_SLASH_INCONSISTENT",
            target_url=sorted(urls)[0],
            occurrences_count=len(urls),
            details={
                "observed_variants": sorted(urls),
                "status_codes": {
                    url: status for url, status, _indexable, _canonical, _redirect in observed
                },
                "canonical": {
                    url: canonical for url, _status, _indexable, canonical, _redirect in observed
                },
            },
        )


def check_placeholder_markers(ctx: AuditContext) -> None:
    """Literal non-Lorem template markers in existing page declarations (#828)."""
    for page in ctx.html_pages():
        rec = _rec(page)
        if _body_unavailable(rec):
            continue
        observed = []
        for field in ("title", "meta_description", "h1", "h1_2", "h2"):
            value = rec.get(field)
            if not isinstance(value, str):
                continue
            markers = sorted(set(PLACEHOLDER_MARKER_RE.findall(value)), key=str.lower)
            if markers:
                observed.append({"field": field, "markers": markers, "value": value[:240]})
        if observed:
            ctx.add("PLACEHOLDER_MARKER", target_url=page.url, details={"observed": observed})


def check_native_page_evidence(ctx: AuditContext) -> None:
    """LOREM_IPSUM_PLACEHOLDER, UNSUPPORTED_PLUGIN, IMG_MISSING_ALT_ATTRIBUTE, IMG_ALT_TOO_LONG.

    Four independent facts a native crawl's own HTML parse already records per page (#385,
    #386), none of which a Screaming Frog export carries by default -- each guards itself
    honestly instead of reading a run that never measured it as clean. Grouped in one
    function because they share nothing but that shape, not because they are related in
    meaning; see seohead.checks.parser for how each is extracted.
    """
    t = ctx.thresholds
    alt_max_chars = t.get("alt_max_chars", 100)
    has_lorem = _has_column(ctx, "lorem_ipsum_count")
    has_plugin = _has_column(ctx, "plugin_elements")
    has_images = _has_column(ctx, "images_total")
    for page in ctx.html_pages():
        rec = _rec(page)
        if _body_unavailable(rec):
            continue
        if has_lorem:
            occurrences = rec.get("lorem_ipsum_count") or 0
            if occurrences > 0:
                ctx.add(
                    "LOREM_IPSUM_PLACEHOLDER",
                    target_url=page.url,
                    details={"occurrences": occurrences},
                )
        if has_plugin:
            count = rec.get("plugin_elements") or 0
            if count > 0:
                ctx.add("UNSUPPORTED_PLUGIN", target_url=page.url, details={"count": count})
        if has_images:
            missing_alt_attr = rec.get("images_missing_alt_attr") or 0
            if missing_alt_attr > 0:
                ctx.add(
                    "IMG_MISSING_ALT_ATTRIBUTE",
                    target_url=page.url,
                    details={
                        "images_missing_alt_attribute": missing_alt_attr,
                        "images_total": rec.get("images_total"),
                    },
                )
            max_alt_length = rec.get("images_max_alt_length") or 0
            if max_alt_length > alt_max_chars:
                ctx.add(
                    "IMG_ALT_TOO_LONG",
                    target_url=page.url,
                    details={"max_alt_length": max_alt_length, "max_chars": alt_max_chars},
                )
    if not has_lorem:
        ctx.skip("LOREM_IPSUM_PLACEHOLDER", "no Lorem Ipsum evidence (native crawl only)")
    if not has_plugin:
        ctx.skip("UNSUPPORTED_PLUGIN", "no plugin-element evidence (native crawl only)")
    if not has_images:
        ctx.skip("IMG_MISSING_ALT_ATTRIBUTE", "no per-image evidence (native crawl only)")
        ctx.skip("IMG_ALT_TOO_LONG", "no per-image evidence (native crawl only)")


def check_ajax_crawling_scheme(ctx: AuditContext) -> None:
    """AJAX_CRAWLING_SCHEME_URL, AJAX_CRAWLING_SCHEME_META_FRAGMENT (#386).

    Google's AJAX crawling scheme -- a ``#!`` hash-bang URL, or a page-wide
    ``<meta name="fragment" content="!">``, each promising a rendered snapshot at an
    ``?_escaped_fragment_=`` companion URL -- was deprecated in 2015 and switched off
    in 2018. ``seohead.checks.render`` already reads both shapes, but only to decide
    how to fetch a page; neither was recorded, so nothing could report that a site
    still carries them.

    Registered at *notice*, deliberately: the scheme is inert rather than broken, and
    a site may still serve it for a legacy client of its own, so a warning would be
    telling an operator off for a decision that may well be theirs. Both checks read
    evidence only a native crawl records and skip by name on a Screaming Frog export,
    which carries neither column.
    """
    has_outlinks = _has_column(ctx, "ajax_scheme_outlinks")
    has_meta = _has_column(ctx, "meta_fragment")
    for page in ctx.html_pages():
        rec = _rec(page)
        if has_outlinks:
            # The page's own address counts as well as the URLs it publishes: a
            # crawl seeded with an _escaped_fragment_ URL, or one that followed a
            # hash-bang link, holds a page that *is* a scheme URL and would
            # otherwise be reported only against whatever happened to link it.
            declared = rec.get("ajax_scheme_outlinks") or 0
            self_uses = uses_ajax_crawling_scheme(page.url)
            if declared or self_uses:
                ctx.add(
                    "AJAX_CRAWLING_SCHEME_URL",
                    target_url=page.url,
                    details={"own_url_uses_scheme": self_uses, "outlinks_using_scheme": declared},
                )
        if has_meta:
            fragment = str(rec.get("meta_fragment") or "").strip()
            if fragment == "!":
                ctx.add(
                    "AJAX_CRAWLING_SCHEME_META_FRAGMENT",
                    target_url=page.url,
                    details={"meta_fragment": fragment},
                )
    if not has_outlinks:
        ctx.skip("AJAX_CRAWLING_SCHEME_URL", "no AJAX-scheme URL evidence (native crawl only)")
    if not has_meta:
        ctx.skip(
            "AJAX_CRAWLING_SCHEME_META_FRAGMENT",
            'no <meta name="fragment"> evidence (native crawl only)',
        )


def check_og(ctx: AuditContext) -> None:
    """Check Open Graph presence.

    Fires on indexable HTML pages missing ``og:title`` — the one tag without
    which a social preview cannot assemble. Honesty contract: if the export
    carries no OG columns at all (user didn't enable OG extraction in SF), the
    check skips rather than flag every page.
    """
    pages = ctx.indexable_html_pages()
    og_fields = ("og_title", "og_description", "og_image", "og_url")
    has_og = any(_rec(p).get(k) for p in pages for k in og_fields)
    if not has_og:
        ctx.skip("OG_MISSING", "no Open Graph columns in Internal:All (enable OG extraction in SF)")
        return
    for page in pages:
        rec = _rec(page)
        if _body_unavailable(rec):
            continue
        if rec.get("og_title"):
            continue
        missing = [
            f.replace("_", ":") for f in ("og_title", "og_image", "og_url") if not rec.get(f)
        ]
        ctx.add("OG_MISSING", target_url=page.url, details={"missing_tags": missing})


def check_native_image_resources(ctx: AuditContext) -> None:
    """IMG_BROKEN: an <img> whose fetched target answered 4xx/5xx.

    Reads the native crawl's resource graph (resource_graph_occurrences joined to
    resource_graph_fetches) from the stored scan. Streams rows through the cursor,
    so memory stays bounded. Skips honestly when no image was measured: a crawl
    without resource capture must not read as "no broken images".
    """
    no_evidence = "no resource evidence (native crawl with resource capture only)"
    con = ctx.scan_con
    if con is None:
        ctx.skip("IMG_BROKEN", no_evidence)
        return
    tables = {row[0] for row in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    if not {"resource_graph_occurrences", "resource_graph_fetches"} <= tables:
        ctx.skip("IMG_BROKEN", no_evidence)
        return
    measured = con.execute(
        "SELECT 1 FROM resource_graph_occurrences o JOIN resource_graph_fetches f "
        "ON f.resolved_url=o.resolved_url WHERE o.kind='image' LIMIT 1"
    ).fetchone()
    if measured is None:
        ctx.skip("IMG_BROKEN", no_evidence)
        return
    rows = con.execute(
        "SELECT p.url, o.resolved_url, f.status_code FROM resource_graph_occurrences o "
        "JOIN resource_graph_fetches f ON f.resolved_url=o.resolved_url "
        "JOIN urls p ON p.url_id=o.page_url_id "
        "WHERE o.kind='image' AND f.status_code>=400 ORDER BY p.url, o.resolved_url"
    )
    for page_url, image_url, status in rows:
        ctx.add(
            "IMG_BROKEN",
            target_url=image_url,
            details={"source_page": page_url, "status_code": status, "reason": f"HTTP {status}"},
        )


def check_native_image_size(ctx: AuditContext) -> None:
    """IMG_OVER_KB: an image whose fetched body is over ``thresholds.img_max_kb``.

    Read from the native crawl's own image fetches (resource_graph_fetches.bytes_received),
    one finding per image file. A fetch that did not complete (budget, excluded or failed)
    leaves that image's size unmeasured, so it is never read as small; when no image body
    was measured at all, the check skips in native terms.
    """
    if ctx.scan_con is None:
        return
    con = ctx.scan_con
    tables = {row[0] for row in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    if not {"resource_graph_occurrences", "resource_graph_fetches"} <= tables:
        ctx.skip("IMG_OVER_KB", _NATIVE_NO_RESOURCES)
        return
    measured = con.execute(
        "SELECT 1 FROM resource_graph_occurrences o JOIN resource_graph_fetches f "
        "ON f.resolved_url=o.resolved_url WHERE o.kind='image' "
        "AND f.state IN ('complete','partial') LIMIT 1"
    ).fetchone()
    if measured is None:
        ctx.skip("IMG_OVER_KB", "not measured by this native crawl: no image body was fetched")
        return
    limit_kb = ctx.thresholds.get("img_max_kb", 150)
    rows = con.execute(
        "SELECT o.resolved_url, MIN(p.url), COUNT(DISTINCT o.page_url_id), f.bytes_received "
        "FROM resource_graph_occurrences o "
        "JOIN resource_graph_fetches f ON f.resolved_url=o.resolved_url "
        "JOIN urls p ON p.url_id=o.page_url_id "
        "WHERE o.kind='image' AND f.state IN ('complete','partial') "
        "AND f.bytes_received > ? GROUP BY o.resolved_url ORDER BY o.resolved_url",
        (int(limit_kb * 1024),),
    )
    for image_url, source_page, pages, size in rows:
        ctx.add(
            "IMG_OVER_KB",
            target_url=image_url,
            details={
                "bytes": size,
                "limit_kb": limit_kb,
                "source_page": source_page,
                "pages": pages,
            },
        )


def check_native_export_aggregates(ctx: AuditContext) -> None:
    """IMG_MISSING_ALT, IMG_MISSING_DIMENSIONS and HREFLANG_ERROR on a native crawl.

    None of the three is an export-only fact here, and none is measured as the
    export defines it, so each says so in native terms rather than pointing at an SF
    filter. Alt text is stored as a per-page count of missing alt attributes
    (IMG_MISSING_ALT_ATTRIBUTE), not per image with empty values. Image pixel size is
    stored, but not whether each <img> declares width and height. HREFLANG_ERROR is an
    umbrella over the per-kind hreflang checks, which the native crawl runs itself.
    """
    if ctx.scan_con is None:
        return
    ctx.skip(
        "IMG_MISSING_ALT",
        "not measured by this native crawl: alt text is recorded as a per-page count of "
        "missing alt attributes (IMG_MISSING_ALT_ATTRIBUTE); empty alt values are not stored per image",
    )
    ctx.skip(
        "IMG_MISSING_DIMENSIONS",
        "not measured by this native crawl: the stored scan keeps each image's pixel size, "
        "not whether the <img> tag declares width and height",
    )
    ctx.skip(
        "HREFLANG_ERROR",
        "not measured by this native crawl as one aggregate; the native hreflang checks "
        "report each error kind on its own",
    )


_NATIVE_NO_RESOURCES = "no resource evidence (native crawl with resource capture only)"

# Checks a native crawl resolves from its own stored data, whether they fire, come out clean,
# or say honestly that the crawl could not measure them. Absence of an SF export says nothing
# about them, so neither skip_unsupported nor the export fallback declares them missing.
NATIVE_RESOLVED_CHECKS = (
    "IMG_OVER_KB",
    "IMG_MISSING_ALT",
    "IMG_MISSING_DIMENSIONS",
    "HREFLANG_ERROR",
    "SITEMAP_URL_4XX_5XX",
    "SITEMAP_URL_3XX",
    "SITEMAP_URL_NON_INDEXABLE",
)


# Native-filter exports: emit one issue per Address when the export is present,
# else honestly skip (no dead zeros). export key -> check id.
_NATIVE_EXPORT_CHECKS = {
    "security_mixed": "MIXED_CONTENT",
    "security_hsts": "MISSING_HSTS",
    "structured_data_missing": "STRUCTURED_DATA_MISSING",
    "images_over_kb": "IMG_OVER_KB",
    "images_missing_size": "IMG_MISSING_DIMENSIONS",
    "images_missing_alt": "IMG_MISSING_ALT",
    "titles_multiple": "TITLE_MULTIPLE",
    "hreflang": "HREFLANG_ERROR",
}


_INBOUND_LINK_CAP = 20


def _inbound_internal_links(ctx: AuditContext) -> dict[str, list[dict[str, str]]] | None:
    """Internal hyperlinks grouped by normalized destination, or None without an inlink inventory."""
    from .inlinks import _all_inlink_records, _internal_hyperlink_records, _site_host
    from .normalize import normalize_value

    records = _all_inlink_records(ctx)
    if records is None:
        return None
    host = _site_host(ctx)
    index: dict[str, list[dict[str, str]]] = {}
    for rec in _internal_hyperlink_records(records, host):
        # An internal link is one written on this site: the source must be on the host too.
        if urllib.parse.urlparse(str(rec["source_url"])).netloc.lower() != host:
            continue
        index.setdefault(norm_url(rec["destination_url"]), []).append(
            {
                "source_url": str(rec["source_url"]),
                "anchor": normalize_value(rec.get("anchor")) or "",
            }
        )
    return index


def _inbound_detail(inbound: dict[str, list[dict[str, str]]] | None, start: str) -> dict[str, Any]:
    """The inbound-link evidence for one chain start, or an honest 'not measured' marker."""
    if inbound is None:
        return {"inbound_links": None, "inbound_links_reason": "no all_inlinks export"}
    links = inbound.get(norm_url(start), [])
    return {
        "inbound_links": links[:_INBOUND_LINK_CAP],
        "inbound_links_count": len(links),
    }


def check_redirect_chains(ctx: AuditContext) -> None:
    """Resolve redirect chains and loops.

    Prefers the native Redirects:Redirect Chains report (full-profile export)
    when present. Otherwise it walks ``ctx.redirect_map`` itself — the
    per-URL Location header every input mode already collects into
    Internal:All — so a light-profile export or a native seohead crawl gets
    the same findings without a second crawl. Only when Internal:All carries
    no redirect data at all is the pair skipped by name.
    """
    from .normalize import find_column, normalize_value, to_int

    df = ctx.exports.get("redirect_chains")
    if df is None or df.empty:
        if (
            ctx.internal_df is None
            or find_column(ctx.internal_df, ["Redirect URL", "Redirect URI"]) is None
        ):
            ctx.skip("REDIRECT_CHAIN", "no redirect data (Internal:All has no Redirect URL column)")
            ctx.skip("REDIRECT_LOOP", "no redirect data (Internal:All has no Redirect URL column)")
            return
        from .redirect_chains import DEFAULT_HOP_CAP, redirect_hop_path, resolve_redirect_chains

        hop_cap = ctx.thresholds.get("redirect_hop_cap", DEFAULT_HOP_CAP)
        inbound = _inbound_internal_links(ctx)
        for start, outcome in resolve_redirect_chains(ctx.redirect_map, hop_cap).items():
            # The full walk, so a finding names each hop to replace, not just a count.
            path = redirect_hop_path(ctx.redirect_map, start, hop_cap)
            # The internal links that still point at the chain's first URL: the edits
            # that move users off the chain, not just the hop that is broken.
            links = _inbound_detail(inbound, start)
            if outcome["kind"] == "loop":
                ctx.add(
                    "REDIRECT_LOOP",
                    target_url=start,
                    details={
                        "hops": outcome["hops"],
                        "final_url": None,
                        "path": path,
                        **links,
                    },
                )
            elif outcome["kind"] == "chain":
                ctx.add(
                    "REDIRECT_CHAIN",
                    target_url=start,
                    details={
                        "hops": outcome["hops"],
                        "final_url": outcome["final_url"],
                        "path": path,
                        **links,
                    },
                )
            elif outcome["kind"] == "unresolved":
                # The walk hit hop_cap without proving either a clean terminus or a
                # loop back into its own chain -- a chain of at least hop_cap hops
                # that could not be fully classified. That is not "no chain here":
                # it must surface as evidence, not fall through silently (#447).
                ctx.add(
                    "REDIRECT_CHAIN",
                    target_url=start,
                    details={
                        "hops": outcome["hops"],
                        "final_url": outcome["final_url"],
                        "unresolved": True,
                        "path": path,
                        **links,
                    },
                )
        return
    addr = find_column(df, ["Address", "URL"])
    hops = find_column(
        df, ["Number of Redirects", "No. Of Redirects", "No. of Redirects", "Redirect Hops", "Hops"]
    )
    final = find_column(df, ["Final Address", "Final URL", "Final URI"])
    loop = find_column(df, ["Loop", "Redirect Loop", "Chain Loop"])
    if not addr:
        # A present-but-unusable native report shares nothing REDIRECT_LOOP could stand
        # on either — its verdict comes from the same per-row address the chain check
        # just found missing, not from a separate source. Skipping only REDIRECT_CHAIN
        # here left REDIRECT_LOOP unrecorded, which aggregate.py then classified as a
        # silent clean pass over a loop nobody evaluated (issue #350).
        reason = "Redirect Chains report has no address column"
        ctx.skip("REDIRECT_CHAIN", reason)
        ctx.skip("REDIRECT_LOOP", reason)
        return
    for _, row in df.iterrows():
        url = normalize_value(row.get(addr))
        if not url:
            continue
        n = to_int(row.get(hops)) if hops else None
        fin = normalize_value(row.get(final)) if final else None
        is_loop = loop and str(normalize_value(row.get(loop))).strip().lower() in (
            "true",
            "yes",
            "1",
        )
        if is_loop:
            ctx.add("REDIRECT_LOOP", target_url=url, details={"hops": n, "final_url": fin})
        elif n is not None and n >= 2:
            ctx.add("REDIRECT_CHAIN", target_url=url, details={"hops": n, "final_url": fin})


def native_check_ids(ctx: AuditContext) -> frozenset[str]:
    """Checks the stored native crawl measures itself, so they need no SF export.

    The parsed-block column exists only in a native crawl's own records, so
    STRUCTURED_DATA_MISSING needs no stored scan and a legacy and a stored-scan audit
    of one crawl agree. HSTS and mixed content read response headers and the resource
    graph, which only a stored scan keeps. An export that happens to exist is not
    consulted for these: a missing export never says the site lacks a header.
    """
    ids: set[str] = set()
    if _has_column(ctx, "structured_data_parsed"):
        ids.add("STRUCTURED_DATA_MISSING")
    con = ctx.scan_con
    if con is None:
        return frozenset(ids)
    ids.update(NATIVE_RESOLVED_CHECKS)
    tables = {row[0] for row in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    if "responses" in tables:
        ids.add("MISSING_HSTS")
    if (
        "resource_graph_occurrences" in tables
        and con.execute("SELECT 1 FROM resource_graph_occurrences LIMIT 1").fetchone()
    ):
        ids.add("MIXED_CONTENT")
    return frozenset(ids)


def check_native_structured_and_mixed(ctx: AuditContext) -> None:
    """STRUCTURED_DATA_MISSING and MIXED_CONTENT, measured from the native crawl.

    Structured data: an indexable HTML page whose stored block count is zero has no
    markup at all. Mixed content: an https page whose resource graph names an http
    subresource. MISSING_HSTS is judged from response headers by
    ``seohead.crawl.security_headers`` in the audit handler, not here.
    """
    native = native_check_ids(ctx)
    if "STRUCTURED_DATA_MISSING" in native:
        measured = 0
        for page in ctx.indexable_html_pages():
            rec = _rec(page)
            if _body_unavailable(rec):
                continue
            found = rec.get("structured_data")
            if found is None:
                continue
            measured += 1
            if found == 0:
                ctx.add("STRUCTURED_DATA_MISSING", target_url=page.url)
        if not measured:
            ctx.skip("STRUCTURED_DATA_MISSING", "no indexable HTML page carries a block count")
    if "MIXED_CONTENT" in native:
        insecure: dict[str, list[str]] = defaultdict(list)
        rows = ctx.scan_con.execute(
            "SELECT p.url, o.resolved_url FROM resource_graph_occurrences o "
            "JOIN urls p ON p.url_id=o.page_url_id "
            "WHERE p.url LIKE 'https://%' AND o.resolved_url LIKE 'http://%' "
            "ORDER BY p.url, o.resolved_url"
        )
        for page_url, resource in rows:
            if resource not in insecure[page_url]:
                insecure[page_url].append(resource)
        for page_url, resources in insecure.items():
            ctx.add(
                "MIXED_CONTENT",
                target_url=page_url,
                details={
                    "count": len(resources),
                    "insecure_resources": resources[:_PLACEMENT_EVIDENCE_ITEMS],
                },
            )


def check_native_exports(ctx: AuditContext) -> None:
    from .normalize import find_column, normalize_value

    native = native_check_ids(ctx)
    for key, check_id in _NATIVE_EXPORT_CHECKS.items():
        if check_id in native:
            continue
        df = ctx.exports.get(key)
        if df is None or df.empty:
            if ctx.scan_con is not None:
                ctx.skip(check_id, "not measured by this native crawl")
            else:
                ctx.skip(check_id, f"no {key} export (export this SF filter to enable)")
            continue
        col = find_column(df, ["Address", "URL", "Image URL", "Image"])
        if not col:
            ctx.skip(check_id, f"{key} export has no address column")
            continue
        for value in df[col].tolist():
            url = normalize_value(value)
            if url:
                ctx.add(check_id, target_url=url, evidence={"export": ctx.exports.files.get(key)})


ALL_CHECKS = [
    check_lighthouse_reports,
    check_render_assets,
    check_resource_assets,
    check_native_structured_and_mixed,
    check_response_codes,
    check_render_blocking,
    check_indexability,
    check_redirect_type,
    check_titles,
    check_descriptions,
    check_headings,
    check_heading_outline,
    check_link_placement,
    check_canonical_directives,
    check_canonical_policy,
    check_content,
    check_url_and_perf,
    check_schema,
    check_duplicates,
    # extensions — maximize extraction from Internal:All
    check_url_extra,
    check_content_quality,
    check_directives_extra,
    check_canonical_extra,
    check_canonical_chain,
    check_canonical_to_redirect,
    check_canonical_target_error,
    check_canonical_homepage_group,
    check_unlinked_canonical,
    check_pagination,
    check_pagination_series,
    check_pagination_sequence,
    check_links_extra,
    check_tech_extra,
    check_amp_pairing,
    check_native_page_evidence,
    check_native_image_resources,
    check_native_image_size,
    check_native_export_aggregates,
    check_ajax_crawling_scheme,
    check_charset,
    check_doctype,
    check_viewport,
    check_mobile_alternate,
    check_compression,
    check_element_position,
    check_document_skeleton,
    check_html_structure,
    check_url_hygiene,
    check_placeholder_markers,
    check_og,
    check_redirect_chains,
    check_native_exports,
]


def run_rules(ctx: AuditContext) -> None:
    """Run every Internal:All-derived check against the context."""
    if ctx.internal_df is None:
        ctx.skip("INTERNAL_ALL", "Internal:All export not loaded")
        return
    _declare_unparsed_body_skips(ctx)
    for check in ALL_CHECKS:
        check(ctx)
