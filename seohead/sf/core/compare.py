"""Diff two audits: what changed, not just what each one found separately.

The most repeated billable question in audit work is "did the developer
actually ship the fix", and a naive diff cannot answer it. A page that stopped
matching a finding because it was fixed, and a page that stopped matching
because it was deleted from the crawl entirely, look identical if you only
compare two sets of findings — and they mean opposite things.

So every finding lands in exactly one of four disjoint sets, keyed by the
finding's own fingerprint plus the URL it was found on:

    entered      the URL existed in both crawls; it did not match before, matches now
    left         the URL was fetched by both crawls; it matched before, does not match now
    appeared     the URL is new to this crawl, and matches now
    disappeared  not rechecked: the URL was not fetched by both crawls, so its
                 earlier finding cannot be proven fixed or still present

"left" is progress, and only a URL fetched by both crawls can earn it. A URL the
later crawl never fetched (a partial crawl, a budget stop) is "not rechecked":
not measuring a page is a different fact from fixing it, so it is never reported
as a fix. The key stays ``disappeared`` for compatibility.

An audit-wide finding (no ``target_url`` — e.g. TITLE_TEMPLATED, which
describes the crawl as a whole) has no page to appear or disappear, so it can
only ever land in "entered" or "left": the condition it describes now holds,
or it no longer does.
"""

from __future__ import annotations

import json
import threading
from collections.abc import Callable, Mapping
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit


class CompareError(ValueError):
    """Two audits that cannot be compared without lying about the result."""


_CORRESPONDENCE_SCHEMA = "url-correspondence.v1"
_RELEASE_REVIEW_SCHEMA = "release_review.v1"
_CORRESPONDENCE_KEYS = frozenset({"schema_version", "origin_map", "pairs"})
_PAIR_KEYS = frozenset({"before", "after"})


def _reject_duplicate_object_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    """Keep a JSON map from silently replacing a declared correspondence."""
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise CompareError(f"url correspondence repeats object key {key!r}")
        result[key] = value
    return result


def _origin(value: str, *, label: str) -> str:
    parsed = urlsplit(value)
    if (
        parsed.scheme not in {"http", "https"}
        or not parsed.netloc
        or parsed.path
        or parsed.query
        or parsed.fragment
        or parsed.username is not None
        or parsed.password is not None
    ):
        raise CompareError(
            f"{label} must be an http(s) origin without a path, query, fragment, or userinfo"
        )
    return f"{parsed.scheme}://{parsed.netloc}"


