"""Bounded native crawl adapter for an explicit SQLite scan artifact.

The adapter deliberately owns no global page, link, frontier, seen, or query
collections.  ``NativeScan`` is the sole writer and source of those facts.
The small per-document batch comes from the shared spider helpers so legacy and
SQLite link semantics cannot drift.
"""

from __future__ import annotations

import dataclasses
import inspect
import itertools
import json
import sqlite3
import time
from collections.abc import Callable, Iterable, Iterator
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager, nullcontext, suppress
from dataclasses import dataclass, field
from pathlib import Path
from threading import Lock
from typing import Any, cast
from urllib.parse import urlsplit

from seohead.checks.robots import is_allowed, match_path, politeness_delay
from seohead.core.models import ParsedRobots
from seohead.crawl.collect import _resolve_redirect_destination, fetch_one
from seohead.crawl.settings import (
    checked_url_budget,
    fingerprint,
    resolve_credential_headers,
)
from seohead.crawl.spider import (
    STOP_AFTER_CONSECUTIVE_FAILURES,
    Scope,
    _DispatchGate,
    _fetch_robots,
    _fold_failure_streaks,
    _strip_fragment,
    normalize_get_parameters,
)
from seohead.crawl.throttle import RequestBudgetExhausted, Throttle
from seohead.crawl.user_agents import same_robots_identity
from seohead.recon.net import UA, http_client, normalize_url
from seohead.storage import MAX_RECORD_BYTES, ScanBackpressure, ScanError
from seohead.storage.corpus import html_body_retention
from seohead.storage.native_scan import NativeScan

MAX_LINK_OBSERVATIONS = 20_000
MAX_FORM_OBSERVATIONS = 2_000


def _append_capture(captures, event):
    size = sum(len(item.entity_bytes or b"") for item in captures) + len(event.entity_bytes or b"")
    if len(captures) >= 1000 or size > 8 * MAX_RECORD_BYTES:
        raise ScanError("page response observations exceed the bounded capture unit")
    captures.append(event)


@dataclass(frozen=True)
class ScanRun:
    """Small native-scan outcome; graph evidence remains in the SQLite artifact."""

    path: str
    pages: int
    links: int
    forms: int
    lifecycle: str
    finish_reason: str
    partial: bool
    resumed: bool = False
    audit_available: bool = False
    audit_reason: str = "collection has not run the analyzer"
    limitations: tuple[str, ...] = ()
    start_page_gate: dict[str, Any] | None = None
    corpus_partial: bool = True
    capabilities: dict[str, Any] | None = None
    # How many fetched HTML page bodies the corpus kept, and why it dropped the rest
    # (#647). A capability flag cannot carry this: "partial" is the same word for one
    # missing body and for four fifths of them.
    html_bodies: dict[str, Any] = field(default_factory=dict)
    # How many rendered DOMs the corpus kept, and why it dropped the rest (#656).
    # Filled in after render escalation, which runs past this collector; a
    # capability flag cannot carry it, because "partial" is the same word for one
    # missing DOM and for all of them.
    rendered_bodies: dict[str, Any] = field(default_factory=dict)
    # Runtime-only orchestration state.  It is intentionally not written to
    # the scan artifact: locks and callbacks cannot survive a process, while
    # one in-process handler must carry its budget through audit follow-ups.
    dispatch_gate: _DispatchGate | None = None


class _SharedDispatchGate:
    """Compose a per-scan budget gate with an optional project-origin turn gate."""

    def __init__(self, local: _DispatchGate, shared_wait: Callable[[], None]) -> None:
        self._local = local
        self._shared_wait = shared_wait

    @property
    def throttle(self) -> Throttle:
        return self._local.throttle

    @property
    def requests_used(self) -> int:
        return self._local.requests_used

    @property
    def max_requests(self) -> int:
        return self._local.max_requests

    def restore_requests_used(self, value: int) -> None:
        self._local.restore_requests_used(value)

    def wait_turn(self) -> None:
        self._local.wait_turn()
        self._shared_wait()


@dataclass
class _DocumentBatch:
    links: list[dict[str, Any]] = field(default_factory=list)
    forms: list[dict[str, Any]] = field(default_factory=list)
    decisions: list[dict[str, Any]] = field(default_factory=list)
    candidates: list[dict[str, Any]] = field(default_factory=list)
    partial_reasons: list[str] = field(default_factory=list)
    route_observations: list[dict[str, Any]] = field(default_factory=list)
    route_coverage: dict[str, Any] | None = None


def _runtime(
    throttle: Throttle,
    *,
    max_depth: int,
    elapsed: float,
    timeouts: int,
    server_errors: int,
    robots_delay: float | None,
    dispatch_gate: _DispatchGate | None = None,
) -> dict[str, Any]:
    """The C-owned resume shape; no adapter-side state file exists."""
    throttle_state = throttle.snapshot_state()
    throttle_state["requests_used"] = dispatch_gate.requests_used if dispatch_gate else 0
    return {
        "max_depth_reached": max_depth,
        "elapsed_seconds": elapsed,
        "circuit_timeout_streak": timeouts,
        "circuit_server_error_streak": server_errors,
        "crawl_delay_applied": robots_delay,
        "throttle": throttle_state,
    }


