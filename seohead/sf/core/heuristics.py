"""Heuristics beyond native Screaming Frog findings: HTML weight anomalies, DOM size,
bytes/word and templated titles. These catch problems SF measures but never
marks as errors — e.g. "300 KB page where the site median is 70 KB".

Derived metrics are written back onto ``page.metrics`` even when no threshold
trips, so downstream analysis/LLMs have the raw signal.
"""

from __future__ import annotations

import os
import statistics
import urllib.parse
from collections import Counter
from typing import Any

from .context import AuditContext
from .normalize import find_column

SEPARATORS = (" | ", " — ", " - ", " · ", " :: ", " // ")

# A Tukey fence measures distance in units of spread, so it needs spread to
# exist. A templated site has almost none: every page renders the same shell,
# the interquartile range collapses to zero, and p75 + 1.5 * 0 degenerates into
# "heavier than the median" — which reports a page 0.2% above it as an outlier,
# on most of the site at once. Below this fraction of the median the sizes are
# one value at the resolution that matters, and only the absolute and
# multiple-of-median rules have anything to say.
MIN_IQR_FRACTION_OF_MEDIAN = 0.10

# Screaming Frog's "HTML Document Over 2MB" is an absolute ceiling, not a site-relative
# outlier, so it is a separate finding from LARGE_HTML (200 KB default, median-relative).
HTML_OVER_2MB_BYTES = 2 * 1024 * 1024


def _quantile(sorted_vals: list[float], q: float) -> float:
    if not sorted_vals:
        return 0.0
    pos = (len(sorted_vals) - 1) * q
    lo = int(pos)
    hi = min(lo + 1, len(sorted_vals) - 1)
    frac = pos - lo
    return sorted_vals[lo] * (1 - frac) + sorted_vals[hi] * frac


def size_stats(ctx: AuditContext) -> dict[str, Any]:
    sizes = [p.metrics.get("size_bytes") for p in ctx.indexable_html_pages()]
    sizes = sorted(s for s in sizes if isinstance(s, (int, float)) and s > 0)
    if not sizes:
        return {}
    return {
        "count": len(sizes),
        "median": statistics.median(sizes),
        "p75": _quantile(sizes, 0.75),
        "p90": _quantile(sizes, 0.90),
        "p95": _quantile(sizes, 0.95),
        "max": sizes[-1],
        "iqr": _quantile(sizes, 0.75) - _quantile(sizes, 0.25),
    }


