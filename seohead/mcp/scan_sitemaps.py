"""Persist selected sitemap expansions before native collection starts."""

from __future__ import annotations

from collections.abc import Callable, Iterable
from typing import Any
from urllib.parse import urlsplit, urlunsplit

from seohead.checks.sitemap import MAX_SITEMAPS, normalize_url
from seohead.crawl.sitemap_capture import SourceRoot, capture_declared_roots
from seohead.storage import ScanError


def initial_sitemaps(
    explicit: str | None, *, source: str = "explicit"
) -> tuple[tuple[str, str], ...]:
    return ((normalize_url(explicit), source),) if explicit else ()


def _conventional_sitemap(start_url: str) -> str:
    parts = urlsplit(start_url)
    return normalize_url(urlunsplit((parts.scheme, parts.netloc, "/sitemap.xml", "", "")))


def load_sitemaps(
    scan: Any,
    emit_seeds: Callable[[Iterable[str]], None],
    *,
    settings: dict[str, Any],
    result: dict[str, Any],
    request_gate: Callable[[], None] | None = None,
    proxy_route=None,
    start_url: str | None = None,
) -> None:
    selected = scan.sitemap_roots()
    if settings["sitemaps"]["auto_discover"] and not any(
        root["source"] == "explicit" for root in selected
    ):
        robots = scan.read_context("robots_summary")
        if not robots or robots["fetch_state"] != "fetched":
            scan.note_audit_unavailable("robots sitemap discovery was unavailable")
            return
        # Replay the complete saved declaration sequence after an interruption
        # between root transactions. Exact retries are idempotent.
        # This set is bounded by the selected-root cap, never the graph or its URL members.
        selected_keys: set[str] = set()
        for url in robots["parsed"]["sitemaps"]:
            key = normalize_url(url)
            if key in selected_keys:
                continue
            if len(selected_keys) >= MAX_SITEMAPS:
                raise ScanError("selected sitemap root limit reached")
            selected_keys.add(key)
            scan.declare_sitemap(key, "robots", len(selected_keys) - 1)
        # robots.txt that declares no Sitemap falls back to the conventional /sitemap.xml.
        if (
            not selected_keys
            and start_url
            and not any(root["source"] == "fallback" for root in selected)
        ):
            scan.declare_sitemap(_conventional_sitemap(start_url), "fallback", 0)
        selected = scan.sitemap_roots()
    result.update(
        sitemap_url=selected[0]["url"] if selected else None,
        sitemap_urls=[root["url"] for root in selected],
        declared=[],  # Membership is in SQLite; there is no second URL-list owner.
    )
    pending: list[str] = []
    declared_count = 0

    def seed(url: str, _root: SourceRoot) -> None:
        nonlocal declared_count
        declared_count += 1
        if (
            any(root["source"] == "sitemap-only" for root in selected)
            and settings["limits"]["max_urls"]
            and declared_count > settings["limits"]["max_urls"]
        ):
            raise ScanError("sitemap-only population exceeds the configured URL budget")
        pending.append(url)
        if len(pending) >= 256:
            emit_seeds(pending)
            pending.clear()

    outcomes = capture_declared_roots(
        [SourceRoot(root["sitemap_url_id"], root["url"], root["source"]) for root in selected],
        write_sitemap_members=scan.write_sitemap_members,
        finish_sitemap=scan.finish_sitemap,
        emit_seed=seed,
        read_sitemap_summary=lambda sid: scan.read_context("sitemap_fetch_summary", f"url:{sid}"),
        read_sitemap_members=scan.iter_sitemap_members,
        request_gate=request_gate,
        proxy_route=proxy_route,
    )
    if pending:
        emit_seeds(pending)
    # A conventional /sitemap.xml is a guess: when it is absent the root stays recorded as
    # incomplete, but that alone must not interrupt a crawl that robots.txt never promised.
    guessed_only = bool(selected) and all(root["source"] == "fallback" for root in selected)
    if any(not outcome.complete for outcome in outcomes) and not guessed_only:
        scan.interrupt("sitemap expansion is incomplete")
    elif any(root["source"] == "sitemap-only" for root in selected) and not declared_count:
        raise ScanError("sitemap-only source declared no crawlable URL members")