def _headers(settings: dict[str, Any], url: str) -> dict[str, str] | None:
    from seohead.crawl.settings import request_headers

    headers = dict(request_headers(settings) or {})
    headers.update(
        resolve_credential_headers(
            settings["http"]["credential_headers"], urlsplit(url).hostname or ""
        )
        or {}
    )
    return headers or None


def _parse_options(
    settings: dict[str, Any], content_area_config: dict[str, Any] | None
) -> dict[str, Any]:
    """The current spider's parser options plus scan-only bounded observations."""
    options = {
        "classify_links": settings["link_position"]["classify"],
        "link_position_rules": settings["link_position"]["rules"],
        "content_area": content_area_config,
        "max_link_observations": MAX_LINK_OBSERVATIONS,
        "max_form_observations": MAX_FORM_OBSERVATIONS,
        "invalid_link_targets": settings["discovery"]["record_invalid_targets"],
    }
    if "resources" in settings:
        options.update(resource_declarations=True, max_resource_declarations=MAX_LINK_OBSERVATIONS)
    return options


def _resource_observations(record, parsed, captures, settings):
    """Describe measured-empty, bounded or unavailable declaration extraction."""
    if "resources" not in settings or not record.is_html:
        return {}
    values, omitted = [], 0
    if parsed is not None and "resource_declarations" in parsed:
        values = parsed["resource_declarations"]
        omitted = parsed["resource_declarations_omitted"]
        state = "partial" if omitted else "complete"
    elif any(event.requested_url == record.url and event.entity_bytes == b"" for event in captures):
        state = "complete"
    else:
        state = "unavailable"
    return {"resources": values, "resource_inventory_state": state, "resources_omitted": omitted}


@contextmanager
def _client_context(
    settings: dict[str, Any],
    fetcher: Callable[[str], Any] | None,
    proxy_route=None,
    request_gate: Callable[[], None] | None = None,
) -> Iterator[Any]:
    """The same guarded, no-follow client used by the legacy spider."""
    if fetcher is not None:
        yield None
        return
    from seohead.crawl.auth import host_bound_auth
    from seohead.crawl.settings import request_headers
    from seohead.recon.net import crawl_transport_options

    headers = {"User-Agent": settings["http"]["user_agent"] or UA}
    headers.update(request_headers(settings) or {})

    client, _http2 = http_client(
        settings["http"]["timeout_seconds"],
        follow_redirects=False,
        headers=headers,
        auth=host_bound_auth(settings["http"]["auth"], request_gate),
        **crawl_transport_options(proxy_route),
    )
    try:
        yield client
    finally:
        client.close()


def _restore_runtime(
    throttle: Throttle, dispatch_gate: _DispatchGate, snapshot: dict[str, Any]
) -> tuple[int, float, int, int, float | None]:
    """Use the shared Throttle restore API; never re-create hidden counters here."""
    runtime = snapshot["runtime"]
    throttle_state = dict(runtime["throttle"])
    dispatch_gate.restore_requests_used(int(throttle_state.pop("requests_used", 0)))
    throttle.restore_state(throttle_state)
    return (
        runtime["max_depth_reached"],
        runtime["elapsed_seconds"],
        runtime["circuit_timeout_streak"],
        runtime["circuit_server_error_streak"],
        runtime["crawl_delay_applied"],
    )


def _storage_failure(scan: NativeScan, exc: Exception) -> None:
    """Leave a resumable marker when SQLite rejects an otherwise valid unit."""
    reason = f"storage_error:{type(exc).__name__}:{str(exc)[:160]}"
    with suppress(Exception):
        scan.interrupt(reason)
    # Disk-full can prevent even the best-effort lifecycle update.  The caller
    # still gets the original error and C's last committed unit is left untouched.


def _document_batch(
    parsed: dict[str, Any] | None,
    *,
    source_url: str,
    depth: int,
    scope: Scope,
    start_host: str,
    settings: dict[str, Any],
) -> _DocumentBatch:
    """Use the shared helper; this module never reparses HTML or recreates links."""
    batch = _DocumentBatch()
    if parsed is None:
        if settings["rendering"]["rendered_links"]["store"]:
            from seohead.storage.rendered_routes import observations

            batch.route_observations, batch.route_coverage = observations(parsed, batch, "static")
        return batch
    try:
        from seohead.crawl.spider import apply_document_links, form_edges
    except ImportError as exc:  # integration order guard until shared helper merges
        raise RuntimeError("SQLite adapter requires shared spider document helpers") from exc

    def record_edge(edge: Any) -> None:
        batch.links.append(dataclasses.asdict(edge))

    def reject(reason: str, url: str | None) -> None:
        if url is not None:
            batch.decisions.append(
                {"url": url, "reason": reason, "source": source_url, "depth": depth + 1}
            )

    def discover(target: str, next_depth: int, requested_url: str) -> str | None:
        if (
            settings["limits"]["max_url_length"]
            and len(target) > settings["limits"]["max_url_length"]
        ):
            return "url_too_long"
        parts = urlsplit(target)
        batch.candidates.append(
            {
                "path_key": parts.path or "/",
                "query_key": parts.query,
                "requested_url": requested_url,
                "frontier_url": target,
                "depth": next_depth,
            }
        )
        # C resolves query reservation and known-frontier identity atomically.
        return None

    def mark_partial(omitted: int) -> None:
        if omitted:
            batch.partial_reasons.append("link_observations_omitted")

    apply_document_links(
        parsed,
        source_url,
        depth,
        depth_limit=settings["limits"]["max_depth"],
        host=start_host,
        rejection=scope.rejection,
        discover=discover,
        store_hyperlinks=settings["discovery"]["hyperlinks"]["store"],
        store_external_links=settings["discovery"]["external"]["store"],
        crawl_hyperlinks=settings["discovery"]["hyperlinks"]["crawl"],
        follow_nofollow=settings["discovery"]["follow_nofollow"],
        capture_link_attributes=settings["link_attributes"]["capture"],
        record_edge=record_edge,
        reject=reject,
        mark_link_partial=mark_partial,
        normalize_target=(
            normalize_get_parameters if settings["scope"]["normalize_tracking_parameters"] else None
        ),
    )
    forms, omitted = form_edges(parsed, source_url)
    batch.forms.extend(dataclasses.asdict(form) for form in forms)
    if omitted:
        batch.partial_reasons.append("form_observations_omitted")
    if settings["rendering"]["rendered_links"]["store"]:
        from seohead.storage.rendered_routes import observations

        batch.route_observations, batch.route_coverage = observations(parsed, batch, "static")
    return batch