def check_html_weight(ctx: AuditContext) -> dict[str, Any]:
    stats = size_stats(ctx)
    if not stats:
        ctx.skip("LARGE_HTML", "no Size (bytes) data for HTML pages")
        ctx.skip("HTML_BLOAT", "no Size (bytes) data for HTML pages")
        ctx.skip("HTML_OVER_2MB", "no Size (bytes) data for HTML pages")
        return stats

    t = ctx.thresholds
    median = stats["median"] or 1
    abs_bytes = t["large_html_abs_kb"] * 1024
    k = t["large_html_x_median"]
    has_spread = stats["iqr"] >= median * MIN_IQR_FRACTION_OF_MEDIAN
    tukey_upper = stats["p75"] + 1.5 * stats["iqr"] if has_spread else None

    # rank the heaviest pages for the report
    # Keep only scalar rank inputs. Retaining Page models here bypasses the
    # disk context's bounded cache throughout the following two full passes.
    rank_of = {
        url: i + 1
        for i, (url, _size) in enumerate(
            sorted(
                (
                    (p.url, p.metrics["size_bytes"])
                    for p in ctx.indexable_html_pages()
                    if p.metrics.get("size_bytes")
                ),
                key=lambda item: item[1],
                reverse=True,
            )
        )
    }

    # bytes/word distribution for the bloat heuristic
    bpw_vals = []
    for page in ctx.indexable_html_pages():
        size = page.metrics.get("size_bytes")
        wc = page.metrics.get("word_count")
        if size and wc and wc > 0:
            bpw = size / wc
            page.metrics["bytes_per_word"] = round(bpw, 1)
            bpw_vals.append(bpw)
    bpw_median = statistics.median(bpw_vals) if bpw_vals else None

    for page in ctx.indexable_html_pages():
        size = page.metrics.get("size_bytes")
        if not size:
            continue
        ratio = size / median
        page.metrics["size_vs_median_ratio"] = round(ratio, 2)
        is_outlier = size > median * k or (tukey_upper is not None and size > tukey_upper)
        if size > abs_bytes or is_outlier:
            ctx.add(
                "LARGE_HTML",
                target_url=page.url,
                details={
                    "size_bytes": int(size),
                    "site_median": int(median),
                    "ratio": round(ratio, 2),
                    "rank": rank_of.get(page.url),
                    "abs_threshold_kb": t["large_html_abs_kb"],
                    "outlier": bool(is_outlier),
                },
            )
        if size > HTML_OVER_2MB_BYTES:
            ctx.add(
                "HTML_OVER_2MB",
                target_url=page.url,
                details={"size_bytes": int(size), "limit_bytes": HTML_OVER_2MB_BYTES},
            )
        bpw = page.metrics.get("bytes_per_word")
        if (
            bpw
            and bpw_median
            and bpw > bpw_median * t["bytes_per_word_x_median"]
            and (page.metrics.get("word_count") or 0) > 0
        ):
            ctx.add(
                "HTML_BLOAT",
                target_url=page.url,
                details={
                    "bytes_per_word": round(bpw, 1),
                    "site_median_bpw": round(bpw_median, 1),
                    "word_count": page.metrics.get("word_count"),
                    "size_bytes": int(size),
                },
            )
    return stats


# --------------------------------------------------------------------------
# DOM depth / nodes — selected native document or SF stored HTML
# --------------------------------------------------------------------------
def _dom_metrics(html: str) -> tuple[int, int]:
    """Return (max nesting depth, element count) — single O(n) DFS, no parent walks."""
    from lxml import html as lxml_html

    tree = lxml_html.fromstring(html)
    nodes = 0
    max_depth = 0
    stack: list[tuple[Any, int]] = [(tree, 0)]
    while stack:
        el, depth = stack.pop()
        if isinstance(el.tag, str):  # skip comments / processing instructions
            nodes += 1
            max_depth = max(max_depth, depth)
        for child in el:
            stack.append((child, depth + 1))
    return max_depth, nodes


def _stored_html_reader(ctx: AuditContext):
    """Share a bounded HTML reader across DOM, literal and semantic checks."""
    from seohead.storage import ScanError
    from seohead.storage.bodies import read_page_document
    from seohead.storage.corpus_inputs import MAX_DOCUMENT_BYTES

    if ctx.scan_con is not None:
        return lambda url: read_page_document(
            ctx.scan_con, url, max_decoded_bytes=MAX_DOCUMENT_BYTES
        )
    html_dir = ctx.config.get("input", {}).get("html_store_dir")
    index = _build_html_index(html_dir) if html_dir and os.path.isdir(html_dir) else None

    def read(url):
        if index is None:
            raise ScanError("no stored HTML (input.html_store_dir not set; crawl was not stored)")
        path = _match_html_file(index, url)
        if not path:
            raise ScanError("stored HTML present but no files mapped to crawled URLs")
        with open(path, "rb") as stream:
            raw = stream.read(MAX_DOCUMENT_BYTES + 1)
        if len(raw) > MAX_DOCUMENT_BYTES:
            raise ScanError("stored HTML exceeds decoded byte limit")
        return raw.decode("utf-8", errors="replace")

    return read


def _body_coverage(matched: int, total: int, omitted: Counter) -> str | None:
    if not omitted:
        return None
    reasons = "; ".join(f"{reason} ({count})" for reason, count in sorted(omitted.items()))
    return f"stored HTML matched only {matched} of {total} indexable pages; {reasons}"


