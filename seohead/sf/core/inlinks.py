"""Link localization from ``*:Inlinks`` bulk exports.

This is the module that answers the core request: a broken link — *where it
sits* (source page), *where it goes* (destination), *where in the DOM*
(Link Position + Link Path/XPath), and *on how many pages*. One issue per
destination URL, with every source as a location.
"""

from __future__ import annotations

import json
import re
import statistics
import urllib.parse
from collections import Counter, OrderedDict
from collections.abc import Callable, Mapping
from itertools import groupby
from typing import Any

from seohead.checks.hreflang import code_error
from seohead.checks.parser import robots_directives
from seohead.core.graph import InlinkCompositionRow

from .context import AuditContext
from .crawl_path import bfs_tree_from_seed, route_from_parents
from .internal_linking import summarize_depth, summarize_positions, unmeasured
from .link_score import (
    DEFAULT_DAMPING,
    DEFAULT_MAX_ITERATIONS,
    DEFAULT_TOLERANCE,
    compute_link_scores,
)
from .models import Link
from .normalize import HREFLANG_FIELD_MAP, INLINKS_FIELD_MAP, is_true, norm_url, records_from_df

# logical export key -> (internal-destination check, external-destination check).
# The *:Inlinks exports include links to BOTH internal and external destinations;
# we split by host so an external 404 isn't mislabeled as an internal one.
INLINK_SOURCES = {
    "inlinks_4xx": ("BROKEN_INTERNAL_LINK", "BROKEN_EXTERNAL_LINK"),
    "inlinks_5xx": ("LINK_TO_5XX", "BROKEN_EXTERNAL_LINK"),
    "inlinks_3xx": ("INTERNAL_LINK_TO_REDIRECT", "EXTERNAL_LINK_TO_REDIRECT"),
}


def _site_host(ctx: AuditContext) -> str:
    counts: Counter = Counter()
    for page in ctx.pages:
        host = urllib.parse.urlparse(page.url).netloc.lower()
        if host:
            counts[host] += 1
    return counts.most_common(1)[0][0] if counts else ""


def _all_inlink_records(ctx: AuditContext) -> list[dict[str, Any]] | None:
    """The complete inlink inventory, or ``None`` when it was never exported.

    ``all_inlinks`` (Bulk Export → Links → All Inlinks) is the only export that
    carries every link on the site rather than only the broken ones, so it is
    the one input the whole-graph passes below (link score, inlink
    composition, the discovery path, and the resource inventory) all share.
    """
    df = ctx.exports.get("all_inlinks")
    if df is None or df.empty:
        return None
    return records_from_df(df, INLINKS_FIELD_MAP)


def _graph_access(ctx: AuditContext):
    """The optional native backend; export behavior remains the default."""
    return getattr(ctx, "graph_access", None)


def _link_from_record(rec: dict[str, Any]) -> Link:
    follow_raw = rec.get("follow")
    return Link(
        source_url=rec.get("source_url"),
        destination_url=rec.get("destination_url"),
        anchor=rec.get("anchor"),
        alt_text=rec.get("alt_text"),
        status_code=rec.get("status_code"),
        link_position=rec.get("link_position"),
        link_path=rec.get("link_path"),
        follow=is_true(follow_raw) if follow_raw is not None else None,
        rel=rec.get("rel"),
        target=rec.get("target"),
    )


def _process_export(
    ctx: AuditContext, key: str, internal_check: str, external_check: str, site_host: str
) -> None:
    if ctx.scan_con is not None:
        # Native internal target statuses are emitted by the retained occurrence pass.
        # This export aggregate does not classify retained external HTTP outcomes.
        ctx.skip(
            external_check,
            "not measured by this native check: external response status aggregates are not projected",
        )
        return
    df = ctx.exports.get(key)
    if df is None or df.empty:
        ctx.skip(internal_check, f"export {key} not available")
        ctx.skip(external_check, f"export {key} not available")
        return

    max_locs = ctx.config.get("output", {}).get("max_locations_per_issue", 200)
    by_dest: OrderedDict[str, list[Link]] = OrderedDict()
    raw_dests: dict[str, list[str]] = {}
    for rec in records_from_df(df, INLINKS_FIELD_MAP):
        link = _link_from_record(rec)
        if not link.destination_url:
            continue
        dest_key = norm_url(link.destination_url)
        by_dest.setdefault(dest_key, []).append(link)
        raws = raw_dests.setdefault(dest_key, [])
        if link.destination_url not in raws:
            raws.append(link.destination_url)

    for dest_key, links in by_dest.items():
        # Group occurrences by normalized identity, but report the actual
        # crawled spelling whenever it is known. redirect_map is keyed by the
        # Internal:All Address spelling, so first-seen raw inlink text would
        # make both target_url and final_url depend on CSV row order.
        crawled = ctx.page_by_norm.get(dest_key)
        dest = (
            crawled.url
            if crawled is not None
            else sorted(raw_dests[dest_key], key=lambda value: (value.casefold(), value))[0]
        )
        dest_host = urllib.parse.urlparse(dest).netloc.lower()
        is_internal = (not dest_host) or (dest_host == site_host)
        check_id = internal_check if is_internal else external_check
        status = next((link.status_code for link in links if link.status_code), None)
        details: dict[str, Any] = {
            "link_position_breakdown": _position_breakdown(links),
            "destination_scope": "internal" if is_internal else "external",
        }
        if check_id == "INTERNAL_LINK_TO_REDIRECT":
            final = ctx.redirect_map.get(dest)
            if final:
                details["final_url"] = final
        locations = [link.as_location() for link in links[:max_locs]]
        # Count distinct source pages, not raw link occurrences. This answers
        # "on how many pages does this link appear?" when a page repeats it.
        n_sources = len({link.source_url for link in links if link.source_url})
        ctx.add(
            check_id,
            target_url=dest,
            status_code=status,
            occurrences_count=n_sources or len(links),
            locations=locations,
            details=details,
            evidence={
                "export": ctx.exports.files.get(key),
                "raw_destinations": raw_dests[dest_key],
            },
        )