def _forms_only_batch(parsed: dict[str, Any] | None, source_url: str) -> _DocumentBatch:
    """Keep legacy's forms-before-circuit behavior without discovering links."""
    from seohead.crawl.spider import form_edges

    batch = _DocumentBatch()
    forms, omitted = form_edges(parsed, source_url)
    batch.forms.extend(dataclasses.asdict(form) for form in forms)
    if omitted:
        batch.partial_reasons.append("form_observations_omitted")
    return batch


def _redirect_discovery(
    batch: _DocumentBatch,
    record: Any,
    *,
    depth: int,
    scope: Scope,
    host: str,
    settings: dict[str, Any],
) -> None:
    """Apply the legacy redirect-discovery order before ordinary page links."""
    if (
        not settings["discovery"]["redirects"]["crawl"]
        or not record.redirect_url
        or (settings["limits"]["max_depth"] >= 0 and depth >= settings["limits"]["max_depth"])
    ):
        return
    target = _strip_fragment(record.redirect_url)
    reason = scope.rejection(target, host)
    if reason:
        batch.decisions.append(
            {
                "url": target,
                "reason": "redirect_off_host" if reason == "outside_host" else reason,
                "source": record.url,
                "depth": depth + 1,
            }
        )
        return
    if settings["limits"]["max_url_length"] and len(target) > settings["limits"]["max_url_length"]:
        batch.decisions.append(
            {
                "url": target,
                "reason": "url_too_long",
                "source": record.url,
                "depth": depth + 1,
            }
        )
        return
    parts = urlsplit(target)
    batch.candidates.append(
        {
            "path_key": parts.path or "/",
            "query_key": parts.query,
            "requested_url": target,
            "frontier_url": target,
            "depth": depth + 1,
        }
    )