def _url(value: Any, *, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise CompareError(f"{label} must be a non-empty absolute http(s) URL")
    parsed = urlsplit(value)
    if (
        parsed.scheme not in {"http", "https"}
        or not parsed.netloc
        or parsed.username is not None
        or parsed.password is not None
    ):
        raise CompareError(f"{label} must be a non-empty absolute http(s) URL without userinfo")
    return value


def _load_correspondence(value: Any) -> dict[str, Any]:
    """Read the deliberately small, closed URL-pairing declaration.

    It is intentionally not a redirect matcher and never looks at page text or
    titles.  A release reviewer has to declare the migration relationship.
    """
    if isinstance(value, (str, Path)):
        try:
            with Path(value).open("rb") as stream:
                content = stream.read(16 * 1024 * 1024 + 1)
            if len(content) > 16 * 1024 * 1024:
                raise CompareError("url correspondence exceeds the 16 MiB declaration limit")
            raw = content.decode("utf-8")
            value = json.loads(raw, object_pairs_hook=_reject_duplicate_object_keys)
        except OSError as exc:
            raise CompareError(f"could not read url correspondence: {exc}") from exc
        except json.JSONDecodeError as exc:
            raise CompareError(f"url correspondence is not valid JSON: {exc.msg}") from exc
    if not isinstance(value, Mapping):
        raise CompareError("url correspondence must be an object or a JSON file path")
    if set(value) != _CORRESPONDENCE_KEYS:
        extra = sorted(set(value) - _CORRESPONDENCE_KEYS)
        missing = sorted(_CORRESPONDENCE_KEYS - set(value))
        details = []
        if extra:
            details.append(f"unknown keys: {', '.join(extra)}")
        if missing:
            details.append(f"missing keys: {', '.join(missing)}")
        raise CompareError("url correspondence has a closed schema (" + "; ".join(details) + ")")
    if value.get("schema_version") != _CORRESPONDENCE_SCHEMA:
        raise CompareError(f"url correspondence schema_version must be {_CORRESPONDENCE_SCHEMA!r}")

    raw_origins = value["origin_map"]
    if not isinstance(raw_origins, Mapping):
        raise CompareError("url correspondence origin_map must be an object")
    if len(raw_origins) > 10_000:
        raise CompareError("url correspondence exceeds 10000 origin declarations")
    origins: dict[str, str] = {}
    for before, after in raw_origins.items():
        if not isinstance(before, str) or not isinstance(after, str):
            raise CompareError("url correspondence origin_map keys and values must be strings")
        before_origin = _origin(before, label="origin_map key")
        after_origin = _origin(after, label="origin_map value")
        if before != before_origin or after != after_origin:
            raise CompareError(
                "url correspondence origins must be written without a trailing slash"
            )
        origins[before_origin] = after_origin

    raw_pairs = value["pairs"]
    if not isinstance(raw_pairs, list):
        raise CompareError("url correspondence pairs must be a list")
    if len(raw_pairs) > 10_000:
        raise CompareError(
            "url correspondence exceeds 10000 explicit pairs; use origin_map for an origin migration"
        )
    pairs: dict[str, str] = {}
    reverse_pairs: dict[str, str] = {}
    for index, raw_pair in enumerate(raw_pairs):
        if not isinstance(raw_pair, Mapping) or set(raw_pair) != _PAIR_KEYS:
            raise CompareError(
                f"url correspondence pairs[{index}] must contain exactly before and after"
            )
        before = _url(raw_pair.get("before"), label=f"pairs[{index}].before")
        after = _url(raw_pair.get("after"), label=f"pairs[{index}].after")
        if before in pairs:
            raise CompareError(f"url correspondence maps {before!r} more than once")
        if after in reverse_pairs:
            raise CompareError(
                f"url correspondence maps both {reverse_pairs[after]!r} and {before!r} to {after!r}"
            )
        pairs[before] = after
        reverse_pairs[after] = before
    return {"origin_map": origins, "pairs": pairs}


def _mapped_url(url: str, correspondence: Mapping[str, Any]) -> tuple[str | None, str | None]:
    """Return one declared destination and its source, never an inferred one."""
    explicit = correspondence["pairs"].get(url)
    origin = f"{urlsplit(url).scheme}://{urlsplit(url).netloc}" if urlsplit(url).netloc else None
    target_origin = correspondence["origin_map"].get(origin) if origin else None
    origin_target = f"{target_origin}{url[len(origin) :]}" if target_origin and origin else None
    if explicit is not None:
        return explicit, "explicit_pair"
    if origin_target is not None:
        return origin_target, "origin_map"
    return None, None


def _page_rows(audit: Any) -> dict[str, dict[str, Any]]:
    rows: dict[str, dict[str, Any]] = {}
    for page in _iter_rows(audit, "pages"):
        url = page.get("url")
        if not url:
            continue
        if url in rows:
            raise CompareError(f"url correspondence cannot pair duplicate page URL {url!r}")
        rows[url] = page
    return rows


def _host_changed(before: str, after: str) -> bool:
    return urlsplit(before).hostname != urlsplit(after).hostname


def _resolve_correspondence(
    before: Any, after: Any, declaration: Mapping[str, Any]
) -> dict[str, Any]:
    """Bind declared pairs to the retained page rows and expose every miss."""
    before_pages = _page_rows(before)
    after_pages = _page_rows(after)
    resolved: dict[str, str] = {}
    pair_rows: list[dict[str, Any]] = []
    seen_after: dict[str, str] = {}
    for before_url in sorted(before_pages):
        after_url, kind = _mapped_url(before_url, declaration)
        if after_url is None:
            continue
        row = {
            "before_url": before_url,
            "after_url": after_url,
            "correspondence_key": after_url,
            "kind": kind,
            "before_origin": f"{urlsplit(before_url).scheme}://{urlsplit(before_url).netloc}",
            "after_origin": f"{urlsplit(after_url).scheme}://{urlsplit(after_url).netloc}",
            "host_changed": _host_changed(before_url, after_url),
        }
        if after_url not in after_pages:
            row["state"] = "after_not_crawled"
            pair_rows.append(row)
            continue
        other_before = seen_after.get(after_url)
        if other_before is not None and other_before != before_url:
            raise CompareError(
                f"url correspondence maps both {other_before!r} and {before_url!r} to crawled page {after_url!r}"
            )
        seen_after[after_url] = before_url
        resolved[before_url] = after_url
        row["state"] = "matched"
        pair_rows.append(row)

    # Explicit entries outside the saved baseline remain visible rather than
    # disappearing from a review just because one crawl did not contain them.
    declared_before = {row["before_url"] for row in pair_rows}
    for before_url, after_url in sorted(declaration["pairs"].items()):
        if before_url in declared_before:
            continue
        pair_rows.append(
            {
                "before_url": before_url,
                "after_url": after_url,
                "correspondence_key": after_url,
                "kind": "explicit_pair",
                "before_origin": f"{urlsplit(before_url).scheme}://{urlsplit(before_url).netloc}",
                "after_origin": f"{urlsplit(after_url).scheme}://{urlsplit(after_url).netloc}",
                "host_changed": _host_changed(before_url, after_url),
                "state": "before_not_crawled",
            }
        )
    if len({resolved.get(url, url) for url in before_pages}) != len(before_pages):
        raise CompareError("url correspondence has a page collision with an unmapped baseline URL")
    return {
        "before_to_after": resolved,
        "origin_map": dict(declaration["origin_map"]),
        "pairs": pair_rows,
        "before_pages": before_pages,
        "after_pages": after_pages,
    }


def _mapped_issues(
    audit: Any, before_to_after: Mapping[str, str] | None = None
) -> dict[tuple[str, str], dict[str, Any]]:
    records: dict[tuple[str, str], dict[str, Any]] = {}
    for issue in _iter_rows(audit, "issues"):
        key = _key(issue)
        if before_to_after and key[1] in before_to_after:
            key = (key[0], before_to_after[key[1]])
        if key in records:
            raise CompareError(f"comparison has duplicate finding key {key!r}")
        records[key] = issue
    return records


def _fact(value: Any, present: bool, source: str) -> dict[str, Any]:
    return {
        "value": value if present else None,
        "state": "measured"
        if present and value is not None
        else "absent"
        if present
        else "unavailable",
        "source": source,
        "coverage": "recorded" if present else "unavailable",
    }


def _page_facts(page: Mapping[str, Any]) -> dict[str, dict[str, Any]]:
    metrics = page.get("metrics") if isinstance(page.get("metrics"), Mapping) else {}
    facts = {
        "status": _fact(page.get("status_code"), "status_code" in page, "pages.status_code"),
        "title": _fact(metrics.get("title"), "title" in metrics, "pages.metrics.title"),
        "description": _fact(
            metrics.get("meta_description"),
            "meta_description" in metrics,
            "pages.metrics.meta_description",
        ),
        "h1": _fact(metrics.get("h1"), "h1" in metrics, "pages.metrics.h1"),
        "canonical": _fact(
            metrics.get("canonical"), "canonical" in metrics, "pages.metrics.canonical"
        ),
    }
    robots_present = "meta_robots" in metrics or "x_robots" in metrics
    robots = {"meta": metrics.get("meta_robots"), "x_robots": metrics.get("x_robots")}
    facts["robots"] = _fact(robots, robots_present, "pages.metrics.meta_robots,x_robots")
    return facts


def _release_review(
    before: Any, after: Any, resolved: Mapping[str, Any], result: Mapping[str, Any], force: bool
) -> dict[str, Any]:
    facts: list[dict[str, Any]] = []
    for pair in resolved["pairs"]:
        if pair["state"] != "matched":
            continue
        before_facts = _page_facts(resolved["before_pages"][pair["before_url"]])
        after_facts = _page_facts(resolved["after_pages"][pair["after_url"]])
        facts.append(
            {
                "before_url": pair["before_url"],
                "after_url": pair["after_url"],
                "correspondence_key": pair["correspondence_key"],
                "kind": pair["kind"],
                "host_changed": pair["host_changed"],
                "before": before_facts,
                "after": after_facts,
                "changed": sorted(
                    name
                    for name in before_facts
                    if before_facts[name]["state"] != after_facts[name]["state"]
                    or before_facts[name]["value"] != after_facts[name]["value"]
                ),
            }
        )
    return {
        "schema_version": _RELEASE_REVIEW_SCHEMA,
        "provenance": {
            "before_generated_at": _run(before).get("generated_at"),
            "after_generated_at": _run(after).get("generated_at"),
            "force": force,
            "warnings": result["warnings"],
            "compatibility": result["compatibility"],
        },
        "correspondence": {
            "schema_version": _CORRESPONDENCE_SCHEMA,
            "origin_map": resolved["origin_map"],
            "pairs": resolved["pairs"],
        },
        "facts": facts,
        "findings": {name: result[name] for name in ("entered", "left", "appeared", "disappeared")},
        "summary": result["summary"],
    }


def _key(issue: dict[str, Any]) -> tuple[str, str]:
    """(check, target_url) — the same finding on the same page, across runs.

    Not the fingerprint alone: the fingerprint already folds in target_url, so
    this is equivalent, but naming both parts keeps the four sets legible. An
    audit-wide issue has no target_url and keys on "" — that is never a real
    URL, so it cannot collide with a page-level finding of the same check.
    """
    return (issue.get("check", ""), str(issue.get("target_url") or ""))


def _url_set_lists(
    before_urls: set[str], after_urls: set[str], *, unproven: bool
) -> dict[str, Any]:
    """Pages crawled in only one side. Partial crawls make absence unproven (#212, #458)."""
    only_before = sorted(before_urls - after_urls)
    only_after = sorted(after_urls - before_urls)
    return {
        "only_in_before": only_before,
        "only_in_after": only_after,
        "counts": {"only_in_before": len(only_before), "only_in_after": len(only_after)},
        "unproven": unproven,
    }


def _crawled_urls(audit: dict[str, Any]) -> set[str]:
    urls: set[str] = set()
    for page in _iter_rows(audit, "pages"):
        if page.get("url"):
            if page["url"] in urls:
                raise CompareError(f"comparison has duplicate page URL {page['url']!r}")
            urls.add(page["url"])
    return urls


def _by_key(audit: dict[str, Any]) -> dict[tuple[str, str], dict[str, Any]]:
    """Every issue, keyed for diffing — including audit-wide ones (issue #213).

    Dropping issues with no target_url here made a real TITLE_TEMPLATED delta
    vanish from every bucket and the summary; compare must account for a
    finding it was actually given, not just the ones that name a page.
    """
    return _mapped_issues(audit)


def _header(audit: Any) -> dict[str, Any]:
    return audit.header if hasattr(audit, "iter_collection") else audit


def _iter_rows(audit: Any, name: str):
    if hasattr(audit, "iter_collection"):
        pointer = f"/{name}"
        if pointer not in audit.collections:
            raise CompareError(f"audit.v2 is missing its {name} collection")
        return audit.iter_collection(pointer)
    return audit.get(name, [])


def _run(audit: Any) -> dict[str, Any]:
    return _header(audit).get("run", {})


def _measurement_gaps(before: Any, after: Any) -> list[dict[str, Any]]:
    """Keep missing check execution distinct from a measured finding delta."""
    gaps = []
    for side, source in (("before", before), ("after", after)):
        for field in ("checks_skipped", "checks_disabled"):
            for item in _run(source).get(field) or []:
                if isinstance(item, Mapping) and isinstance(item.get("id"), str):
                    gaps.append(
                        {
                            "side": side,
                            "check": item["id"],
                            "state": field.removeprefix("checks_"),
                            "reason": item.get("reason") or "no measurement available",
                        }
                    )
    return gaps


def preflight(before: Any, after: Any) -> list[str]:
    """Reasons a comparison would mislead, without refusing outright.

    A caller decides whether to proceed; this only says what to distrust.
    """
    warnings: list[str] = []
    for label, source in (("before", before), ("after", after)):
        audit = _header(source)
        if audit.get("run", {}).get("crawl_valid") is False:
            warnings.append(f"{label} crawl is marked invalid — it measured nothing usable")
    # A partial baseline and a partial current crawl poison opposite buckets, not
    # the same one (issue #212): a page the before crawl never reached looks
    # brand new to it, so "appeared" — not "disappeared" — is the bucket that
    # baseline can no longer prove; symmetrically, a page the after crawl never
    # reached looks gone to it, so "disappeared" is what that side poisons.
    if _run(before).get("crawl_partial"):
        warnings.append(
            "before crawl is partial — an 'appeared' finding may only mean the before "
            "crawl did not reach that URL, not that the URL is new"
        )
    if _run(after).get("crawl_partial"):
        warnings.append(
            "after crawl is partial — a 'disappeared' finding is not rechecked (the after "
            "crawl did not fetch its URL), so it is not counted as fixed"
        )
    before_cfg = _header(before).get("run", {}).get("crawl_config")
    after_cfg = _header(after).get("run", {}).get("crawl_config")
    # A missing crawl_config is an unknown comparison basis, not an established match --
    # the same distinction crawl_partial already draws above (issue #287). Only a native
    # crawl currently records this manifest, so an SF-derived audit reaches here with
    # none at all; silently treating "no manifest" as "same manifest" let a native-versus-
    # SF comparison classify same-URL finding changes as fixes or regressions when they
    # may just be two different tools measuring the same URL under different rules.
    if before_cfg is None or after_cfg is None:
        missing = [
            label for label, cfg in (("before", before_cfg), ("after", after_cfg)) if cfg is None
        ]
        warnings.append(
            f"{' and '.join(missing)} recorded no crawl configuration — comparability is "
            "unknown, not equal, so some of the difference may be the configuration rather "
            "than the site"
        )
    elif before_cfg != after_cfg:
        changed = sorted(
            k for k in set(before_cfg) | set(after_cfg) if before_cfg.get(k) != after_cfg.get(k)
        )
        warnings.append(
            "results-affecting settings differ between the two runs, so some of the "
            f"difference may be the configuration rather than the site: {', '.join(changed)}"
        )
    # crawl_config only exists for a native crawl; an SF-derived audit's nearest
    # equivalent today is its export profile (full/lite/...), which changes which
    # checks even had evidence to fire. Known profiles that differ are evidence of a
    # comparability gap even when neither side has a full effective manifest.
    before_profile = _header(before).get("run", {}).get("profile")
    after_profile = _header(after).get("run", {}).get("profile")
    if before_profile is not None and after_profile is not None and before_profile != after_profile:
        warnings.append(
            "SF export profile differs between the two runs "
            f"({before_profile!r} vs {after_profile!r}), so some of the difference may be "
            "the profile's check coverage rather than the site"
        )
    for gap in _measurement_gaps(before, after):
        warnings.append(
            f"{gap['side']} {gap['check']} was {gap['state']}: {gap['reason']}; "
            "finding differences are unverified where this check was not measured"
        )
    return warnings


def compare(
    before: Any,
    after: Any,
    *,
    force: bool = False,
    correspondence: Any = None,
    out_dir: str | Path | None = None,
    compression: str = "none",
    resume: bool = False,
    progress: Callable[[Any], None] | None = None,
    cancel_event: threading.Event | None = None,
) -> dict[str, Any]:
    """Diff two audit.json documents into the four sets, per check.

    Both documents must carry ``pages`` and ``issues`` in the shape this
    toolkit produces; a document missing either is refused by name rather than
    silently treated as empty, because an empty crawl and an unreadable one
    must not look the same in the result. A known difference between the two
    effective crawl configurations is also refused unless the caller passes
    ``force=True``: a changed robots policy or URL budget is not evidence that
    the site itself changed. Warnings about a partial crawl remain result data;
    they do not erase the historical observations or recategorize them.
    """
    if compression not in {"none", "gzip"}:
        raise CompareError("comparison compression must be none or gzip")
    if resume and out_dir is None:
        raise CompareError("comparison resume requires out_dir")
    if compression != "none" and out_dir is None:
        raise CompareError("comparison compression requires out_dir")
    before_source, after_source = before, after
    for label, source in (("before", before), ("after", after)):
        audit = _header(source)
        if "pages" not in audit or "issues" not in audit:
            raise CompareError(f"{label} is not an audit.json document (missing pages or issues)")

    before = _header(before)
    after = _header(after)
    before_cfg = before.get("run", {}).get("crawl_config")
    after_cfg = after.get("run", {}).get("crawl_config")
    if before_cfg is not None and after_cfg is not None and before_cfg != after_cfg and not force:
        changed = sorted(
            key
            for key in set(before_cfg) | set(after_cfg)
            if before_cfg.get(key) != after_cfg.get(key)
        )
        raise CompareError(
            "results-affecting settings differ between the two runs: "
            f"{', '.join(changed)}; pass force=True only when this comparison is intended"
        )

    if out_dir is not None:
        from .compare_store import compare_to_files

        return compare_to_files(
            before_source,
            after_source,
            out_dir=out_dir,
            force=force,
            correspondence=correspondence,
            compression=compression,
            resume=resume,
            progress=progress,
            cancel_event=cancel_event,
        )
    for source in (before_source, after_source):
        if hasattr(source, "iter_collection") and any(
            source.collections.get(f"/{name}", 0) > 10_000 for name in ("pages", "issues")
        ):
            raise CompareError(
                "large audit.v2 comparison requires out_dir for complete file output"
            )

    resolved: dict[str, Any] | None = None
    if correspondence is None:
        before_urls = _crawled_urls(before_source)
        before_issues = _by_key(before_source)
    else:
        resolved = _resolve_correspondence(
            before_source, after_source, _load_correspondence(correspondence)
        )
        before_urls = {
            resolved["before_to_after"].get(url, url) for url in _crawled_urls(before_source)
        }
        before_issues = _mapped_issues(before_source, resolved["before_to_after"])
    after_urls = _crawled_urls(after_source)
    after_issues = _by_key(after_source) if correspondence is None else _mapped_issues(after_source)
    # A partial baseline cannot prove a URL it never reached is genuinely new —
    # only that it did not see it (issue #212). Without this, every finding on
    # a URL outside the truncated baseline is misreported as "appeared".
    before_partial = bool(_run(before).get("crawl_partial"))
    # A partial after-crawl leaves its unreached URLs unproven (issue #458); they
    # are reported as not rechecked, never as fixed.
    after_partial = bool(_run(after).get("crawl_partial"))

    entered: list[dict[str, Any]] = []
    left: list[dict[str, Any]] = []
    appeared: list[dict[str, Any]] = []
    disappeared: list[dict[str, Any]] = []

    all_keys = set(before_issues) | set(after_issues)
    for key in all_keys:
        url = key[1]
        in_before = key in before_issues
        in_after = key in after_issues

        if in_before and in_after:
            continue  # unchanged: matched in both, not a difference

        # An audit-wide finding (no target_url, key[1] == "") describes the
        # crawl as a whole rather than a page, so it has no page-presence to
        # test and can only enter or leave (issue #213) — never appear or
        # disappear, which both assert something about a URL's existence.
        if not url:
            if in_after and not in_before:
                entered.append(dict(after_issues[key]))
            else:
                left.append(dict(before_issues[key]))
            continue

        url_in_before_crawl = url in before_urls
        url_in_after_crawl = url in after_urls

        if in_after and not in_before:
            record = dict(after_issues[key])
            if url_in_before_crawl or before_partial:
                entered.append(record)  # existed before, is a new finding now
            else:
                appeared.append(record)  # the URL itself is new to this crawl
        elif in_before and not in_after:
            record = dict(before_issues[key])
            if url_in_before_crawl and url_in_after_crawl:
                left.append(record)  # fetched by both crawls, no longer matches — a fix
            else:
                disappeared.append(record)  # not rechecked: not fetched by both crawls

    def _sort(items: list[dict[str, Any]]) -> list[dict[str, Any]]:
        return sorted(items, key=lambda i: (i.get("check", ""), str(i.get("target_url") or "")))

    by_check: dict[str, dict[str, int]] = {}
    for bucket_name, bucket in (
        ("entered", entered),
        ("left", left),
        ("appeared", appeared),
        ("disappeared", disappeared),
    ):
        for item in bucket:
            row = by_check.setdefault(
                item["check"], {"entered": 0, "left": 0, "appeared": 0, "disappeared": 0}
            )
            row[bucket_name] += 1

    # The established warning text remains the compatibility surface for older
    # callers.  The additive rows preserve the individual bases (scope,
    # configuration, representation, saved corpus and provider) so a report
    # cannot flatten an unknown basis into an apparent apples-to-apples diff.
    from .evidence_contract import comparison_compatibility, comparison_warnings

    result = {
        "schema_version": "compare.v1",
        "before": {
            "generated_at": _run(before_source).get("generated_at"),
            "urls_crawled": len(before_urls),
        },
        "after": {
            "generated_at": _run(after_source).get("generated_at"),
            "urls_crawled": len(after_urls),
        },
        "warnings": list(
            dict.fromkeys(preflight(before, after) + comparison_warnings(before, after))
        ),
        "compatibility": comparison_compatibility(before, after),
        "summary": {
            "entered": len(entered),
            "left": len(left),
            "appeared": len(appeared),
            "disappeared": len(disappeared),
            "by_check": by_check,
        },
        "entered": _sort(entered),
        "left": _sort(left),
        "appeared": _sort(appeared),
        "disappeared": _sort(disappeared),
        "url_sets": _url_set_lists(
            before_urls, after_urls, unproven=before_partial or after_partial
        ),
    }
    gaps = _measurement_gaps(before_source, after_source)
    if gaps:
        result["measurement_gaps"] = gaps
    if resolved is not None:
        result["release_review"] = _release_review(
            before_source, after_source, resolved, result, force
        )
    return result