def _position_breakdown(links: list[Link]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for link in links:
        pos = link.link_position or "Unknown"
        counts[pos] = counts.get(pos, 0) + 1
    return counts


# ---------------------------------------------------------------------------
# Generic anchor text
# ---------------------------------------------------------------------------
# Curated Russian-and-English dictionary of non-descriptive anchors. Match the
# entire normalized anchor after lowercasing, collapsing whitespace, and
# trimming surrounding punctuation. This catches "Read More", localized
# equivalents, and variants such as "  click here…", but not a meaningful
# phrase such as "click here to buy" where the remaining words add context.
_GENERIC_ANCHORS = frozenset(
    {
        # Russian
        "тут",
        "здесь",
        "далее",
        "подробнее",
        "сюда",
        "ссылка",
        "читать далее",
        "читать дальше",
        "читайте далее",
        "читайте дальше",
        "по ссылке",
        "перейти по ссылке",
        "перейдите по ссылке",
        "нажмите здесь",
        "узнать больше",
        "больше",
        "смотрите тут",
        # English
        "here",
        "click here",
        "read more",
        "learn more",
        "more",
        "see more",
        "view more",
        "click",
        "link",
        "this",
        "this link",
        "continue reading",
        "check it out",
        "details",
    }
)

_GENERIC_ANCHOR_TRIM = " \t\u00a0.,;:!?«»\"'()[]{}…—–-"  # noqa: RUF001 - punctuation set


def _norm_anchor(anchor: str) -> str:
    cleaned = re.sub(r"\s+", " ", anchor.lower()).strip()
    return cleaned.strip(_GENERIC_ANCHOR_TRIM).strip()


def check_anchor_text(ctx: AuditContext) -> None:
    """Flag localized non-descriptive anchors such as "here" and "click here".

    Scans every available ``*:Inlinks`` export (``all_inlinks`` preferred; the
    status-code reports as fallback). The fix lives on the *source* page, so one
    issue is emitted per source URL, with each generic link in ``details``. If no
    inlinks export is loaded the check skips honestly rather than emit zeros.
    """
    max_locs = ctx.config.get("output", {}).get("max_locations_per_issue", 200)
    export_keys = [
        k
        for k in ("all_inlinks", "inlinks_4xx", "inlinks_5xx", "inlinks_3xx")
        if ctx.exports.has(k) and not ctx.exports.get(k).empty
    ]
    if not export_keys:
        graph = _graph_access(ctx)
        if graph is not None:
            for group in graph.iter_anchor_groups(
                lambda anchor: _norm_anchor(anchor) in _GENERIC_ANCHORS, max_locs
            ):
                ctx.add(
                    "GENERIC_ANCHOR_TEXT",
                    target_url=group.source_url,
                    occurrences_count=group.occurrences_count,
                    locations=group.locations,
                    details={"generic_links": group.generic_links},
                    evidence={"exports": ["all_inlinks"], "files": [None]},
                )
            return
        ctx.skip(
            "GENERIC_ANCHOR_TEXT",
            "no *:Inlinks export available (export 'All Inlinks' or any *:Inlinks report)",
        )
        return

    seen: set[tuple[str | None, str | None, str]] = set()
    by_source: OrderedDict[str, list[Link]] = OrderedDict()
    for key in export_keys:
        for rec in records_from_df(ctx.exports.get(key), INLINKS_FIELD_MAP):
            link = _link_from_record(rec)
            anchor = link.anchor
            if not anchor or not link.source_url:
                continue
            if _norm_anchor(anchor) not in _GENERIC_ANCHORS:
                continue
            dedup = (link.source_url, link.destination_url, anchor.strip().lower())
            if dedup in seen:
                continue
            seen.add(dedup)
            by_source.setdefault(link.source_url, []).append(link)

    for source, links in by_source.items():
        generic_links = [
            {
                "anchor": link.anchor,
                "destination": link.destination_url,
                "link_position": link.link_position,
            }
            for link in links[:max_locs]
        ]
        ctx.add(
            "GENERIC_ANCHOR_TEXT",
            target_url=source,
            occurrences_count=len(links),
            locations=[link.as_location() for link in links[:max_locs]],
            details={"generic_links": generic_links},
            evidence={
                "exports": export_keys,
                "files": [ctx.exports.files.get(k) for k in export_keys],
            },
        )


def _hreflang_unavailable(ctx, sf_reason: str) -> str:
    """Why an hreflang check cannot answer, in the words of the run that produced it.

    An SF run names the export that would enable the check. A native crawl has no export to
    name: it either retained declarations, in which case this check has no native measure, or
    it retained none at all.
    """
    if ctx.scan_con is None:
        return sf_reason
    if ctx.native_hreflang and ctx.native_hreflang.get("declarations"):
        return "not measured by this native crawl"
    return "the native crawl retained no hreflang declarations"


# ---------------------------------------------------------------------------
# hreflang -> broken target
# ---------------------------------------------------------------------------
def _native_hreflang_groups(ctx, key, predicate):
    """Group native relation findings on disk and quote an explicitly bounded sample."""
    con = ctx._disk_pages.con
    con.execute(
        "CREATE TEMP TABLE hreflang_findings(seq INTEGER PRIMARY KEY,group_key TEXT,value TEXT)"
    )
    try:
        con.executemany(
            "INSERT INTO hreflang_findings(group_key,value) VALUES(?,?)",
            (
                (item[key], json.dumps(item))
                for item in ctx.native_hreflang["declarations"]
                if predicate(item)
            ),
        )
        con.execute("CREATE INDEX hreflang_findings_group ON hreflang_findings(group_key,seq)")
        limit = ctx.config.get("output", {}).get("max_locations_per_issue", 200)
        for group_key, count in con.execute(
            "SELECT group_key,COUNT(*) FROM hreflang_findings GROUP BY group_key ORDER BY MIN(seq)"
        ):
            rows = [
                json.loads(row[0])
                for row in con.execute(
                    "SELECT value FROM hreflang_findings WHERE group_key=? ORDER BY seq LIMIT ?",
                    (group_key, limit),
                )
            ]
            yield group_key, rows, count
    finally:
        con.execute("DROP TABLE hreflang_findings")


def _native_sample_details(name, entries, count):
    details = {name: entries}
    if count > len(entries):
        details.update(
            sample_truncated=True,
            omitted_relations=count - len(entries),
            full_relations_collection="/summary/saved_corpus_derivations/internationalization/declarations",
        )
    return details


# A captured page, or a target the post-crawl probe fetched because no link reached it (#987).
_ANSWERED_TARGET_STATES = frozenset({"observed", "probed"})


def _active_declaration(item):
    return (
        item["source_representation_state"] == "active" and item["declaration_state"] == "declared"
    )


def check_hreflang_targets(ctx: AuditContext) -> None:
    """HREFLANG_BROKEN_TARGET — hreflang points at a 3xx/4xx/5xx URL.

    Reads the Bulk Export → Links → ``All Hreflang`` report (one row per
    hreflang annotation: source → target URL + lang). Each target is matched
    against the crawl: a target that responds 3xx/4xx/5xx (or carries a Redirect
    URL) breaks international localization and is flagged on its source page.
    Targets not in the crawl (external cross-domain hreflang) cannot be
    classified and are skipped silently. If the export is absent the check
    skips honestly rather than emit a dead zero.
    """
    native = ctx.native_hreflang
    if native is not None and native["declarations"]:

        def broken(item):
            target = item["target_observation"]
            code = target.get("status_code")
            return (
                _active_declaration(item)
                and target["state"] in _ANSWERED_TARGET_STATES
                and type(code) is int
                and (code >= 300 or target["redirect_url"])
            )

        fired = False
        for source, rows, count in _native_hreflang_groups(ctx, "source_url", broken):
            fired = True
            targets = [
                {
                    "hreflang": item["lang"],
                    "target_url": item["target"],
                    "target_identity": item["target_identity"],
                    "status_code": item["target_observation"]["status_code"],
                    "redirect_url": item["target_observation"]["redirect_url"],
                    "source_declaration_id": item["source_declaration_id"],
                    "label_context": item["label_context"],
                }
                for item in rows
            ]
            ctx.add(
                "HREFLANG_BROKEN_TARGET",
                target_url=source,
                occurrences_count=count,
                details=_native_sample_details("broken_targets", targets, count),
            )
        unmeasured = any(
            _active_declaration(item)
            and (
                item["target_observation"]["state"] not in _ANSWERED_TARGET_STATES
                or type(item["target_observation"].get("status_code")) is not int
            )
            for item in native["declarations"]
        )
        if not fired and (unmeasured or native["coverage"]["state"] != "complete"):
            ctx.skip(
                "HREFLANG_BROKEN_TARGET",
                "one or more hreflang targets or the retained population were unmeasured",
            )
        return

    df = ctx.exports.get("all_hreflang")
    if df is None or df.empty:
        ctx.skip(
            "HREFLANG_BROKEN_TARGET",
            _hreflang_unavailable(
                ctx, "no all_hreflang export (export Bulk Export → Links → All Hreflang to enable)"
            ),
        )
        return

    by_source: OrderedDict[str, list[dict[str, Any]]] = OrderedDict()
    seen_pairs: set[tuple[str, str]] = set()
    for rec in records_from_df(df, HREFLANG_FIELD_MAP):
        src = rec.get("source_url")
        dest = rec.get("destination_url")
        if not src or not dest:
            continue
        # #176 audit: this reads the single 2xx-preferring representative, not every
        # variant under the key, but that stays correct here — unlike the bug in
        # check_canonical_to_redirect, a 3xx and a 4xx twin agree on the verdict this
        # check cares about (both are "broken"), so which one is picked only changes
        # which status/redirect_url gets quoted, never whether HREFLANG_BROKEN_TARGET fires.
        target = ctx.page_by_norm.get(norm_url(dest))
        if target is None:
            continue  # external / not crawled — cannot classify the target
        code = target.status_code
        redirect_url = ctx.redirect_map.get(target.url)
        is_redirect = (code is not None and 300 <= code <= 399) or bool(redirect_url)
        is_error = code is not None and code >= 400
        if not (is_redirect or is_error):
            continue
        pair = (src, dest)
        if pair in seen_pairs:
            continue
        seen_pairs.add(pair)
        by_source.setdefault(src, []).append(
            {
                "hreflang": rec.get("hreflang"),
                "target_url": dest,
                "status_code": code,
                "redirect_url": redirect_url,
            }
        )

    for source, targets in by_source.items():
        ctx.add(
            "HREFLANG_BROKEN_TARGET",
            target_url=source,
            occurrences_count=len(targets),
            details={"broken_targets": targets},
            evidence={"export": ctx.exports.files.get("all_hreflang")},
        )


def check_hreflang_unlinked_targets(ctx: AuditContext) -> None:
    """HREFLANG_UNLINKED_TARGET — an hreflang alternate no ordinary hyperlink reaches.

    A URL that is only ever named by another page's ``<link rel="alternate"
    hreflang>`` is discoverable by a search engine following the annotation,
    but never by a user or crawler following a link. This is the hreflang twin
    of UNLINKED_CANONICAL: the same set difference, with hreflang targets in
    place of canonical targets. The target's own Inlinks count is read from
    Internal:All, which counts hyperlinks only, so hreflang declarations never
    count as inlinks. A page that names itself is not an unlinked alternate.
    On a partial crawl "no inlink" cannot be proven, so
    ``aggregate.aggregate`` withholds this finding.
    """
    df = ctx.exports.get("all_hreflang")
    if df is None or df.empty:
        ctx.skip(
            "HREFLANG_UNLINKED_TARGET",
            _hreflang_unavailable(
                ctx, "no all_hreflang export (export Bulk Export → Links → All Hreflang to enable)"
            ),
        )
        return
    if not any(_rec(p).get("inlinks") is not None for p in ctx.pages):
        ctx.skip("HREFLANG_UNLINKED_TARGET", "no Inlinks column in Internal:All")
        return
    declared_from: OrderedDict[str, list[str]] = OrderedDict()
    for rec in records_from_df(df, HREFLANG_FIELD_MAP):
        src = rec.get("source_url")
        dest = rec.get("destination_url")
        if not src or not dest:
            continue
        source_norm = norm_url(src)
        target_norm = norm_url(dest)
        if source_norm == target_norm:
            continue  # a self-reference is not evidence that anything links here
        sources = declared_from.setdefault(target_norm, [])
        if src not in sources:
            sources.append(src)
    for target_norm, sources in declared_from.items():
        # Targets outside the crawl cannot be classified, as in check_hreflang_targets.
        target = ctx.page_by_norm.get(target_norm)
        if target is None:
            continue
        rec = _rec(target)
        if rec.get("crawl_depth") == 0:
            continue  # the homepage is never "unlinked"
        inlinks = rec.get("inlinks")
        if inlinks is None or inlinks > 0:
            continue
        ctx.add(
            "HREFLANG_UNLINKED_TARGET",
            target_url=target.url,
            details={"declared_from": sorted(sources)},
            evidence={"export": ctx.exports.files.get("all_hreflang")},
        )


def _rec(page: Any) -> dict[str, Any]:
    return page.metrics.get("_record", {})


# ---------------------------------------------------------------------------
# hreflang -> code/self-reference/x-default/duplicate/canonical quality
# ---------------------------------------------------------------------------
_HREFLANG_QUALITY_CHECKS = (
    "HREFLANG_INVALID_CODE",
    "HREFLANG_MULTIPLE_ENTRIES",
    "HREFLANG_MISSING_SELF_REFERENCE",
    "HREFLANG_MISSING_XDEFAULT",
    "HREFLANG_NOT_CANONICAL",
)


def _native_source_groups(native):
    # Context evidence is ordered by page/document identity; only the active
    # document contributes, so one group is bounded by one captured payload.
    active = (
        item for item in native["declarations"] if item["source_representation_state"] == "active"
    )
    for source, rows in groupby(active, key=lambda item: item["source_url"]):
        yield source, list(rows)


def check_hreflang_quality(ctx: AuditContext) -> None:
    """Validate each page's own hreflang set: codes, duplicates, self, x-default, canonical.

    Reads the same Bulk Export → Links → ``All Hreflang`` report as
    :func:`check_hreflang_targets` (one row per source → destination + lang
    annotation) and reuses the ISO 639-1/3166-1 validator already shipped for
    the single-URL ``seo_hreflang_check`` tool (:func:`seohead.checks.hreflang.
    code_error`) instead of re-implementing it. Every check here groups by the
    declaring page (source), matching how a browser or crawler reads one
    page's hreflang set. If the export is absent, all five checks skip
    honestly rather than emit dead zeros.
    """
    native = ctx.native_hreflang
    if native is not None and native["declarations"]:
        for source, rows in _native_source_groups(native):
            entries = [{"hreflang": row["lang"], "destination_url": row["target"]} for row in rows]
            _check_invalid_codes(ctx, source, entries, {})
            _check_duplicate_entries(ctx, source, entries, {})
            targets = [row for row in entries if row["destination_url"]]
            _check_self_reference(ctx, source, targets, {})
            _check_xdefault(ctx, source, targets, {})
            _check_not_canonical(ctx, source, targets, {})
        return
    df = ctx.exports.get("all_hreflang")
    if df is None or df.empty:
        for check_id in _HREFLANG_QUALITY_CHECKS:
            ctx.skip(
                check_id,
                _hreflang_unavailable(
                    ctx, "no all_hreflang export (export Bulk Export -> Links -> All Hreflang)"
                ),
            )
        return

    by_source: OrderedDict[str, list[dict[str, Any]]] = OrderedDict()
    for rec in records_from_df(df, HREFLANG_FIELD_MAP):
        src = rec.get("source_url")
        if not src:
            continue
        by_source.setdefault(src, []).append(rec)

    evidence = {"export": ctx.exports.files.get("all_hreflang")}
    for source, entries in by_source.items():
        _check_invalid_codes(ctx, source, entries, evidence)
        _check_duplicate_entries(ctx, source, entries, evidence)
        target_entries = [rec for rec in entries if rec.get("destination_url")]
        _check_self_reference(ctx, source, target_entries, evidence)
        _check_xdefault(ctx, source, target_entries, evidence)
        _check_not_canonical(ctx, source, target_entries, evidence)


def _check_invalid_codes(
    ctx: AuditContext, source: str, entries: list[dict[str, Any]], evidence: dict[str, Any]
) -> None:
    invalid = []
    for rec in entries:
        lang = rec.get("hreflang")
        if not lang:
            continue
        reason = code_error(lang)
        if reason:
            invalid.append(
                {"hreflang": lang, "destination": rec.get("destination_url"), "reason": reason}
            )
    if invalid:
        ctx.add(
            "HREFLANG_INVALID_CODE",
            target_url=source,
            occurrences_count=len(invalid),
            details={"invalid": invalid},
            evidence=evidence,
        )


def _check_duplicate_entries(
    ctx: AuditContext, source: str, entries: list[dict[str, Any]], evidence: dict[str, Any]
) -> None:
    # Language tags are case-insensitive: "en-US" declared twice (even with
    # different casing) is the same annotation twice, not two annotations.
    folded = [str(rec["hreflang"]).strip().lower() for rec in entries if rec.get("hreflang")]
    duplicates = sorted({lang for lang in folded if folded.count(lang) > 1})
    if duplicates:
        ctx.add(
            "HREFLANG_MULTIPLE_ENTRIES",
            target_url=source,
            occurrences_count=len(duplicates),
            details={"duplicate_values": duplicates},
            evidence=evidence,
        )


def _check_self_reference(
    ctx: AuditContext, source: str, entries: list[dict[str, Any]], evidence: dict[str, Any]
) -> None:
    source_norm = norm_url(source)
    destinations = {norm_url(rec.get("destination_url")) for rec in entries}
    if source_norm not in destinations:
        ctx.add(
            "HREFLANG_MISSING_SELF_REFERENCE",
            target_url=source,
            details={"declared_targets": sorted({rec["destination_url"] for rec in entries})},
            evidence=evidence,
        )


def _check_xdefault(
    ctx: AuditContext, source: str, entries: list[dict[str, Any]], evidence: dict[str, Any]
) -> None:
    folded = {str(rec["hreflang"]).strip().lower() for rec in entries if rec.get("hreflang")}
    if "x-default" not in folded:
        ctx.add("HREFLANG_MISSING_XDEFAULT", target_url=source, evidence=evidence)


def _check_not_canonical(
    ctx: AuditContext, source: str, entries: list[dict[str, Any]], evidence: dict[str, Any]
) -> None:
    offenders = []
    unmeasured = 0
    for rec in entries:
        dest = rec.get("destination_url")
        # #176 audit: correct by construction. A redirecting twin under this key rarely
        # carries a canonical tag of its own (SF's parser reads canonical off the fetched
        # HTML, and a 3xx has none) — the 2xx variant is the one this check needs.
        target = ctx.page_by_url.get(dest) or ctx.page_by_norm.get(norm_url(dest))
        canonical = (
            _rec(target).get("canonical")
            if target
            else ctx.target_probes.get(norm_url(dest), {}).get("canonical")
        )
        if (target is None and canonical is None) or (
            target is not None and _rec(target).get("body_unavailable")
        ):
            unmeasured += 1
            continue
        if canonical and norm_url(canonical) != norm_url(dest):
            offenders.append(
                {"hreflang": rec.get("hreflang"), "destination": dest, "canonical": canonical}
            )
    if offenders:
        ctx.add(
            "HREFLANG_NOT_CANONICAL",
            target_url=source,
            occurrences_count=len(offenders),
            details={"non_canonical_targets": offenders},
            evidence=evidence,
        )
    if unmeasured:
        ctx.skip(
            "HREFLANG_NOT_CANONICAL",
            f"{unmeasured} hreflang target(s) lack measured canonical annotations; not measured by this native crawl or export",
        )


def check_hreflang_reciprocity(ctx: AuditContext) -> None:
    """HREFLANG_MISSING_RETURN_LINK — A names B, but B never names A back.

    Google's hreflang contract requires every annotation to be reciprocal:
    if A points to B, B must point back to A. Whether B reciprocates is a
    property of the *pair* and depends on B's own hreflang set, which is only
    on hand once B itself has been crawled — provable only once the crawl of
    both sides is complete (issue #15, item 6).
    """
    native = ctx.native_hreflang
    if native is not None and native["declarations"]:

        def missing(item):
            return (
                _active_declaration(item)
                and item["source_identity"] != item["target_identity"]
                and item["reciprocity"]["state"] == "missing"
            )

        fired = False
        for target_identity, entries, count in _native_hreflang_groups(
            ctx, "target_identity", missing
        ):
            fired = True
            target = ctx.page_by_norm.get(target_identity)
            relations = [
                {
                    "source_url": item["source_url"],
                    "source_identity": item["source_identity"],
                    "target": item["target"],
                    "target_identity": item["target_identity"],
                    "hreflang": item["lang"],
                    "source_declaration_id": item["source_declaration_id"],
                    "label_context": item["label_context"],
                }
                for item in entries
            ]
            details = _native_sample_details("relations", relations, count)
            details["expected_return_to"] = sorted({item["source_url"] for item in entries})
            ctx.add(
                "HREFLANG_MISSING_RETURN_LINK",
                target_url=target.url if target else entries[0]["target"],
                occurrences_count=count,
                details=details,
            )
        if not fired and native["coverage"]["state"] != "complete":
            ctx.skip(
                "HREFLANG_MISSING_RETURN_LINK",
                "retained hreflang relationship coverage is incomplete: "
                + native["coverage"]["reason"],
            )
        return

    df = ctx.exports.get("all_hreflang")
    if df is None or df.empty:
        ctx.skip(
            "HREFLANG_MISSING_RETURN_LINK",
            _hreflang_unavailable(
                ctx, "no all_hreflang export (export Bulk Export -> Links -> All Hreflang)"
            ),
        )
        return

    edges: set[tuple[str, str]] = set()
    for rec in records_from_df(df, HREFLANG_FIELD_MAP):
        src, dest = rec.get("source_url"), rec.get("destination_url")
        if not src or not dest:
            continue
        src_norm, dest_norm = norm_url(src), norm_url(dest)
        if src_norm == dest_norm:
            continue  # a self-reference is not a reciprocity pair
        edges.add((src_norm, dest_norm))

    missing_by_target: OrderedDict[str, list[str]] = OrderedDict()
    unmeasured = 0
    for src_norm, dest_norm in sorted(edges):
        if (dest_norm, src_norm) in edges:
            continue  # B already names A back
        # #176 audit: every read of page_by_norm below is "does this key exist in the
        # crawl" or a display label — the reciprocity verdict itself comes entirely from
        # the hreflang edge set above, so which variant is the representative is moot.
        target = ctx.page_by_norm.get(dest_norm)
        if target is None:
            unmeasured += 1
            continue  # external / not crawled — cannot fault it for not reciprocating
        if not target.is_2xx or not target.is_html or _rec(target).get("body_unavailable"):
            unmeasured += 1
            continue  # no complete target declaration observation
        missing_by_target.setdefault(dest_norm, []).append(src_norm)

    for dest_norm, source_norms in missing_by_target.items():
        target = ctx.page_by_norm[dest_norm]
        expected_from = sorted(
            ctx.page_by_norm[n].url if n in ctx.page_by_norm else n for n in source_norms
        )
        ctx.add(
            "HREFLANG_MISSING_RETURN_LINK",
            target_url=target.url,
            occurrences_count=len(expected_from),
            details={"expected_return_to": expected_from},
            evidence={"export": ctx.exports.files.get("all_hreflang")},
        )
    if not missing_by_target and unmeasured:
        ctx.skip(
            "HREFLANG_MISSING_RETURN_LINK",
            "one or more hreflang targets lack a complete crawled HTML observation",
        )


def check_hreflang_noindex_targets(ctx: AuditContext) -> None:
    """Flag observed noindex alternates; unknown targets stay unmeasured."""
    native = ctx.native_hreflang
    by_source: OrderedDict[str, list[dict[str, Any]]] = OrderedDict()
    unmeasured = 0
    if native is not None and native["declarations"]:

        def noindex(item):
            target = item["target_observation"]
            return (
                _active_declaration(item)
                and target["state"] in {"observed", "probed"}
                and target["indexability"]["reason"] == "noindex directive"
            )

        fired = False
        for source, rows, count in _native_hreflang_groups(ctx, "source_url", noindex):
            fired = True
            entries = [
                {
                    "hreflang": item["lang"],
                    "target": item["target"],
                    "target_identity": item["target_identity"],
                    "source_declaration_id": item["source_declaration_id"],
                    "label_context": item["label_context"],
                }
                for item in rows
            ]
            ctx.add(
                "HREFLANG_NOINDEX_TARGET",
                target_url=source,
                occurrences_count=count,
                details=_native_sample_details("noindex_targets", entries, count),
            )
        unmeasured = any(
            _active_declaration(item)
            and (
                item["target_observation"]["state"] not in {"observed", "probed"}
                or item["target_observation"]["indexability"]["state"] == "unmeasured"
            )
            for item in native["declarations"]
        )
        if not fired and (unmeasured or native["coverage"]["state"] != "complete"):
            ctx.skip(
                "HREFLANG_NOINDEX_TARGET",
                "one or more hreflang targets or the retained population were unmeasured",
            )
        return
    else:
        df = ctx.exports.get("all_hreflang")
        if df is None or df.empty:
            ctx.skip(
                "HREFLANG_NOINDEX_TARGET",
                _hreflang_unavailable(ctx, "no all_hreflang declaration export"),
            )
            return
        for rec in records_from_df(df, HREFLANG_FIELD_MAP):
            source, destination = rec.get("source_url"), rec.get("destination_url")
            if not source or not destination:
                continue
            target = ctx.page_by_norm.get(norm_url(destination))
            if target is None:
                unmeasured += 1
                continue
            observed = _rec(target)
            reason = str(observed.get("indexability_status") or "").strip().lower()
            directives = robots_directives(observed.get("meta_robots"), observed.get("x_robots"))
            if reason.startswith("noindex") or reason == "none" or "noindex" in directives:
                by_source.setdefault(source, []).append(
                    {
                        "hreflang": rec.get("hreflang"),
                        "target": destination,
                        "target_identity": norm_url(destination),
                    }
                )
            elif str(observed.get("indexability") or "").strip().lower() != "indexable":
                unmeasured += 1
    for source, entries in by_source.items():
        ctx.add(
            "HREFLANG_NOINDEX_TARGET",
            target_url=source,
            occurrences_count=len(entries),
            details={"noindex_targets": entries},
        )
    if not by_source and (
        unmeasured or (native is not None and native["coverage"]["state"] != "complete")
    ):
        ctx.skip(
            "HREFLANG_NOINDEX_TARGET",
            "one or more hreflang targets or the retained population were unmeasured",
        )


# "x-default" names a fallback for unmatched users, not a language and region, so
# it is never a locale claim either side of a pair can be measured against: a page
# is routinely declared both as "en" by its counterparts and as "x-default" by
# itself, and reading the second as a contradiction of the first would fire on the
# single most common correct hreflang layout there is.
_XDEFAULT = "x-default"


def _hreflang_code(value: Any) -> str:
    """One hreflang value folded for comparison -- case only, nothing else.

    Language tags are case-insensitive ("en-GB" and "en-gb" are one tag), so the
    fold is safe. Nothing further is normalised on purpose: "en" and "en-GB" are
    genuinely different annotations, and quietly treating a region-less tag as
    matching a regioned one would hide exactly the inconsistency this check exists
    to find.
    """
    return str(value or "").strip().lower()


def check_hreflang_confirmation_consistency(ctx: AuditContext) -> None:
    """HREFLANG_INCONSISTENT_CONFIRMATION -- A declares B as "fr"; B says it is "de".

    :func:`check_hreflang_reciprocity` asks only whether a return link exists. A pair
    can be fully reciprocal and still be discarded by Google, because the contract is
    that both sides name the *same* language and region code: a page's counterparts
    must call it what it calls itself. That is the fact this reads (#386).

    The counterpart's own self-referencing hreflang is the authority for what it is,
    which is why a counterpart with no self-reference is passed over entirely rather
    than guessed at -- that page has no statement to disagree with, and
    HREFLANG_MISSING_SELF_REFERENCE already names it. ``x-default`` is excluded on
    both sides (see ``_XDEFAULT``), and a declaration whose target was never crawled
    is left alone for the same reason reciprocity leaves it alone: a page nobody
    fetched cannot be faulted for what it does or does not say.

    Reported against the *declaring* page, because that is where the annotation that
    disagrees was written; the details name the counterpart, the code this page
    claimed for it, and the codes the counterpart confirms for itself, so the reader
    can see which of the two is wrong without opening both.
    """
    native = ctx.native_hreflang
    if native is not None and native["declarations"]:
        if ctx._saved_corpus is not None:
            confirmed_for = ctx._saved_corpus.owner.self_codes
        else:
            self_codes = {}
            for item in native["declarations"]:
                code = _hreflang_code(item["lang"])
                if (
                    item["source_representation_state"] == "active"
                    and item["source_identity"] == item["target_identity"]
                    and code
                    and code != _XDEFAULT
                ):
                    self_codes.setdefault(item["source_identity"], set()).add(code)
            confirmed_for = self_codes.get
        for source, rows in _native_source_groups(native):
            mismatched = []
            for item in rows:
                code = _hreflang_code(item["lang"])
                if (
                    not code
                    or code == _XDEFAULT
                    or item["source_identity"] == item["target_identity"]
                ):
                    continue
                confirmed = confirmed_for(item["target_identity"])
                target = ctx.page_by_norm.get(item["target_identity"])
                if confirmed and code not in confirmed and target is not None:
                    mismatched.append(
                        {
                            "counterpart": target.url,
                            "declared_here": code,
                            "confirmed_there": sorted(confirmed),
                        }
                    )
            if mismatched:
                page = ctx.page_by_norm.get(norm_url(source))
                ctx.add(
                    "HREFLANG_INCONSISTENT_CONFIRMATION",
                    target_url=page.url if page else source,
                    occurrences_count=len(mismatched),
                    details={"inconsistent": mismatched},
                )
        return
    df = ctx.exports.get("all_hreflang")
    if df is None or df.empty:
        ctx.skip(
            "HREFLANG_INCONSISTENT_CONFIRMATION",
            _hreflang_unavailable(
                ctx, "no all_hreflang export (export Bulk Export -> Links -> All Hreflang)"
            ),
        )
        return

    # What each page declares about itself, and what every other page declares
    # about it, keyed the same way so the two are comparable at all.
    self_codes: dict[str, set[str]] = {}
    claims: OrderedDict[str, list[tuple[str, str]]] = OrderedDict()
    # The URL as the export wrote it, for the case where neither end is in the
    # crawl's own page table: a normalised key is a comparison aid, not an address
    # a reader can open.
    written_as: dict[str, str] = {}
    for rec in records_from_df(df, HREFLANG_FIELD_MAP):
        src, dest = rec.get("source_url"), rec.get("destination_url")
        code = _hreflang_code(rec.get("hreflang"))
        if not src or not dest or not code or code == _XDEFAULT:
            continue
        src_norm, dest_norm = norm_url(src), norm_url(dest)
        written_as.setdefault(src_norm, str(src))
        written_as.setdefault(dest_norm, str(dest))
        if src_norm == dest_norm:
            self_codes.setdefault(src_norm, set()).add(code)
        else:
            claims.setdefault(src_norm, []).append((dest_norm, code))

    evidence = {"export": ctx.exports.files.get("all_hreflang")}
    for src_norm, declared in claims.items():
        source_page = ctx.page_by_norm.get(src_norm)
        mismatched = []
        for dest_norm, code in declared:
            confirmed = self_codes.get(dest_norm)
            if not confirmed or code in confirmed:
                continue  # no self-statement to contradict, or the two agree
            target = ctx.page_by_norm.get(dest_norm)
            if target is None:
                continue  # counterpart was not crawled, so its self-row is not usable evidence
            mismatched.append(
                {
                    "counterpart": target.url,
                    "declared_here": code,
                    "confirmed_there": sorted(confirmed),
                }
            )
        if mismatched:
            ctx.add(
                "HREFLANG_INCONSISTENT_CONFIRMATION",
                target_url=(source_page.url if source_page is not None else written_as[src_norm]),
                occurrences_count=len(mismatched),
                details={"inconsistent": mismatched},
                evidence=evidence,
            )


# ---------------------------------------------------------------------------
# Whole-graph passes over the complete inlink inventory (issue #15, items
# 1, 7, 10, 11): each needs every link on the site, not only the broken ones,
# so all four honestly skip without ``all_inlinks``.
# ---------------------------------------------------------------------------
def _internal_hyperlink_edges(
    records: list[dict[str, Any]], site_host: str
) -> list[tuple[str, str]]:
    """(source, destination) pairs for internal, followed ``Hyperlink`` rows.

    Excludes external destinations (a different host) and rows Screaming Frog
    marks ``Follow: false`` — neither carries internal link equity or forms
    part of internal navigation. A row with no Type value at all (an older
    export) is kept rather than dropped, since assuming it is a hyperlink is
    the safer default for this filter.
    """
    edges: list[tuple[str, str]] = []
    for rec in records:
        source, dest = rec.get("source_url"), rec.get("destination_url")
        if not source or not dest:
            continue
        link_type = rec.get("type")
        if link_type is not None and str(link_type).strip().lower() != "hyperlink":
            continue
        follow = rec.get("follow")
        if follow is not None and not is_true(follow):
            continue
        dest_host = urllib.parse.urlparse(dest).netloc.lower()
        if dest_host and dest_host != site_host:
            continue
        edges.append((norm_url(source), norm_url(dest)))
    return edges


def _emit_link_scores(ctx: AuditContext, count: int, median: float, score_for) -> None:
    """Apply the one reviewed LOW_LINK_SCORE threshold/emission path."""
    for page in ctx.pages:
        score = score_for(norm_url(page.url))
        if score is not None:
            page.metrics["link_score_computed"] = round(score, 6)
    if count < 5 or median <= 0:
        return
    ratio = ctx.thresholds.get("link_score_low_ratio", 0.25)
    for page in ctx.indexable_html_pages():
        if _rec(page).get("crawl_depth") == 0:
            continue
        score = score_for(norm_url(page.url))
        if score is None or score >= median * ratio:
            continue
        ctx.add(
            "LOW_LINK_SCORE",
            target_url=page.url,
            details={
                "link_score": round(score, 6),
                "site_median": round(median, 6),
                "ratio_to_median": round(score / median, 3),
            },
        )


def check_link_score(ctx: AuditContext) -> None:
    """LOW_LINK_SCORE — an iterative internal-PageRank pass (issue #15, item 1).

    One new edge changes every page's score, so this can only run once the
    crawl's own edge list is complete. It reads the same ``all_inlinks``
    export the checks below share, restricted to internal, followed hyperlink
    edges, and flags indexable pages scoring far below the site's own median
    — a signal a raw inlink *count* misses, since a page can hold several
    inlinks and still be starved of link equity if every one is nofollow or
    itself comes from a poorly linked page.
    """
    records = _all_inlink_records(ctx)
    if records is None:
        graph = _graph_access(ctx)
        if graph is not None:
            stats = graph.link_score(
                damping=DEFAULT_DAMPING,
                max_iterations=DEFAULT_MAX_ITERATIONS,
                tolerance=DEFAULT_TOLERANCE,
            )
            if stats is None:
                ctx.skip("LOW_LINK_SCORE", "all_inlinks export has no internal followed hyperlinks")
                return
            _emit_link_scores(ctx, stats.count, stats.median, stats.score_for)
            return
        ctx.skip(
            "LOW_LINK_SCORE", "no all_inlinks export (needed for the complete internal edge list)"
        )
        return
    site_host = _site_host(ctx)
    edges = _internal_hyperlink_edges(records, site_host)
    if not edges:
        ctx.skip("LOW_LINK_SCORE", "all_inlinks export has no internal followed hyperlinks")
        return

    urls = {norm_url(p.url) for p in ctx.pages}
    scores = compute_link_scores(edges, urls)
    values = sorted(scores.values())
    _emit_link_scores(ctx, len(values), statistics.median(values) if values else 0.0, scores.get)


def check_inlink_composition(ctx: AuditContext) -> None:
    """ONLY_NOFOLLOW_INLINKS / ONLY_NONINDEXABLE_SOURCE_INLINKS.

    Both are aggregates over *every* inlink a URL has: "only nofollow" is
    false the moment one more, follow inlink turns up, and the same is true of
    "only from non-indexable sources" — so neither is provable before the
    crawl (and the complete ``all_inlinks`` export) is finished (issue #15,
    item 7). Crawl depth, the aggregate item 7 also names, is already answered
    by ``DEEP_CRAWL_DEPTH`` from Internal:All's own Crawl Depth column, which
    Screaming Frog only finalizes once the crawl ends.
    """
    records = _all_inlink_records(ctx)
    if records is None:
        graph = _graph_access(ctx)
        if graph is not None:

            def source_indexable(source: str) -> bool | None:
                page = ctx.page_by_norm.get(norm_url(source))
                return page.is_indexable if page is not None else None

            _emit_inlink_composition(ctx, graph.iter_inlink_composition(source_indexable, 20))
            return
        for check_id in ("ONLY_NOFOLLOW_INLINKS", "ONLY_NONINDEXABLE_SOURCE_INLINKS"):
            ctx.skip(check_id, "no all_inlinks export (needed for the complete inlink list)")
        return

    site_host = _site_host(ctx)
    # #313: group by the fragment-free page identity, not the raw destination.
    # A crawled page is one row in ctx.page_by_norm keyed by norm_url(page.url),
    # which never carries a fragment; norm_url(dest) alone still does (#202), so
    # "/target" and "/target#details" grouped by raw dest formed two composition
    # buckets and the fragment-bearing one could never resolve to the crawled
    # page, silently dropping its nofollow/nonindexable-source evidence. Each
    # record keeps its own raw destination_url below — only the grouping key is
    # defragmented, and neither norm_url() nor CANONICAL_FRAGMENT are touched.
    by_dest: OrderedDict[str, list[dict[str, Any]]] = OrderedDict()
    for rec in records:
        source, dest = rec.get("source_url"), rec.get("destination_url")
        if not source or not dest:
            continue
        link_type = rec.get("type")
        if link_type is not None and str(link_type).strip().lower() != "hyperlink":
            continue
        dest_host = urllib.parse.urlparse(dest).netloc.lower()
        if dest_host and dest_host != site_host:
            continue  # external destination — not this page's own composition
        dest_key = urllib.parse.urldefrag(norm_url(dest))[0]
        by_dest.setdefault(dest_key, []).append(rec)

    def export_rows():
        for dest_key, links in by_dest.items():
            follows = [
                is_true(link["follow"]) if link.get("follow") is not None else True
                for link in links
            ]
            source_indexability = []
            for link in links:
                source_page = ctx.page_by_norm.get(norm_url(link.get("source_url")))
                if source_page is not None:
                    source_indexability.append(source_page.is_indexable)
            yield InlinkCompositionRow(
                destination_key=dest_key,
                occurrences_count=len(links),
                all_nofollow=not any(follows),
                has_known_source=bool(source_indexability),
                has_indexable_source=any(source_indexability),
                source_examples=sorted({link["source_url"] for link in links})[:20],
            )

    _emit_inlink_composition(ctx, export_rows())


def _emit_inlink_composition(ctx: AuditContext, rows) -> None:
    """Apply the shared target eligibility, thresholds and issue payloads."""
    for row in rows:
        destination_key = row.destination_key
        occurrences = row.occurrences_count
        all_nofollow = row.all_nofollow
        known_source = row.has_known_source
        indexable_source = row.has_indexable_source
        sources = row.source_examples
        # #176 audit: correct by construction, same reasoning as check_unlinked_canonical —
        # is_indexable is a property of the live page, so the 2xx-preferring representative
        # is the variant "is this destination's inlink composition worth flagging" means.
        # The per-source lookup at the bottom of this loop resolves each source individually,
        # not a shared-key group, so the same single-representative read is simply correct.
        # destination_key is already norm_url() with the fragment stripped (#313), and
        # ctx.page_by_norm's keys are norm_url(page.url) for URLs that never carry
        # a fragment, so it is the correct lookup key as-is.
        target = ctx.page_by_norm.get(destination_key)
        if target is None or not target.is_indexable:
            continue
        if _rec(target).get("crawl_depth") == 0:
            continue  # the homepage's inlink composition is not the whole story

        if all_nofollow:
            ctx.add(
                "ONLY_NOFOLLOW_INLINKS",
                target_url=target.url,
                occurrences_count=occurrences,
                details={"inlink_count": occurrences, "sources": sorted(sources)[:20]},
            )

        if known_source and not indexable_source:
            ctx.add(
                "ONLY_NONINDEXABLE_SOURCE_INLINKS",
                target_url=target.url,
                occurrences_count=occurrences,
                details={
                    "inlink_count": occurrences,
                    "sources": sorted(sources)[:20],
                },
            )


def _path_endpoint(ctx: AuditContext, key: str) -> str:
    page = ctx.page_by_norm.get(key)
    return page.url if page is not None else key


def _emit_discovery_paths(
    ctx: AuditContext,
    path_for,
    depths: Mapping[str, int] | Callable[[str], int | None],
    seed: str,
) -> None:
    """Emit the one reviewed DEEP_DISCOVERY_PATH threshold and issue shape."""
    max_depth = ctx.thresholds.get("crawl_depth_max", 4)
    depth_for = depths if callable(depths) else depths.get
    for page in ctx.indexable_html_pages():
        key = norm_url(page.url)
        depth = depth_for(key)
        if depth is None or depth <= max_depth:
            continue
        path_norm = path_for(key) if depth < _MAX_PATH_URLS else None
        details: dict[str, Any] = {"hops": depth}
        if path_norm is not None:
            details["path"] = [_path_endpoint(ctx, item) for item in path_norm]
        else:
            details.update(
                path_start=[_path_endpoint(ctx, seed)], path_end=[page.url], path_truncated=True
            )
        ctx.add(
            "DEEP_DISCOVERY_PATH",
            target_url=page.url,
            details=details,
        )


def check_discovery_path(ctx: AuditContext) -> None:
    """DEEP_DISCOVERY_PATH — the concrete shortest click path from the seed.

    Internal:All's Crawl Depth column already reports each page's distance
    from the start URL, but not the route that produces it, and only a
    breadth-first walk over the *finished* internal hyperlink graph can
    reconstruct one (issue #15, item 10). Flags indexable pages whose
    shortest route exceeds the same ``crawl_depth_max`` threshold
    ``DEEP_CRAWL_DEPTH`` already uses, this time with the actual path attached.
    """
    records = _all_inlink_records(ctx)
    if records is None:
        graph = _graph_access(ctx)
        if graph is not None:
            if not graph.has_internal_hyperlinks:
                ctx.skip("DEEP_DISCOVERY_PATH", "all_inlinks export has no internal hyperlinks")
                return
            seed = next((page for page in ctx.pages if _rec(page).get("crawl_depth") == 0), None)
            if seed is None:
                ctx.skip("DEEP_DISCOVERY_PATH", "no page at Crawl Depth 0 to use as the seed")
                return
            paths = graph.begin_paths(norm_url(seed.url))
            if paths is None:
                ctx.skip("DEEP_DISCOVERY_PATH", "all_inlinks export has no internal hyperlinks")
                return
            _emit_discovery_paths(ctx, paths.path_to, paths.depth_for, norm_url(seed.url))
            return
        ctx.skip(
            "DEEP_DISCOVERY_PATH",
            "no all_inlinks export (needed for the complete internal edge list)",
        )
        return
    site_host = _site_host(ctx)
    edges = _internal_hyperlink_edges(records, site_host)
    if not edges:
        ctx.skip("DEEP_DISCOVERY_PATH", "all_inlinks export has no internal hyperlinks")
        return

    seed = next((p for p in ctx.pages if _rec(p).get("crawl_depth") == 0), None)
    if seed is None:
        ctx.skip("DEEP_DISCOVERY_PATH", "no page at Crawl Depth 0 to use as the seed")
        return

    depths_raw, parents_raw = bfs_tree_from_seed(edges, norm_url(seed.url))
    # Native evidence and the SF-shaped record bridge can disagree only in
    # harmless URL spelling (notably a trailing slash).  The page view uses
    # norm_url keys, so normalize both sides of the predecessor tree once
    # before emission.  Without this, every lookup misses and the bounded
    # branch falls back to materializing the full route for every page.
    depths = {norm_url(key): depth for key, depth in depths_raw.items()}
    parents = {
        norm_url(key): (norm_url(parent) if parent is not None else None)
        for key, parent in parents_raw.items()
    }
    _emit_discovery_paths(
        ctx,
        lambda key: route_from_parents(parents, key),
        depths,
        norm_url(seed.url),
    )


# How many repeated (destination, anchor) pairs one DUPLICATE_INTERNAL_LINK finding
# lists. A template emitting one duplicated block repeats every link in it, and the
# first handful identify the block as surely as all two hundred would.
_MAX_DUPLICATE_REPEATS = 20

# The default click-depth floor, used when no config supplies one. Ten, not the
# four ``crawl_depth_max`` uses for DEEP_CRAWL_DEPTH: that threshold answers "is
# this deeper than the site's own depth budget", and a site with a long, healthy
# paginated archive trips it everywhere. This one answers the different, coarser
# question the depth histogram exists for -- which pages are so far from the start
# URL that navigation does not reach them at all.
DEFAULT_CLICK_DEPTH_FLOOR = 10

# How many URLs of a shortest route one finding quotes, and how many from each end
# when the route is longer. The archive that prompted this check has a 3 005-hop
# route to one article; pasting that into 23 742 findings is not evidence, it is a
# report nobody can open. The depth is stated in full either way, and the two ends
# identify the route -- the middle of a chain is more of the same link.
_MAX_PATH_URLS = 20
_PATH_END_URLS = 5


def _internal_hyperlink_records(
    records: list[dict[str, Any]], site_host: str
) -> list[dict[str, Any]]:
    """Rows describing a hyperlink to this site, nofollow ones included.

    ``_internal_hyperlink_edges`` above drops nofollow rows, and rightly: it feeds
    passes about link equity and reachability. This one feeds descriptions of the
    markup -- where links sit, which of them are written twice -- and a nofollow
    link is as much a part of the template as any other.
    """
    kept: list[dict[str, Any]] = []
    for rec in records:
        source, dest = rec.get("source_url"), rec.get("destination_url")
        if not source or not dest:
            continue
        link_type = rec.get("type")
        if link_type is not None and str(link_type).strip().lower() != "hyperlink":
            continue
        dest_host = urllib.parse.urlparse(dest).netloc.lower()
        if dest_host and dest_host != site_host:
            continue
        kept.append(rec)
    return kept


def click_depth_seed(ctx: AuditContext) -> tuple[str | None, str]:
    """The URL the click-depth walk starts from, or the reason there is none.

    The crawl's own recorded start URL is the only trustworthy seed. Crawl Depth 0
    is the fallback for a Screaming Frog export, which records no start URL of its
    own -- and it is only usable when exactly one page carries it. A sitemap-seeded
    native crawl records 0 for every seeded URL (33 471 of 40 920 pages on the site
    this was measured against), so picking "a page at depth 0" would silently walk
    from an arbitrary article and report a depth histogram of the wrong site.
    """
    start = getattr(ctx, "start_url", None)
    if start:
        return norm_url(start), ""
    roots = [page for page in ctx.pages if _rec(page).get("crawl_depth") == 0]
    if not roots:
        return None, (
            "the run recorded no crawl start URL and no page is at Crawl Depth 0, "
            "so there is no page to walk the link graph from"
        )
    if len(roots) > 1:
        return None, (
            f"the run recorded no crawl start URL and {len(roots)} pages carry Crawl "
            "Depth 0, so that column cannot identify where the crawl began"
        )
    return norm_url(roots[0].url), ""


def _emit_duplicate_links(ctx: AuditContext, groups) -> int:
    """One finding per repeated-link pattern: the pages that write the same links twice.

    A navigation link written twice on every page is one template defect. Pages that
    share the same repeats become one finding with their page count, so a site-wide
    menu does not appear once per URL. Returns the surplus.
    """
    surplus_total = 0
    buckets: OrderedDict[tuple, list] = OrderedDict()
    for group in groups:
        surplus_total += group.surplus_total
        pattern = tuple((r["destination"], r["anchor"], r["count"]) for r in group.repeats)
        page = ctx.page_by_norm.get(norm_url(group.source_url))
        bucket = buckets.setdefault(pattern, [group.surplus_total, []])
        bucket[1].append(page.url if page is not None else group.source_url)
    for pattern, (surplus, urls) in buckets.items():
        # No threshold here, and that is deliberate: "written more than once" is a
        # count, not a judgement calibrated against a number somebody chose.
        repeats = [{"destination": d, "anchor": a, "count": c} for d, a, c in pattern]
        ctx.add_site_wide(
            "DUPLICATE_INTERNAL_LINK",
            urls,
            {"surplus_links": surplus, "repeats": repeats},
            occurrences=surplus,
        )
    return surplus_total


def _emit_deep_click_depth(
    ctx: AuditContext,
    depths: Mapping[str, int] | Callable[[str], int | None],
    floor: int,
    path_for,
    seed: str,
    *,
    compact_routes: bool = False,
) -> None:
    """Flag indexable pages further from the start URL than the configured floor."""
    depth_for = depths if callable(depths) else depths.get
    for page in ctx.indexable_html_pages():
        depth = depth_for(norm_url(page.url))
        if depth is None or depth <= floor:
            continue
        details: dict[str, Any] = {
            "click_depth": depth,
            # Named in every finding: the verdict is "deeper than this number",
            # and the number is configuration, not a property of the site.
            "floor_used": floor,
            "threshold": "thresholds.click_depth_max",
        }
        # Do not rebuild a million-node predecessor chain just to render a
        # bounded finding sample.  The depth is the verdict; for a long route
        # retain its known endpoints and say that the middle was omitted.
        route = (
            path_for(norm_url(page.url)) if not compact_routes or depth < _MAX_PATH_URLS else None
        )
        if route:
            urls = [
                ctx.page_by_norm[item].url if item in ctx.page_by_norm else item for item in route
            ]
            if len(urls) <= _MAX_PATH_URLS:
                details["path"] = urls
            else:
                details["path_start"] = urls[:_PATH_END_URLS]
                details["path_end"] = urls[-_PATH_END_URLS:]
                details["path_truncated"] = True
        elif depth >= _MAX_PATH_URLS:
            details["path_start"] = [_path_endpoint(ctx, seed)]
            details["path_end"] = [page.url]
            details["path_truncated"] = True
        ctx.add("DEEP_CLICK_DEPTH", target_url=page.url, details=details)


def check_internal_link_graph(ctx: AuditContext) -> None:
    """DEEP_CLICK_DEPTH / DUPLICATE_INTERNAL_LINK, and the graph summary both read (#634).

    One pass over the complete edge inventory produces three things a per-edge
    check cannot: the distribution of edges by page position, how many edges are
    repeats of another edge, and how many clicks from the crawl's start URL each
    page is. The numbers land in ``ctx.internal_linking`` for the audit summary;
    the two checks above are the verdicts drawn from them.

    ``DEEP_CLICK_DEPTH`` is not ``DEEP_DISCOVERY_PATH`` with a different name.
    That check reads Screaming Frog's own Crawl Depth column to pick a seed and
    flags anything past ``crawl_depth_max``, the same budget ``DEEP_CRAWL_DEPTH``
    uses. This one refuses to start unless the run recorded where the crawl
    actually began (see ``click_depth_seed``), and its floor is the separate,
    coarser ``click_depth_max`` -- the depth past which navigation has stopped
    reaching a page at all, rather than the depth past which a site's own budget
    is exceeded.
    """
    floor = int(ctx.thresholds.get("click_depth_max", DEFAULT_CLICK_DEPTH_FLOOR))
    records = _all_inlink_records(ctx)
    graph = _graph_access(ctx) if records is None else None
    if records is None and graph is None:
        reason = "no all_inlinks export (needed for the complete internal edge list)"
        for check_id in ("DEEP_CLICK_DEPTH", "DUPLICATE_INTERNAL_LINK"):
            ctx.skip(check_id, reason)
        ctx.internal_linking = unmeasured(reason)
        return

    if graph is not None:
        if not graph.has_internal_hyperlinks:
            reason = "all_inlinks export has no internal hyperlinks"
            for check_id in ("DEEP_CLICK_DEPTH", "DUPLICATE_INTERNAL_LINK"):
                ctx.skip(check_id, reason)
            ctx.internal_linking = unmeasured(reason)
            return
        positions = summarize_positions(graph.position_totals())
        surplus = _emit_duplicate_links(ctx, graph.iter_duplicate_links(_MAX_DUPLICATE_REPEATS))
    else:
        internal = _internal_hyperlink_records(records, _site_host(ctx))
        if not internal:
            reason = "all_inlinks export has no internal hyperlinks"
            for check_id in ("DEEP_CLICK_DEPTH", "DUPLICATE_INTERNAL_LINK"):
                ctx.skip(check_id, reason)
            ctx.internal_linking = unmeasured(reason)
            return
        totals: Counter = Counter((rec.get("link_position") or "") for rec in internal)
        positions = summarize_positions(dict(totals))
        surplus = _emit_duplicate_links(
            ctx, _duplicate_groups_from_records(internal, _MAX_DUPLICATE_REPEATS)
        )

    ctx.internal_linking = {
        "measured": True,
        **positions,
        "duplicate_edges": surplus,
        "duplicate_fraction": (
            round(surplus / positions["edges_total"], 4) if positions["edges_total"] else 0.0
        ),
        "click_depth": _measure_click_depth(ctx, records, graph, floor),
    }


def _duplicate_groups_from_records(records: list[dict[str, Any]], max_repeats: int):
    """The export-side twin of ``AnalysisGraph.iter_duplicate_links``."""
    from seohead.core.graph import DuplicateLinkGroup

    by_source: OrderedDict[str, Counter] = OrderedDict()
    for rec in records:
        source = rec["source_url"]
        pair = (rec["destination_url"], (rec.get("anchor") or "").strip())
        by_source.setdefault(source, Counter())[pair] += 1
    for source, pairs in by_source.items():
        repeated = [(pair, count) for pair, count in pairs.items() if count > 1]
        if not repeated:
            continue
        repeated.sort(key=lambda item: (-item[1], item[0][0]))
        yield DuplicateLinkGroup(
            source,
            sum(count - 1 for _pair, count in repeated),
            [
                {"destination": destination, "anchor": anchor, "count": count}
                for (destination, anchor), count in repeated[:max_repeats]
            ],
        )


def open_click_depth_session(ctx: AuditContext, graph) -> tuple[Any, str | None, str]:
    """Begin the followed-graph walk from the crawl's start: ``(session, seed, reason)``.

    ``session`` is None, with ``reason`` set, when there is nothing to walk.
    """
    seed, reason = click_depth_seed(ctx)
    if seed is None:
        return None, None, reason
    session = graph.begin_paths(seed)
    if session is None:
        return None, seed, "all_inlinks export has no internal hyperlinks"
    return session, seed, ""


def _measure_click_depth(ctx: AuditContext, records, graph, floor: int) -> dict[str, Any]:
    """Walk the followed internal graph from the start URL; emit and describe."""
    if graph is not None:
        session, seed, reason = open_click_depth_session(ctx, graph)
        if session is None:
            ctx.skip("DEEP_CLICK_DEPTH", reason)
            return unmeasured(reason)
        depths = session.depth_for
        _emit_deep_click_depth(ctx, depths, floor, session.path_to, seed, compact_routes=True)
    else:
        seed, reason = click_depth_seed(ctx)
        if seed is None:
            ctx.skip("DEEP_CLICK_DEPTH", reason)
            return unmeasured(reason)
        edges = _internal_hyperlink_edges(records, _site_host(ctx))
        if not edges:
            reason = "all_inlinks export has no internal followed hyperlinks"
            ctx.skip("DEEP_CLICK_DEPTH", reason)
            return unmeasured(reason)
        # The parent tree, never the routes themselves: one entry per node instead
        # of one per hop of every route (see crawl_path.bfs_tree_from_seed).
        depths, parents = bfs_tree_from_seed(edges, seed)
        _emit_deep_click_depth(
            ctx, depths, floor, lambda key: route_from_parents(parents, key), seed
        )
    seed_page = ctx.page_by_norm.get(seed)
    return summarize_depth(
        depths,
        ctx.html_page_keys(),
        seed=seed_page.url if seed_page is not None else seed,
        floor=floor,
    )


# Rows Screaming Frog's All Inlinks export uses for a page's own directives
# rather than an actual fetched resource — never "insecure subresources".
_NON_RESOURCE_LINK_TYPES = frozenset(
    {"hyperlink", "canonical", "rel next", "rel prev", "meta refresh", "amphtml"}
)


def check_insecure_subresources(ctx: AuditContext) -> None:
    """INSECURE_SUBRESOURCE — an HTTPS page loads a subresource over HTTP.

    Needs the completed inventory of every page cross-joined with the
    resources it loads (issue #15, item 11): this reads the non-hyperlink
    rows (Image, JavaScript, CSS, ...) of the same ``all_inlinks`` export the
    checks above already use. A native Security:Mixed Content export answers
    the same question and is preferred when present (``check_native_exports``
    → ``MIXED_CONTENT``); this only fills the gap when that report was not
    exported, and skips rather than double-report when it was.
    """
    if ctx.exports.has("security_mixed"):
        ctx.skip("INSECURE_SUBRESOURCE", "native Security:Mixed Content export already covers this")
        return
    records = _all_inlink_records(ctx)
    if records is None:
        graph = _graph_access(ctx)
        if graph is not None:
            if graph.has_resource_type:
                by_source: OrderedDict[str, list[str]] = OrderedDict()
                for source, dest, link_type in graph.iter_resources():
                    if not source.lower().startswith("https://"):
                        continue
                    if str(link_type).strip().lower() in _NON_RESOURCE_LINK_TYPES:
                        continue
                    if dest.lower().startswith("http://"):
                        by_source.setdefault(source, []).append(dest)
                for source, resources in by_source.items():
                    ctx.add(
                        "INSECURE_SUBRESOURCE",
                        target_url=source,
                        occurrences_count=len(resources),
                        details={"resources": sorted(set(resources))[:20]},
                    )
                return
            ctx.skip(
                "INSECURE_SUBRESOURCE",
                "all_inlinks export has no Type column (needed to tell a resource from a hyperlink)",
            )
            return
        ctx.skip(
            "INSECURE_SUBRESOURCE", "no all_inlinks export (needed for the resource inventory)"
        )
        return
    if not any(rec.get("type") for rec in records):
        ctx.skip(
            "INSECURE_SUBRESOURCE",
            "all_inlinks export has no Type column (needed to tell a resource from a hyperlink)",
        )
        return

    by_source: OrderedDict[str, list[str]] = OrderedDict()
    for rec in records:
        source, dest = rec.get("source_url"), rec.get("destination_url")
        if not source or not dest or not source.lower().startswith("https://"):
            continue
        link_type = str(rec.get("type") or "").strip().lower()
        if link_type in _NON_RESOURCE_LINK_TYPES:
            continue
        if dest.lower().startswith("http://"):
            by_source.setdefault(source, []).append(dest)

    for source, resources in by_source.items():
        ctx.add(
            "INSECURE_SUBRESOURCE",
            target_url=source,
            occurrences_count=len(resources),
            details={"resources": sorted(set(resources))[:20]},
        )


# Screaming Frog types every All Inlinks row, and a page's own rel="next" /
# rel="prev" declarations are rows in it like any other link (the same Type
# values _NON_RESOURCE_LINK_TYPES above already names), one row per
# declaration. That export is also the only place PAGINATION_URL_NOT_IN_ANCHOR
# can be answered from at all, because it needs the page's anchors, not just
# its declarations -- Internal:All carries no anchor inventory whatever its
# column list covers.
#
# PAGINATION_MULTIPLE does not actually need that heavy export: SF numbers a
# repeated head element by occurrence rather than dropping it -- canonical /
# canonical_2 is that pair, and CANONICAL_MULTIPLE (check_canonical_extra)
# reads it straight off Internal:All -- and normalize.INTERNAL_FIELD_MAP now
# carries the same rel_next_2 / rel_prev_2 pair. Two declarations are enough
# to answer "did this page declare more than one successor", so the light
# check below reads Internal:All first and only asks the heavier All Inlinks
# export when that column is missing (an older SF profile, or a native crawl
# that does not fill occurrence-2 head columns at all).
_PAGINATION_LINK_TYPES = {"rel next": 'rel="next"', "rel prev": 'rel="prev"'}

_PAGINATION_DECLARATION_CHECKS = ("PAGINATION_MULTIPLE", "PAGINATION_URL_NOT_IN_ANCHOR")


def _pagination_declarations(
    records: list[dict[str, Any]],
) -> OrderedDict[str, OrderedDict[str, list[str]]]:
    """source URL -> relation -> declared destinations, in export order.

    Destinations are de-duplicated per relation **through** ``norm_url``, the
    same identity ``_anchor_destinations`` and the anchor half of
    ``check_pagination_declarations`` compare by. De-duplicating by raw string
    instead would make ``/blog/page/2`` and ``/blog/page/2/`` one successor to
    the anchor half and two to this one, and PAGINATION_MULTIPLE would then tell
    the operator a crawler must guess between two different successors when
    there is only one -- two plugins writing the same URL with different
    formatting is untidy markup, not an ambiguous series, and the row this feeds
    is about *which page comes next*.

    The first spelling seen is the one reported, so the finding quotes markup
    that is actually on the page rather than a normalized form of it.
    """
    seen: OrderedDict[str, OrderedDict[str, OrderedDict[str, str]]] = OrderedDict()
    for rec in records:
        source, dest = rec.get("source_url"), rec.get("destination_url")
        if not source or not dest:
            continue
        relation = _PAGINATION_LINK_TYPES.get(str(rec.get("type") or "").strip().lower())
        if relation is None:
            continue
        seen.setdefault(source, OrderedDict()).setdefault(relation, OrderedDict()).setdefault(
            norm_url(dest), dest
        )
    return OrderedDict(
        (source, OrderedDict((relation, list(t.values())) for relation, t in by_relation.items()))
        for source, by_relation in seen.items()
    )


def _anchor_destinations(records: list[dict[str, Any]]) -> dict[str, set[str]]:
    """source (normalized) -> every destination it reaches with a ``Hyperlink`` row.

    Unlike ``_internal_hyperlink_edges`` this keeps nofollow rows: the question
    here is whether an ``<a href>`` to the paginated URL exists on the page at
    all, and a nofollow anchor is still an anchor.
    """
    out: dict[str, set[str]] = {}
    for rec in records:
        if str(rec.get("type") or "").strip().lower() != "hyperlink":
            continue
        source, dest = rec.get("source_url"), rec.get("destination_url")
        if not source or not dest:
            continue
        out.setdefault(norm_url(source), set()).add(norm_url(dest))
    return out


def _light_pagination_multiple(ctx: AuditContext) -> bool:
    """PAGINATION_MULTIPLE from Internal:All's occurrence-2 columns alone.

    Mirrors ``check_canonical_extra``'s CANONICAL_MULTIPLE: the mere presence
    of a second occurrence is the finding, whatever its value happens to be.
    Returns whether the light columns exist at all, so the caller knows
    whether it can skip the heavier All Inlinks read for this check.
    """
    from .normalize import INTERNAL_FIELD_MAP, find_column

    if ctx.internal_df is None:
        return False
    has_light_column = any(
        find_column(ctx.internal_df, INTERNAL_FIELD_MAP[field]) is not None
        for field in ("rel_next_2", "rel_prev_2")
    )
    if not has_light_column:
        return False
    for page in ctx.html_pages():
        rec = _rec(page)
        for relation, first_field, second_field in (
            ('rel="next"', "rel_next", "rel_next_2"),
            ('rel="prev"', "rel_prev", "rel_prev_2"),
        ):
            first, second = rec.get(first_field), rec.get(second_field)
            if not first or not second:
                continue
            if norm_url(first) == norm_url(second):
                continue
            ctx.add(
                "PAGINATION_MULTIPLE",
                target_url=page.url,
                details={"relation": relation, "urls": [first, second]},
            )
    return True


def check_pagination_declarations(ctx: AuditContext) -> None:
    """PAGINATION_MULTIPLE / PAGINATION_URL_NOT_IN_ANCHOR — the declarations themselves.

    Both read the one export that carries every rel="next"/rel="prev"
    declaration beside every anchor on the same page, so both answer from the
    page's own row set rather than from a whole-site negative: unlike
    UNLINKED_PAGINATION_SERIES, "this page does not link its own successor" is
    provable on a partial crawl, because the page in question was crawled and
    its links are all here.

    Each precondition the pair can fail is declared by name rather than passed
    over: no export, no Type column to tell a declaration from an anchor, no
    declarations in it at all.
    """
    if ctx.scan_con is not None:
        from seohead.checks.parser import _rel_has
        from seohead.storage.head_links import active, eligible_count

        measured = 0
        partial = False
        for source, annotations in active(ctx.scan_con):
            page = ctx.page_by_url.get(source)
            if page is None or not page.is_html or not page.is_2xx:
                continue
            if annotations is None:
                continue
            measured += 1
            partial |= len(annotations) >= 100
            for relation in ("next", "prev"):
                targets = {
                    norm_url(row["target"]): row["target"]
                    for row in annotations
                    if row["target"] and _rel_has(relation)(row["rel"])
                }
                if len(targets) > 1:
                    ctx.add(
                        "PAGINATION_MULTIPLE",
                        target_url=source,
                        details={"relation": f'rel="{relation}"', "urls": list(targets.values())},
                    )
        if not measured:
            ctx.skip(
                "PAGINATION_MULTIPLE",
                "not measured by this native crawl: raw pagination annotations were not retained",
            )
        if partial or measured < eligible_count(ctx.scan_con):
            ctx.skip(
                "PAGINATION_MULTIPLE",
                "not measured completely by this native crawl: head annotations were unavailable or reached their limit",
            )
        ctx.skip(
            "PAGINATION_URL_NOT_IN_ANCHOR",
            "native head annotation and hyperlink comparison is not measured",
        )
        return

    # PAGINATION_MULTIPLE's light path is independent of every precondition
    # below -- it reads Internal:All's occurrence-2 columns, not All Inlinks --
    # so it runs first and, when it has the columns to answer from, PAGINATION_
    # MULTIPLE is no longer among the checks a missing/short All Inlinks export
    # skips.
    light_ran = _light_pagination_multiple(ctx)
    skip_checks = ("PAGINATION_URL_NOT_IN_ANCHOR",) if light_ran else _PAGINATION_DECLARATION_CHECKS

    records = _all_inlink_records(ctx)
    if records is None and _graph_access(ctx) is None:
        for check_id in skip_checks:
            ctx.skip(
                check_id,
                'no all_inlinks export (needed for every rel="next"/rel="prev" '
                "declaration and the anchors beside them)",
            )
        return
    # A native crawl arrives here by either route and neither carries a link
    # type: its All Inlinks projection leaves Type unset because the spider
    # records only <a href> hyperlinks (see crawl/evidence._inlinks_frame), and
    # the SQL-backed graph declares has_resource_type False. Both therefore
    # reach the same verdict as an export written without the column, which is
    # also what the two routes' parity contract requires of every check.
    if records is None or not any(rec.get("type") for rec in records):
        for check_id in skip_checks:
            ctx.skip(
                check_id,
                "the link inventory carries no link type (needed to tell a "
                'rel="next" declaration from an anchor)',
            )
        return
    declarations = _pagination_declarations(records)
    if not declarations:
        for check_id in skip_checks:
            ctx.skip(check_id, 'all_inlinks export contains no rel="next"/rel="prev" rows')
        return

    anchors = _anchor_destinations(records)
    if not anchors:
        # Declarations but not one Hyperlink row anywhere is a filtered export,
        # not a site with no links on it. Reporting every declaration as
        # un-anchored off that would be a whole crawl of wrong findings.
        ctx.skip(
            "PAGINATION_URL_NOT_IN_ANCHOR",
            "all_inlinks export contains no Hyperlink rows (needed for the page's anchors)",
        )

    for source, by_relation in declarations.items():
        if not light_ran:
            for relation, targets in by_relation.items():
                if len(targets) > 1:
                    ctx.add(
                        "PAGINATION_MULTIPLE",
                        target_url=source,
                        details={"relation": relation, "urls": targets},
                    )
        if not anchors:
            continue
        linked = anchors.get(norm_url(source), set())
        missing = [
            {"relation": relation, "url": target}
            for relation, targets in by_relation.items()
            for target in targets
            if norm_url(target) not in linked
        ]
        if missing:
            ctx.add(
                "PAGINATION_URL_NOT_IN_ANCHOR",
                target_url=source,
                occurrences_count=len(missing),
                details={"declared_without_an_anchor": missing},
            )


def run_inlinks(ctx: AuditContext) -> None:
    site_host = _site_host(ctx)
    for key, (internal_check, external_check) in INLINK_SOURCES.items():
        _process_export(ctx, key, internal_check, external_check, site_host)
    check_anchor_text(ctx)
    check_hreflang_targets(ctx)
    check_hreflang_unlinked_targets(ctx)
    check_hreflang_noindex_targets(ctx)
    check_hreflang_quality(ctx)
    check_hreflang_reciprocity(ctx)
    check_hreflang_confirmation_consistency(ctx)
    check_link_score(ctx)
    check_inlink_composition(ctx)
    check_discovery_path(ctx)
    check_insecure_subresources(ctx)
    check_pagination_declarations(ctx)
    # Last: it opens its own path session over the stored graph, and opening one
    # closes whichever session check_discovery_path above is still holding.
    check_internal_link_graph(ctx)