def crawl_to_scan(
    start_url: str,
    *,
    scan_out: str,
    settings: dict[str, Any],
    producer_version: str,
    producer_revision: str,
    runtime_versions: dict[str, str],
    seed_urls: Iterable[str] = (),
    initial_sitemaps: tuple[tuple[str, str], ...] = (),
    seed_loader: Callable[..., None] | None = None,
    sitemap_only: bool = False,
    content_area_config: dict[str, Any] | None = None,
    fetcher: Callable[[str], Any] | None = None,
    sleeper: Callable[[float], None] = time.sleep,
    clock: Callable[[], float] = time.monotonic,
    progress: Callable[[int, int], None] | None = None,
    progress_snapshot: Callable[[dict[str, int]], None] | None = None,
    shared_request_gate: Callable[[], None] | None = None,
    proxy_route=None,
) -> ScanRun:
    """Collect a cache-off native crawl into one explicit scan artifact.

    Handler/CLI/MCP wiring is deliberately outside this module.  Callers pass
    the already loaded settings and producer provenance, so there is no second
    configuration path here.

    ``progress``, when given, is called with ``(fetched, queued)`` once per
    frontier batch and once more when collection ends.  Both numbers come from
    the artifact's own ``resume_snapshot`` -- the same query the loop uses to
    decide whether to keep going -- so what an operator is shown is what the
    scan file on disk actually holds at that moment, never a separate in-memory
    tally that could drift from it.  It is called between batches, where no
    request is in flight, so the count is never a mid-write reading.
    """
    if settings["cache"]["mode"] != "off":
        raise ValueError(
            "SQLite scan collection requires cache.mode=off; its artifact owns retained bodies"
        )
    if settings.get("storage", {}).get("capacity_profile", "stable") != "stable":
        raise ValueError(
            "experimental_synthetic capacity profile is storage-only, not a live crawl"
        )
    limit = checked_url_budget(settings["limits"]["max_urls"])
    start = normalize_url(start_url)
    if settings["scope"]["normalize_tracking_parameters"]:
        start = normalize_get_parameters(start)
    host = (urlsplit(start).hostname or "").lower()
    if not host:
        raise ValueError("SQLite scan collection requires a crawlable start URL")
    scope = Scope.from_config(settings["scope"])
    if scope.start_folder_only:
        scope = scope.anchored_to(start)
    throttle = Throttle(
        min_delay=settings["speed"]["min_delay_seconds"],
        max_delay=settings["speed"]["max_delay_seconds"],
        max_concurrency=settings["speed"]["concurrency"],
        adaptive=settings["speed"]["adaptive"],
    )
    from seohead.crawl.events import EventSink

    adapter_events = EventSink()
    event_lock = Lock()

    def emit_event(event_type, payload):
        with event_lock:
            adapter_events.emit(event_type, payload)

    dispatch_gate = _DispatchGate(
        throttle,
        sleeper,
        clock,
        max_requests=settings["limits"]["max_requests"],
        event_callback=emit_event,
    )
    if shared_request_gate is not None:
        dispatch_gate = _SharedDispatchGate(dispatch_gate, shared_request_gate)
    started = clock()
    timeouts = server_errors = max_depth = 0
    elapsed_before = 0.0
    robots_delay = None
    partial = False
    finish_reason = "finished"
    limitations = ["browser-network response capture is unavailable"]
    start_page_gate: dict[str, Any] | None = None

    existing = Path(scan_out).exists()
    scan_context = (
        NativeScan.open(
            scan_out,
            expected_start_url=start,
            expected_config=settings,
            expected_writer_revision=producer_revision,
        )
        if existing
        else NativeScan.create(
            scan_out,
            start_url=start,
            config=settings,
            config_fingerprint=fingerprint(settings),
            writer_version=producer_version,
            writer_revision=producer_revision,
            runtime_versions=runtime_versions,
            limitations=limitations,
            initial_sitemaps=initial_sitemaps,
            format_version=settings["storage"]["format_version"],
        )
    )
    interrupted_run: ScanRun | None = None

    @contextmanager
    def checkpoint_interrupt():
        """Turn a terminal Ctrl-C into a durable native-scan checkpoint.

        The executor exits before this context sees ``KeyboardInterrupt``, so
        no worker remains in flight while the frontier is requeued.  This is a
        CLI collector boundary, not a signal handler: MCP and worker threads
        keep their existing cancellation ownership.
        """

        nonlocal interrupted_run
        with scan_context as scan:
            try:
                yield scan
            except KeyboardInterrupt as exc:
                scan.recover_inflight()
                scan.record_events(adapter_events)
                scan.record_request_count(dispatch_gate.requests_used)
                action = getattr(exc, "action", None)
                reason = (
                    f"operator requested {action}"
                    if action in {"pause", "stop"}
                    else "operator interrupted (SIGINT)"
                )
                scan.interrupt(reason)
                outcome = scan.resume_snapshot(include_edges=True)
                interrupted_run = ScanRun(
                    path=str(scan.path),
                    pages=outcome["counts"]["pages"],
                    links=outcome["counts"]["links"],
                    forms=outcome["counts"]["forms"],
                    lifecycle=outcome["scan"]["lifecycle"],
                    finish_reason="interrupted",
                    partial=True,
                    resumed=existing,
                    limitations=tuple(json.loads(outcome["scan"]["limitations_json"])),
                    start_page_gate=start_page_gate
                    or retained_start_gate(scan, settings, content_area_config),
                    dispatch_gate=dispatch_gate,
                    corpus_partial=bool(outcome["scan"]["corpus_partial"]),
                    capabilities=json.loads(outcome["scan"]["capabilities_json"]),
                    html_bodies=html_body_retention(scan.con),
                )

    # A native crawl claims at most ``concurrency`` URLs per durable batch.  Keep
    # the workers alive across those batches: creating and joining a fresh pool
    # per batch makes the dispatcher itself the throughput limit at large URL
    # counts, while the ordered fold-back below still awaits exactly the same
    # futures in exactly the same lease order.
    with (
        _client_context(settings, fetcher, proxy_route, dispatch_gate.wait_turn) as client,
        checkpoint_interrupt() as scan,
        ThreadPoolExecutor(max_workers=max(1, throttle.concurrency)) as fetch_pool,
    ):
        scan.preflight_capture()
        snapshot = scan.resume_snapshot()
        seeded = (
            existing
            and json.loads(snapshot["scan"]["capabilities_json"])["resume"]["state"] == "complete"
        )
        selected_roots = scan.sitemap_roots() if seed_loader is not None else []
        if any(
            not (
                scan.read_context("sitemap_fetch_summary", f"url:{root['sitemap_url_id']}") or {}
            ).get("complete")
            for root in selected_roots
        ):
            seeded = False
        if existing and initial_sitemaps:
            expected = [url for url, _source in initial_sitemaps]
            selected = [root["url"] for root in selected_roots if root["source"] == "explicit"]
            if expected != selected:
                raise ScanError("selected sitemap inputs changed; refusing unsafe resume")
        discovery_needs_robots = (
            seed_loader is not None and settings["sitemaps"]["auto_discover"] and not selected_roots
        )
        robots_policy = settings["robots"]["policy"]
        robots_token = settings["robots"]["user_agent_token"]
        saved_robots = scan.read_context("robots_summary") if existing else None
        if saved_robots is not None:
            if not isinstance(saved_robots, dict):
                raise ScanError("resumable native scan is missing its robots summary")
            saved_token = saved_robots.get("token")
            if (
                saved_robots.get("policy") != robots_policy
                or not isinstance(saved_token, str)
                or not same_robots_identity(saved_token, robots_token)
            ):
                raise ScanError(
                    "resumable native scan robots policy differs from its stored context"
                )
            # Resume under the identity the scan started with, so its stored context stays one token.
            robots_token = saved_token
            robots = saved_robots.get("parsed")
            robots_note = saved_robots.get("note")
            robots_state = saved_robots.get("fetch_state")
            if not isinstance(robots, dict) or not isinstance(robots_note, str):
                raise ScanError("resumable native scan robots context is invalid")
            robots_unavailable = robots_state == "unavailable"
        elif seeded:
            raise ScanError("resumable native scan is missing its robots summary")
        elif robots_policy == "ignore" and not discovery_needs_robots:
            robots, robots_note, robots_unavailable = (
                {"groups": [], "sitemaps": []},
                "robots.txt not fetched (policy: ignore)",
                False,
            )
            robots_state = "not_fetched"
        else:
            robots, robots_note, robots_unavailable = _fetch_robots(
                start, fetcher, client, wait=dispatch_gate.wait_turn
            )
            robots_state = "unavailable" if robots_unavailable else "fetched"
        if saved_robots is None:
            scan.write_context(
                [
                    {
                        "kind": "robots_summary",
                        "item_key": "run",
                        "payload_version": "scan_context.v1",
                        "payload_json": json.dumps(
                            {
                                "policy": robots_policy,
                                "token": robots_token,
                                "fetch_state": robots_state,
                                "final_response_id": None,
                                "note": robots_note,
                                "parsed": robots,
                            },
                            sort_keys=True,
                        ),
                        "completeness": "partial" if robots_unavailable else "complete",
                        "reason": robots_note,
                    }
                ]
            )
        if (
            robots_policy == "respect"
            and robots_unavailable
            and settings["robots"]["unavailable_means_stop"]
        ):
            scan.interrupt(robots_note or "robots.txt unavailable")
            outcome = scan.resume_snapshot(include_edges=True)
            return ScanRun(
                str(scan.path),
                outcome["counts"]["pages"],
                outcome["counts"]["links"],
                outcome["counts"]["forms"],
                outcome["scan"]["lifecycle"],
                "robots_unavailable",
                True,
                resumed=existing,
                limitations=tuple(json.loads(outcome["scan"]["limitations_json"])),
                corpus_partial=bool(outcome["scan"]["corpus_partial"]),
                capabilities=json.loads(outcome["scan"]["capabilities_json"]),
                html_bodies=html_body_retention(scan.con),
                dispatch_gate=dispatch_gate,
            )
        asked_delay = politeness_delay(cast(ParsedRobots, robots), robots_token)
        if asked_delay is not None:
            robots_delay = max(throttle.min_delay, asked_delay)
            if asked_delay > throttle.min_delay:
                throttle.min_delay = asked_delay
                throttle.delay = max(throttle.delay, asked_delay)
        if existing:
            snapshot = scan.resume_snapshot()
            max_depth, elapsed_before, timeouts, server_errors, robots_delay = _restore_runtime(
                throttle, dispatch_gate, snapshot
            )
            scan.recover_inflight()
        sitemap_only = sitemap_only or any(
            root["source"] == "sitemap-only" for root in selected_roots
        )
        if not seeded:
            # Initial input chunks are replayed until the durable phase is complete.
            # D depends on C's atomic seed API: supply in chunks so sitemap expansion
            # never becomes a second Python frontier.  Until that API is integrated,
            # this intentionally fails instead of falling back to a deque/set.
            if not sitemap_only:
                scan.seed_frontier(
                    [
                        {
                            "requested_url": start,
                            "frontier_url": start,
                            "depth": 0,
                            "reason": "",
                            "source": "start",
                            "reserve_query": False,
                            "seed": False,
                        }
                    ]
                )

            # Existing seed handling preserves the supplied request spelling; C
            # performs canonical membership internally without altering evidence.
            def seed_entry(url: str) -> dict[str, Any]:
                requested = url.strip()
                if settings["scope"]["normalize_tracking_parameters"]:
                    requested = normalize_get_parameters(requested)
                reason = scope.rejection(requested, host)
                if (
                    not reason
                    and settings["limits"]["max_url_length"]
                    and len(requested) > settings["limits"]["max_url_length"]
                ):
                    reason = "url_too_long"
                return {
                    "requested_url": requested,
                    "frontier_url": requested,
                    "depth": 0,
                    "reason": reason,
                    "source": "sitemap",
                    "reserve_query": True,
                    "seed": True,
                }

            def emit_seeds(urls: Iterable[str]) -> None:
                seed_iter = (seed_entry(url) for url in urls if url and url.strip())
                while chunk := list(itertools.islice(seed_iter, 256)):
                    scan.seed_frontier(chunk)

            if seed_loader is None:
                emit_seeds(seed_urls)
            else:
                signature = inspect.signature(seed_loader)
                try:
                    signature.bind(scan, emit_seeds, request_gate=dispatch_gate.wait_turn)
                except TypeError:
                    # Seed loaders predate shared dispatch pacing. Bind before
                    # calling so a TypeError raised inside the callback cannot
                    # be mistaken for an incompatible signature and replay its
                    # side effects.
                    signature.bind(scan, emit_seeds)
                    seed_loader(scan, emit_seeds)
                else:
                    seed_loader(scan, emit_seeds, request_gate=dispatch_gate.wait_turn)

        frontier_state = scan.resume_snapshot()["counts"]
        if frontier_state.get("queued", 0) or frontier_state.get("inflight", 0):
            scan.begin_collection()

        while True:
            snapshot = scan.resume_snapshot()
            counts = snapshot["counts"]
            if progress_snapshot is not None:
                progress_snapshot(
                    {
                        "fetched": counts["pages"],
                        **{key: counts[key] for key in ("queued", "inflight", "excluded")},
                    }
                )
            if progress is not None:
                progress(counts["pages"], counts["queued"] + counts["inflight"])
            if limit and counts["pages"] >= limit:
                emit_event("budget", {"kind": "urls", "limit": limit, "used": counts["pages"]})
                if counts["queued"] or counts["inflight"]:
                    partial, finish_reason = True, "stopped_by_budget"
                    scan.stop_by_budget(f"url limit reached ({limit})")
                break
            if (
                settings["limits"]["max_crawl_seconds"]
                and elapsed_before + clock() - started >= settings["limits"]["max_crawl_seconds"]
            ):
                emit_event(
                    "budget",
                    {
                        "kind": "seconds",
                        "limit": settings["limits"]["max_crawl_seconds"],
                        "used": int(elapsed_before + clock() - started),
                    },
                )
                if counts["queued"] or counts["inflight"]:
                    partial, finish_reason = True, "duration_limit"
                    scan.interrupt("duration limit reached")
                break
            remaining = limit - counts["pages"] if limit else throttle.concurrency
            scan.preflight_capture()
            try:
                leases = scan.claim(min(throttle.concurrency, remaining))
            except ScanBackpressure as exc:
                # The storage layer already waited out the reader for as long as
                # it may. Stop collecting rather than raising: the pages already
                # committed are sound, and the artifact says why it is short.
                partial, finish_reason = True, "storage_backpressure"
                scan.interrupt(f"storage backpressure: {exc}")
                break
            except (ScanError, sqlite3.Error) as exc:
                _storage_failure(scan, exc)
                raise ScanError(f"native scan storage failure: {exc}") from exc
            if not leases:
                # Handler owns audit/no-audit finalization after its bounded
                # compatibility bridge decides whether it may materialize.
                break

            parse_options = _parse_options(settings, content_area_config)

            def fetch(
                lease: Any,
                *,
                gate: _DispatchGate = dispatch_gate,
                options: dict[str, Any] = parse_options,
            ) -> Any:
                captures = []
                captured_bytes = 0

                def observe(observation):
                    nonlocal captured_bytes
                    captured_bytes += (
                        len(observation.entity_bytes) if observation.entity_bytes is not None else 0
                    )
                    if len(captures) >= 1000 or captured_bytes > 8 * MAX_RECORD_BYTES:
                        raise ScanError(
                            "page response observations exceed the bounded capture unit"
                        )
                    captures.append(observation)

                capture_options = {}
                if "storage" in settings:
                    capture_options = {
                        "capture_observer": observe,
                        "capture_max_bytes": min(
                            8 * MAX_RECORD_BYTES,
                            max(
                                settings["limits"]["max_response_bytes"],
                                settings["storage"]["max_body_bytes"]
                                if settings["storage"]["body_mode"] != "off"
                                else 0,
                            ),
                        ),
                    }
                result = fetch_one(
                    lease.url,
                    client=client,
                    fetcher=fetcher,
                    throttle=throttle,
                    extra_headers=_headers(settings, lease.url),
                    user_agent=settings["http"]["user_agent"],
                    max_response_bytes=settings["limits"]["max_response_bytes"],
                    retry_on_timeout=settings["http"]["retry_on_timeout"],
                    parse_options=options,
                    cache=None,
                    wait=gate.wait_turn,
                    response_filter=scope.response_media_rejection,
                    **capture_options,
                )
                return lease, result, captures

            actions: list[tuple[Any, bool]] = []
            robots_context: dict[int, list[dict[str, str]]] = {}
            for lease in leases:
                blocked = robots_policy in {"respect", "report_only"} and not is_allowed(
                    cast(ParsedRobots, robots), match_path(lease.url), robots_token
                )
                if blocked:
                    robots_context[lease.queue_ordinal] = [
                        {
                            "kind": "robots_blocked_url",
                            "item_key": f"url:{lease.url_id}",
                            "payload_version": "scan_context.v1",
                            "payload_json": json.dumps(
                                {
                                    "url_id": lease.url_id,
                                    "token": robots_token,
                                    "policy": "report_only",
                                },
                                sort_keys=True,
                            ),
                            "completeness": "complete",
                            "reason": "robots.txt",
                        }
                    ]
                actions.append((lease, blocked and robots_policy == "respect"))

            fetchable = [lease for lease, excluded in actions if not excluded]
            for lease in fetchable:
                emit_event(
                    "request",
                    {
                        "queue_ordinal": lease.queue_ordinal,
                        "url_id": lease.url_id,
                        "attempt": 1,
                        "method": "GET",
                        "state": "dispatched",
                    },
                )
            with nullcontext(fetch_pool) as pool:
                # Futures are consumed in claim order: the C writer's contiguous
                # inflight-prefix rule then gives deterministic evidence order.
                futures = {lease.queue_ordinal: pool.submit(fetch, lease) for lease in fetchable}
                for lease, excluded in actions:
                    if excluded:
                        max_depth = max(max_depth, lease.depth)
                        try:
                            scan.exclude_lease(
                                lease,
                                "blocked_by_robots",
                                runtime=_runtime(
                                    throttle,
                                    max_depth=max_depth,
                                    elapsed=elapsed_before + clock() - started,
                                    timeouts=timeouts,
                                    server_errors=server_errors,
                                    robots_delay=robots_delay,
                                    dispatch_gate=dispatch_gate,
                                ),
                            )
                        except (ScanError, sqlite3.Error) as exc:
                            _storage_failure(scan, exc)
                            raise ScanError(f"native scan storage failure: {exc}") from exc
                        continue
                    try:
                        lease, (record, parsed), captures = futures[lease.queue_ordinal].result()
                    except RequestBudgetExhausted:
                        partial, finish_reason = True, "request_limit"
                        scan.interrupt("total HTTP request budget exhausted")
                        break
                    except ScanError as exc:
                        _storage_failure(scan, exc)
                        raise
                    record.crawl_depth = lease.depth
                    emit_event(
                        "cache",
                        {
                            "queue_ordinal": lease.queue_ordinal,
                            "url_id": lease.url_id,
                            "state": record.cache_status or "network",
                        },
                    )
                    if record.error_kind == "timeout":
                        emit_event(
                            "retry",
                            {
                                "queue_ordinal": lease.queue_ordinal,
                                "url_id": lease.url_id,
                                "attempt": 1,
                                "reason": "timeout",
                                "delay_ms": round(throttle.delay * 1000),
                            },
                        )
                    max_depth = max(max_depth, lease.depth)
                    if not sitemap_only and (
                        settings["discovery"]["resolve_redirect_destination"]
                        and record.redirect_url
                    ):
                        _resolve_redirect_destination(
                            record,
                            client=client,
                            fetcher=fetcher,
                            throttle=throttle,
                            extra_headers=_headers(settings, lease.url),
                            user_agent=settings["http"]["user_agent"],
                            max_response_bytes=settings["limits"]["max_response_bytes"],
                            retry_on_timeout=settings["http"]["retry_on_timeout"],
                            parse_options=parse_options,
                            cache=None,
                            wait=dispatch_gate.wait_turn,
                            capture_observer=(
                                lambda observation, captured=captures: _append_capture(
                                    captured, observation
                                )
                            )
                            if "storage" in settings
                            else None,
                            capture_max_bytes=min(
                                8 * MAX_RECORD_BYTES,
                                max(
                                    settings["limits"]["max_response_bytes"],
                                    settings.get("storage", {}).get("max_body_bytes", 0),
                                ),
                            ),
                            headers_for_url=lambda target: _headers(settings, target),
                        )
                    if (
                        start_page_gate is None
                        and lease.depth == 0
                        and (sitemap_only or lease.url == start)
                        and parsed is not None
                    ):
                        start_page_gate = {
                            "html": parsed.get("_raw_html", ""),
                            "outlinks": record.outlinks,
                            "external_outlinks": record.external_outlinks,
                        }
                    timeouts, server_errors = _fold_failure_streaks(record, timeouts, server_errors)
                    circuit_stopped = (
                        timeouts >= settings["speed"]["stop_after_consecutive_timeouts"]
                        or server_errors >= STOP_AFTER_CONSECUTIVE_FAILURES
                    )
                    if circuit_stopped:
                        emit_event(
                            "circuit",
                            {
                                "kind": "origin",
                                "state": "stopped",
                                "streak": max(timeouts, server_errors),
                            },
                        )
                        batch = _forms_only_batch(parsed, lease.url)
                    else:
                        batch = _DocumentBatch()
                        if not sitemap_only:
                            _redirect_discovery(
                                batch,
                                record,
                                depth=lease.depth,
                                scope=scope,
                                host=host,
                                settings=settings,
                            )
                        links_batch = _document_batch(
                            parsed,
                            source_url=lease.url,
                            depth=lease.depth,
                            scope=scope,
                            start_host=host,
                            settings=settings,
                        )
                        batch.links.extend(links_batch.links)
                        batch.forms.extend(links_batch.forms)
                        batch.decisions.extend(links_batch.decisions)
                        if not sitemap_only:
                            batch.candidates.extend(links_batch.candidates)
                        batch.partial_reasons.extend(links_batch.partial_reasons)
                        batch.route_observations.extend(links_batch.route_observations)
                        batch.route_coverage = links_batch.route_coverage
                    if record.body_unavailable in {
                        "excluded_by_media_type",
                        "not_included_by_media_type",
                        "media_type_unavailable",
                    }:
                        batch.decisions.append(
                            {
                                "url": record.url,
                                "reason": record.body_unavailable,
                                "source": record.url,
                                "depth": lease.depth,
                            }
                        )
                    try:
                        if (
                            record.is_html
                            and parsed is None
                            and any(
                                event.body_state in {"truncated", "unavailable"}
                                and event.body_reason in {"truncated", "fetch_failed"}
                                for event in captures
                            )
                        ):
                            batch.partial_reasons.append(
                                "response_body_unavailable: HTML could not be decoded completely"
                            )
                        resource_observations = _resource_observations(
                            record, parsed, captures, settings
                        )
                        if resource_observations.get("resources_omitted"):
                            batch.partial_reasons.append("resource_declarations_omitted")
                        scan.commit_page(
                            lease,
                            dataclasses.asdict(record),
                            links=batch.links,
                            forms=batch.forms,
                            decisions=batch.decisions,
                            candidates=batch.candidates,
                            runtime=_runtime(
                                throttle,
                                max_depth=max_depth,
                                elapsed=elapsed_before + clock() - started,
                                timeouts=timeouts,
                                server_errors=server_errors,
                                robots_delay=robots_delay,
                                dispatch_gate=dispatch_gate,
                            ),
                            partial_reasons=tuple(batch.partial_reasons),
                            context=robots_context.get(lease.queue_ordinal, ()),
                            content_capture={
                                "html": (
                                    parsed.get("_raw_html") if isinstance(parsed, dict) else None
                                ),
                                "parsed": parsed,
                                "settings": settings,
                                "indexable": None,
                                "canonical_target": record.canonical,
                                "unavailable_reason": (
                                    "static document was not parsed as eligible HTML"
                                    if parsed is None
                                    else ""
                                ),
                                "extraction_rules": None,
                            },
                            route_observations=batch.route_observations,
                            route_coverage=batch.route_coverage,
                            captures=captures,
                            **resource_observations,
                        )
                    except (ScanError, sqlite3.Error) as exc:
                        _storage_failure(scan, exc)
                        raise ScanError(f"native scan storage failure: {exc}") from exc
                    if circuit_stopped:
                        partial, finish_reason = True, "errors"
                        scan.interrupt("origin stopped responding or refused repeatedly")
                        break
                if partial:
                    break

        if finish_reason not in {"errors", "storage_backpressure"}:
            from seohead.storage.resource_graph import capture as capture_resource_graph

            from .sqlite_resources import capture_resources

            capture_resources(
                scan,
                settings,
                client=client,
                fetcher=fetcher,
                clock=clock,
                sleeper=sleeper,
                throttle=throttle,
                dispatch_gate=dispatch_gate,
            )
            capture_resource_graph(
                scan,
                settings,
                client=client,
                fetcher=fetcher,
                wait=dispatch_gate.wait_turn,
                clock=clock,
            )
            if not sitemap_only:  # a sitemap-only run fetches its members and nothing else
                from seohead.storage.target_probes import capture as capture_target_probes

                capture_target_probes(
                    scan,
                    settings,
                    client=client,
                    fetcher=fetcher,
                    wait=dispatch_gate.wait_turn,
                )
        scan.record_events(adapter_events)
        scan.record_request_count(dispatch_gate.requests_used)
        if start_page_gate is None:
            start_page_gate = retained_start_gate(scan, settings, content_area_config)
        outcome = scan.resume_snapshot(include_edges=True)
        if progress_snapshot is not None:
            counts = outcome["counts"]
            progress_snapshot(
                {
                    "fetched": counts["pages"],
                    **{key: counts[key] for key in ("queued", "inflight", "excluded")},
                }
            )
        if progress is not None:
            # The last word on this run, read after collection has stopped: a
            # crawl that ended on the URL budget still has a queue, and saying
            # so is the difference between "finished" and "stopped early".
            # Leases still marked inflight are outstanding work too -- an
            # interrupted run recovers them on resume, so they are counted here
            # exactly as they are in the loop above.
            progress(
                outcome["counts"]["pages"],
                outcome["counts"]["queued"] + outcome["counts"]["inflight"],
            )
        return ScanRun(
            path=str(scan.path),
            pages=outcome["counts"]["pages"],
            links=outcome["counts"]["links"],
            forms=outcome["counts"]["forms"],
            lifecycle=outcome["scan"]["lifecycle"],
            finish_reason=finish_reason,
            partial=partial or outcome["scan"]["crawl_partial"] == 1,
            resumed=existing,
            limitations=tuple(json.loads(outcome["scan"]["limitations_json"])),
            start_page_gate=start_page_gate,
            dispatch_gate=dispatch_gate,
            corpus_partial=bool(outcome["scan"]["corpus_partial"]),
            capabilities=json.loads(outcome["scan"]["capabilities_json"]),
            html_bodies=html_body_retention(scan.con),
        )

    if interrupted_run is not None:
        return interrupted_run
    raise RuntimeError("native scan collector exited without an outcome")