def check_dom(ctx: AuditContext) -> None:
    from lxml.etree import LxmlError

    from seohead.storage import ScanError

    if not any(ctx.enabled(check) for check in ("DOM_TOO_DEEP", "DOM_TOO_MANY_NODES")):
        return
    read = _stored_html_reader(ctx)
    t = ctx.thresholds
    pages = ctx.indexable_html_pages()
    total = len(pages)
    omitted: Counter = Counter()
    matched = 0
    for page in pages:
        try:
            depth, nodes = _dom_metrics(read(page.url))
        except (ScanError, OSError, ValueError, LxmlError) as exc:
            omitted[str(exc)] += 1
            continue
        matched += 1
        page.metrics["dom_depth"] = depth
        page.metrics["dom_nodes"] = nodes
    coverage_note = _body_coverage(matched, total, omitted)
    for page in pages:
        depth, nodes = page.metrics.get("dom_depth"), page.metrics.get("dom_nodes")
        if depth is None or nodes is None:
            continue
        if depth > t["dom_depth_max"]:
            details = {"dom_depth": depth, "max": t["dom_depth_max"]}
            if coverage_note:
                details["html_coverage"] = coverage_note
            ctx.add("DOM_TOO_DEEP", target_url=page.url, details=details)
        if nodes > t["dom_nodes_max"]:
            details = {"dom_nodes": nodes, "max": t["dom_nodes_max"]}
            if coverage_note:
                details["html_coverage"] = coverage_note
            ctx.add("DOM_TOO_MANY_NODES", target_url=page.url, details=details)
    if coverage_note:
        reason = f"{coverage_note} — DOM size not assessed for the rest"
        ctx.skip("DOM_TOO_DEEP", reason)
        ctx.skip("DOM_TOO_MANY_NODES", reason)
    elif not total:
        ctx.skip("DOM_TOO_DEEP", "no indexable HTML pages")
        ctx.skip("DOM_TOO_MANY_NODES", "no indexable HTML pages")


def _build_html_index(html_dir: str) -> dict[str, str]:
    """Index stored HTML files under several keys so URLs can be matched.

    SF's Store HTML writes a ``<dir>/<host>/<path>`` tree. We key each file by
    its path relative to ``html_dir`` (host+path) *and* its basename, so the
    lookup in :func:`_match_html_file` can try host+path first, then basename.
    """
    index: dict[str, str] = {}
    for root, _dirs, files in os.walk(html_dir):
        for name in files:
            if not name.lower().endswith((".html", ".htm")):
                continue
            full = os.path.join(root, name)
            rel = os.path.relpath(full, html_dir).replace(os.sep, "/").lower()
            index.setdefault(rel, full)
            index.setdefault(rel.lstrip("/"), full)
            index.setdefault(name.lower(), full)  # basename fallback (last wins)
    return index


def _match_html_file(index: dict[str, str], url: str) -> str | None:
    """Try host+path, then path, then basename — return the first hit."""
    parsed = urllib.parse.urlparse(url)
    host = parsed.netloc.lower()
    path = parsed.path
    base = (os.path.basename(path) or "index.html").lower()
    candidates = []
    if host and path:
        hp = f"{host}{path}".lower()
        candidates += [hp, hp.rstrip("/") + "/index.html"]
    if path:
        candidates.append(path.lstrip("/").lower())
    candidates.append(base)
    for key in candidates:
        hit = index.get(key)
        if hit:
            return hit
    return None


# --------------------------------------------------------------------------
# Near-duplicate content — retained native document or SF stored HTML when
# the export did not already compute this itself (issue #15, item 3)
# --------------------------------------------------------------------------
def check_content_duplication(ctx: AuditContext) -> None:
    """NEAR_DUPLICATE / DUPLICATE_BY_HASH computed from stored page text.

    Clustering runs over the collected corpus, with explicit finite document,
    byte, shingle and comparison budgets. Screaming Frog's own
    Near Duplicates/Hash columns already answer this when the crawl exported
    them (``check_content``/``check_duplicates`` in rules.py); this only fills
    whichever half is missing, reusing the selected retained native document
    or ``input.html_store_dir`` that ``check_dom`` reads, scoped to the content area so a
    shared nav/footer does not make every page look alike, and delegating the
    actual clustering to :func:`seohead.checks.duplicate.find_duplicates`
    (SimHash + LSH), which excludes exact-duplicate pairs from the near-dup
    clusters on its own.
    """
    if not any(ctx.enabled(check) for check in ("NEAR_DUPLICATE", "DUPLICATE_BY_HASH")):
        return
    has_native_near = ctx.internal_df is not None and find_column(
        ctx.internal_df, ["No. Near Duplicates"]
    )
    has_native_hash = ctx.internal_df is not None and find_column(
        ctx.internal_df, ["Hash", "Page Hash"]
    )
    if has_native_near:
        ctx.skip("NEAR_DUPLICATE", "SF native Near Duplicates column already covers this")
    if has_native_hash:
        ctx.skip("DUPLICATE_BY_HASH", "SF native Hash column already covers this")
    if has_native_near and has_native_hash:
        return  # nothing left for the stored-text pass to add

    from bs4 import BeautifulSoup

    from seohead.checks.content_area import extract_area_text, resolve_content_area
    from seohead.checks.duplicate import DuplicateBudgetExceeded, content_hash, find_duplicates
    from seohead.storage import ScanError
    from seohead.storage.corpus_inputs import (
        MAX_CORPUS_DOCUMENTS,
        MAX_CORPUS_INPUT_BYTES,
        MAX_DUPLICATE_CANDIDATE_COMPARISONS,
        MAX_DUPLICATE_SHINGLES,
    )

    read = _stored_html_reader(ctx)
    content_cfg = ctx.config.get("content_area", {})
    candidate_pages = ctx.indexable_html_pages()
    total = len(candidate_pages)
    omitted: Counter = Counter()
    items: list[dict[str, Any]] = []
    input_bytes = 0
    visited = 0
    for page in candidate_pages:
        if len(items) >= MAX_CORPUS_DOCUMENTS:
            omitted["duplicate document limit exceeded"] += total - visited
            break
        visited += 1
        try:
            html = read(page.url)
        except (ScanError, OSError, ValueError) as exc:
            omitted[str(exc)] += 1
            continue
        root, _strategy = resolve_content_area(BeautifulSoup(html, "html.parser"), content_cfg)
        text = extract_area_text(root)
        if not text.strip():
            omitted["no comparable content-area text"] += 1
            continue
        size = len(text.encode("utf-8"))
        if input_bytes + size > MAX_CORPUS_INPUT_BYTES:
            omitted["duplicate input-byte budget exceeded"] += total - visited + 1
            break
        input_bytes += size
        items.append({"id": page.url, "text": text})

    coverage_note = _body_coverage(len(items), total, omitted)
    if len(items) < 2:
        reason = coverage_note or "fewer than two comparable indexable page bodies"
        if not has_native_near:
            ctx.skip("NEAR_DUPLICATE", reason)
        if not has_native_hash:
            ctx.skip("DUPLICATE_BY_HASH", reason)
        return

    threshold = ctx.thresholds.get("near_duplicate_similarity", 0.92)
    try:
        result = find_duplicates(
            items,
            threshold=threshold,
            max_shingles=MAX_DUPLICATE_SHINGLES,
            max_candidate_comparisons=MAX_DUPLICATE_CANDIDATE_COMPARISONS,
        )
    except DuplicateBudgetExceeded as exc:
        if not has_native_near:
            ctx.skip("NEAR_DUPLICATE", str(exc))
        # Exact hashes do not depend on a candidate/shingle budget. Preserve
        # their measurement when the independently bounded near pass stops.
        exact_groups: dict[str, list[str]] = {}
        for item in items:
            exact_groups.setdefault(content_hash(item["text"]), []).append(item["id"])
        result = {
            "clusters": [],
            "exact_duplicates": [
                {"hash": digest, "members": members}
                for digest, members in exact_groups.items()
                if len(members) > 1
            ],
        }

    if not has_native_near:
        for cluster in result["clusters"]:
            members = sorted(cluster["members"])
            group = ctx.add_group("NEAR_DUPLICATE", None, members)
            if group is None:
                continue
            for url in members:
                details = {
                    "cluster_min_similarity": cluster["min_similarity"],
                    "cluster_size": len(members),
                    "comparison_url": next(member for member in members if member != url),
                    "status_code": ctx.page_by_url[url].status_code,
                    "method": "content_area_simhash_lsh",
                }
                if coverage_note:
                    details["html_coverage"] = coverage_note
                ctx.add(
                    "NEAR_DUPLICATE",
                    target_url=url,
                    group_id=group.group_id,
                    details=details,
                )
        if coverage_note:
            ctx.skip("NEAR_DUPLICATE", f"{coverage_note} — duplicates not assessed for the rest")
    if not has_native_hash:
        for exact in result["exact_duplicates"]:
            members = sorted(exact["members"])
            group = ctx.add_group("DUPLICATE_BY_HASH", None, members)
            if group is None:
                continue
            for url in members:
                details = {
                    "duplicate_count": len(members),
                    "comparison_url": next(member for member in members if member != url),
                    "status_code": ctx.page_by_url[url].status_code,
                    "content_hash": exact["hash"],
                    "method": "content_area_text_hash",
                }
                if coverage_note:
                    details["html_coverage"] = coverage_note
                ctx.add(
                    "DUPLICATE_BY_HASH",
                    target_url=url,
                    group_id=group.group_id,
                    details=details,
                )
        if coverage_note:
            ctx.skip("DUPLICATE_BY_HASH", f"{coverage_note} — duplicates not assessed for the rest")