def retained_start_gate(scan, settings, content_area_config=None):
    """Reconstruct the raw start-page gate from its static document, one body at a time."""
    from seohead.crawl.collect import PageRecord, _apply_body
    from seohead.storage.bodies import read_document

    start = scan.con.execute("SELECT start_url FROM scan").fetchone()[0]
    row = scan.con.execute(
        "SELECT d.document_id,r.content_type,r.effective_url_id,u.url AS final_url,b.decoded_bytes "
        "FROM documents d JOIN urls logical ON logical.url_id=d.url_id "
        "JOIN responses r ON r.response_id=d.source_response_id "
        "JOIN bodies b ON b.sha256=d.body_sha256 "
        "LEFT JOIN urls u ON u.url_id=r.effective_url_id "
        "WHERE logical.url=? AND d.representation='static' AND d.body_state='complete' "
        "ORDER BY d.document_id DESC LIMIT 1",
        (start,),
    ).fetchone()
    if row is None:
        return None
    html = read_document(scan.con, row["document_id"], max_decoded_bytes=8 * MAX_RECORD_BYTES)
    record = PageRecord(url=start, content_type=row["content_type"])
    parsed = _apply_body(
        record,
        row["final_url"] or start,
        html,
        parse_options=_parse_options(settings, content_area_config),
        max_response_bytes=settings["limits"]["max_response_bytes"],
        size_bytes=row["decoded_bytes"],
    )
    if parsed is None:
        return None
    return {
        "html": html,
        "outlinks": record.outlinks,
        "external_outlinks": record.external_outlinks,
    }