# --------------------------------------------------------------------------
# Templated titles: the same prefix or suffix across most pages
# --------------------------------------------------------------------------
def check_templated_titles(ctx: AuditContext) -> None:
    titles = [p.metrics.get("title") for p in ctx.indexable_html_pages()]
    titles = [str(t).strip() for t in titles if t]
    if len(titles) < 5:
        ctx.skip("TITLE_TEMPLATED", "too few titles to assess templating")
        return
    # The registry promises a finding for a shared prefix OR suffix (e.g. "SEOHEAD — page N"
    # is a prefix template, "page N — SEOHEAD" a suffix one); counting only the text after the
    # last separator missed every prefix-templated corpus outright (#206).
    prefixes: Counter = Counter()
    suffixes: Counter = Counter()
    for title in titles:
        for sep in SEPARATORS:
            if sep in title:
                prefixes[title.split(sep, 1)[0].strip()] += 1
                suffixes[title.rsplit(sep, 1)[-1].strip()] += 1
                break
    best: tuple[str, str, int] | None = None  # (direction, token, count)
    for direction, counts in (("prefix", prefixes), ("suffix", suffixes)):
        if not counts:
            continue
        token, count = counts.most_common(1)[0]
        if best is None or count > best[2]:
            best = (direction, token, count)
    if best is None:
        return
    direction, token, count = best
    share = count / len(titles)
    if share >= ctx.thresholds["templated_title_share"]:
        ctx.add(
            "TITLE_TEMPLATED",
            target_url=None,
            details={
                "direction": direction,
                "token": token,
                "share": round(share, 2),
                "pages": count,
                "total": len(titles),
            },
        )


def run_heuristics(ctx: AuditContext) -> dict[str, Any]:
    from .semantic_checks import check_semantic_content

    stats = check_html_weight(ctx)
    check_dom(ctx)
    check_content_duplication(ctx)
    check_semantic_content(ctx)
    check_templated_titles(ctx)
    return stats
