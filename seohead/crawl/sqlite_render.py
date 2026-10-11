"""Bounded rendering escalation for a native SQLite scan.

The regular directory crawl keeps rendered HTML in ``EscalationResult`` long
enough for ``apply_rendered_evidence`` to fold it back into its page list.  A
scan artifact must not do that: a large crawl would retain every serialized
DOM twice.  This adapter consumes each DOM immediately, commits it in the
same transaction as its page and graph observations, and leaves only the
small escalation summary in memory.
"""

from __future__ import annotations

import copy
import dataclasses
import json
import time
from collections.abc import Callable
from datetime import datetime, timezone
from typing import Any
from urllib.parse import urlsplit

from seohead.crawl import render_escalation


def _now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _policy_facts(settings: dict[str, Any], target_url: str) -> dict[str, bool]:
    """Return only facts the renderer may record about retention policy."""
    from seohead.crawl.settings import resolve_credential_headers

    browser = settings["rendering"]["browser"]
    host = urlsplit(target_url).hostname or ""
    return {
        "credentials_used": bool(
            resolve_credential_headers(settings["http"]["credential_headers"], host)
        )
        or bool(browser.get("persistent_profile")),
        # A browser DOM is not an HTTP response.  It cannot honestly inherit a
        # cache-control header from the static response.
        "cache_control_no_store": False,
    }


def _unknown_renderer(target_url: str, settings: dict[str, Any]) -> dict[str, Any]:
    """Record an unsuccessful renderer attempt without inventing its identity.

    The attempt never produced a browser, so ``engine`` honestly stays
    ``unknown`` -- the *requested* engine is recorded next to it instead, with
    the same effective viewport and page-concurrency a successful renderer
    record would carry.
    """
    browser = settings["rendering"]["browser"]
    from seohead.checks.render import VIEWPORT_PRESETS, resolve_viewport

    try:
        viewport = resolve_viewport(browser)
    except ValueError:
        viewport = dict(VIEWPORT_PRESETS.get(browser["viewport"], VIEWPORT_PRESETS["desktop"]))

    return {
        "engine": "unknown",
        "requested_engine": f"playwright-{browser['engine']}",
        "engine_version": "unknown",
        "navigation": {
            "requested_url": target_url,
            "final_url": target_url,
            "wait_until": browser["wait_until"],
            "timeout_seconds": float(settings["http"]["timeout_seconds"]),
        },
        "settings": {
            "viewport": viewport,
            "device_pixel_ratio": float(browser["device_pixel_ratio"]),
            "mobile_emulation": bool(browser["mobile_emulation"]),
            "touch_emulation": bool(browser["touch_emulation"]),
            "script_timeout_seconds": float(browser["script_timeout_seconds"]),
            "resize_to_content": bool(browser["resize_to_content"]),
            "resize_to_content_max_height_px": int(browser["resize_to_content_max_height_px"]),
            "persistent_profile": bool(browser["persistent_profile"]),
        },
        "transforms": {
            "flatten_shadow_dom_requested": bool(browser["flatten_shadow_dom"]),
            "flatten_shadow_dom_applied": 0,
            "flatten_iframes_requested": bool(browser["flatten_iframes"]),
            "flatten_iframes_applied": 0,
        },
        "policy": _policy_facts(settings, target_url),
        "page_concurrency": int(browser.get("page_concurrency") or 1),
    }


def _rendered_batch(
    parsed: dict[str, Any] | None,
    *,
    target_url: str,
    depth: int,
    settings: dict[str, Any],
) -> tuple[
    list[dict[str, Any]],
    list[dict[str, Any]],
    list[str],
    list[dict[str, Any]],
    dict[str, Any] | None,
    list[dict[str, Any]],
    list[dict[str, Any]],
]:
    """Use the native collector's existing parser-to-observation contract.

    The caller may opt into normal frontier admission after this shared scope,
    depth, URL-length and query-candidate conversion has completed.
    """
    from seohead.crawl.spider import Scope
    from seohead.crawl.sqlite_adapter import _document_batch

    batch = _document_batch(
        parsed,
        source_url=target_url,
        depth=depth,
        scope=Scope.from_config(settings["scope"]),
        start_host=(urlsplit(target_url).hostname or "").lower(),
        settings=settings,
    )
    if settings["rendering"]["rendered_links"]["store"]:
        from seohead.storage.rendered_routes import observations

        routes, coverage = observations(parsed, batch, "rendered")
    else:
        routes, coverage = [], None
    candidates = batch.candidates if settings["rendering"]["rendered_links"]["crawl"] else []
    decisions = batch.decisions if settings["rendering"]["rendered_links"]["crawl"] else []
    return batch.links, batch.forms, batch.partial_reasons, routes, coverage, candidates, decisions


def _content_capture(
    *,
    html: Any,
    parsed: dict[str, Any] | None,
    settings: dict[str, Any],
    canonical_target: str = "",
    unavailable_reason: str = "",
) -> dict[str, Any]:
    """Carry one parsed representation into the writer-owned document context hook."""
    return {
        "html": html if isinstance(html, str) else None,
        "parsed": parsed,
        "settings": settings,
        "indexable": None,
        "canonical_target": canonical_target,
        "unavailable_reason": unavailable_reason,
        "extraction_rules": None,
    }


def _candidate(
    record: Any,
    target_url: str,
    fetched: dict[str, Any],
    representation: str,
    current_links: list[Any],
    settings: dict[str, Any],
    max_parse_bytes: int,
) -> tuple[Any | None, dict[str, Any] | None, bool]:
    """Derive one rendered page with the established raw/rendered union rule."""
    from dataclasses import replace

    html = fetched.get("html")
    if not isinstance(html, str) or not html:
        return None, None, False
    from seohead.crawl.sqlite_adapter import _parse_options

    candidate = replace(record)
    renderer = fetched.get("renderer")
    if isinstance(renderer, dict):
        from seohead.checks.element_diff import classify_diffs, diff_elements
        from seohead.checks.render import _snapshot

        rendered_values = _snapshot(html, fetched.get("final_url") or target_url, "rendered")
        raw_available = getattr(record, "trust_signals", None) is not None
        raw_values = {
            "title": record.title,
            "description": record.meta_description,
            "h1": record.h1,
            "canonical": record.canonical,
            "meta_robots": record.meta_robots,
        }
        diffs = diff_elements(raw_values, rendered_values) if raw_available else []
        renderer["raw_rendered_evidence"] = {
            "schema": "raw_rendered_evidence.v1",
            "state": "complete" if raw_available else "unavailable",
            "reason": "" if raw_available else "static source HTML was not parsed",
            "raw": raw_values if raw_available else None,
            "rendered": {name: rendered_values.get(name, "") for name in raw_values},
            "diffs": [dict(item) for item in diffs],
            "rules": [
                {"rule": rule, "diff": dict(item)} for rule, item in classify_diffs(diffs)
            ],
        }
    parsed, degenerate = render_escalation.fold_rendered_evidence(
        candidate,
        current_links,
        target_url,
        fetched,
        representation,
        parse_options=_parse_options(settings, None),
        max_response_bytes=max_parse_bytes,
    )
    if degenerate:
        return None, parsed, True
    return candidate, parsed, False


def _source_links(scan: Any, target_url: str, fallback: list[Any]) -> list[Any]:
    """Read one static source's observations without materializing the graph."""
    from seohead.crawl.spider import LinkEdge

    if not hasattr(scan, "con"):
        return [edge for edge in fallback if edge.source == target_url]
    rows = scan.con.execute(
        "SELECT destination.url AS destination,l.anchor,l.nofollow,l.position,l.rel_json,l.target,"
        "l.raw_href FROM links l JOIN urls source ON source.url_id=l.source_url_id "
        "JOIN urls destination ON destination.url_id=l.destination_url_id "
        "WHERE source.url=? AND l.evidence_representation='static' ORDER BY l.ordinal",
        (target_url,),
    )
    return [
        LinkEdge(
            source=target_url,
            destination=row["destination"],
            anchor=row["anchor"],
            nofollow=bool(row["nofollow"]),
            position=row["position"],
            rel=tuple(json.loads(row["rel_json"])),
            target=row["target"],
            raw_href=row["raw_href"],
        )
        for row in rows
    ] or [edge for edge in fallback if edge.source == target_url]


def _apply_committed_result(record: Any, candidate: Any) -> None:
    """Update the transient page only after its SQL transaction committed."""
    for field in dataclasses.fields(record):
        setattr(record, field.name, getattr(candidate, field.name))


def _document_state(scan: Any, document_id: int) -> tuple[str, str]:
    """Report the stored retention state without treating omission as failure."""
    if not hasattr(scan, "con"):
        return "complete", ""
    row = scan.con.execute(
        "SELECT body_state,body_reason FROM documents WHERE document_id=?", (document_id,)
    ).fetchone()
    if row is None:
        raise RuntimeError("render transaction returned an unknown document")
    return str(row[0]), "" if row[0] == "complete" else str(row[1])


def _static_html(scan: Any, target_url: str, max_bytes: int) -> str:
    """Read one retained static document for legacy-fragment probing.

    This is deliberately a database read, not a fallback request.  A resumed
    scan therefore cannot turn a missing raw observation into fresh evidence.
    """
    from seohead.storage.bodies import read_document

    row = scan.con.execute(
        "SELECT d.document_id FROM documents d JOIN urls u ON u.url_id=d.url_id "
        "WHERE u.url=? AND d.representation='static' AND d.body_state='complete' "
        "ORDER BY d.document_id DESC LIMIT 1",
        (target_url,),
    ).fetchone()
    if row is None:
        return ""
    return read_document(scan.con, int(row[0]), max_decoded_bytes=max_bytes)


def _legacy_fetch(
    target_url: str,
    raw_html: str,
    settings: dict[str, Any],
    scan: Any,
    max_parse_bytes: int,
    request_gate: Callable[[], None] | None = None,
    proxy_route=None,
) -> dict[str, Any]:
    """Fetch an opted-in escaped fragment and retain its actual response."""
    from seohead.checks import render as render_tool
    from seohead.crawl.capture import now_utc
    from seohead.crawl.collect import fetch_one
    from seohead.crawl.settings import resolve_credential_headers
    from seohead.recon.net import http_client

    escaped = render_tool.legacy_fragment_target(target_url, raw_html)
    if not escaped:
        return {"ok": False, "url": target_url, "error": "no legacy fragment target"}
    captures: list[Any] = []
    host = urlsplit(escaped).hostname or ""
    extra_headers = dict(settings["http"]["headers"])
    extra_headers.update(
        resolve_credential_headers(settings["http"]["credential_headers"], host) or {}
    )
    from seohead.recon.net import crawl_transport_options

    client, _http2 = http_client(
        settings["http"]["timeout_seconds"],
        follow_redirects=False,
        headers={"User-Agent": settings["http"]["user_agent"]},
        **crawl_transport_options(proxy_route),
    )
    try:
        record, parsed = fetch_one(
            escaped,
            client=client,
            extra_headers=extra_headers or None,
            user_agent=settings["http"]["user_agent"],
            max_response_bytes=settings["limits"]["max_response_bytes"],
            retry_on_timeout=settings["http"]["retry_on_timeout"],
            wait=request_gate,
            capture_observer=captures.append,
            capture_max_bytes=max_parse_bytes,
        )
    finally:
        client.close()
    html = parsed.get("_raw_html", "") if parsed is not None else ""
    return {
        "ok": bool(parsed is not None and html),
        "url": target_url,
        "final_url": record.final_url or escaped,
        "html": html,
        "record": record,
        "parsed": parsed,
        "captures": tuple(captures),
        "captured_at": captures[-1].received_at if captures else now_utc(),
        "renderer": {
            **_unknown_renderer(target_url, settings),
            "engine": "legacy-escaped-fragment",
            "engine_version": "scan.v1",
            "navigation_transform": "legacy_escaped_fragment",
            "navigation": {
                "requested_url": escaped,
                "final_url": record.final_url or escaped,
                "wait_until": settings["rendering"]["browser"]["wait_until"],
                "timeout_seconds": float(settings["http"]["timeout_seconds"]),
            },
        },
    }


def run_render_escalation(
    scan: Any,
    result: Any,
    settings: dict[str, Any],
    *,
    request_gate: Callable[[], None] | None = None,
    proxy_route=None,
) -> Any:
    """Run selective rendering and store each attempted representation promptly.

    The return value remains ``EscalationResult`` so existing audit summaries
    retain their contract.  Its ``rendered`` entries contain only metadata;
    serialized HTML is released as soon as its transaction has committed.
    """
    mode = settings["rendering"]["mode"]
    from seohead.storage.rendered_routes import RUN_KIND, run_context

    if hasattr(scan, "write_context") and (
        not hasattr(scan, "read_context") or scan.read_context(RUN_KIND) is None
    ):
        enabled = settings["rendering"]["rendered_links"]["store"]
        if not enabled:
            scan.write_context(
                [
                    run_context(
                        "unavailable",
                        "rendered route ledger disabled by policy",
                        False,
                        mode,
                        len(result.pages),
                        0,
                    )
                ]
            )
        elif mode == "raw":
            scan.write_context(
                [
                    run_context(
                        "unavailable",
                        "rendering mode is raw; rendered representation was not requested",
                        True,
                        mode,
                        len(result.pages),
                        0,
                    )
                ]
            )
    if mode == "raw" or not result.pages:
        return render_escalation.EscalationResult(mode=mode)

    from seohead.checks import render as render_tool

    render_started = time.monotonic()
    elapsed_before = (
        scan.resume_snapshot()["runtime"]["elapsed_seconds"]
        if hasattr(scan, "resume_snapshot")
        else None
    )

    pending_artifacts: dict[str, dict[str, Any]] = {}

    def commit_render(*args, **kwargs):
        if elapsed_before is not None:
            kwargs["elapsed_seconds"] = elapsed_before + time.monotonic() - render_started
        document_id = scan.commit_render(*args, **kwargs)
        if getattr(scan, "con", None) is not None and hasattr(scan, "path"):
            from seohead.storage import browser_artifacts

            page_url_id = scan.con.execute(
                "SELECT url_id FROM documents WHERE document_id=?", (document_id,)
            ).fetchone()[0]
            fetched = pending_artifacts.pop(args[0], {"ok": False})
            artifact_config = settings["rendering"]["artifacts"]
            item = browser_artifacts.save(
                scan.path,
                page_url_id,
                document_id,
                fetched,
                screenshots=artifact_config["screenshots"],
                console_errors=artifact_config["console_errors"],
            )
            scan.write_context([item])
        return document_id

    rendering_config = copy.deepcopy(settings["rendering"])
    saved_render = scan.read_context("render_elapsed") if hasattr(scan, "read_context") else None
    render_seconds_before = float(saved_render["seconds"]) if saved_render else 0.0
    maximum_render_seconds = float(rendering_config["escalation"].get("max_render_seconds", 0))
    # A killed process cannot measure its final interval. Refuse to silently
    # grant a fresh finite budget when the previous phase was left active.
    interrupted_render = bool(saved_render and saved_render["active"])
    exhausted = maximum_render_seconds > 0 and (
        interrupted_render or render_seconds_before >= maximum_render_seconds
    )
    if maximum_render_seconds > 0 and not exhausted:
        rendering_config["escalation"]["max_render_seconds"] = (
            maximum_render_seconds - render_seconds_before
        )

    def save_render_elapsed(active: bool) -> None:
        if hasattr(scan, "write_context"):
            seconds = render_seconds_before + max(0.0, time.monotonic() - render_started)
            scan.write_context(
                [
                    {
                        "kind": "render_elapsed",
                        "item_key": "run",
                        "payload_version": "scan_context.v1",
                        "payload_json": json.dumps(
                            {
                                "schema_version": "render_elapsed.v1",
                                "seconds": seconds,
                                "active": active,
                            }
                        ),
                        "completeness": "complete",
                        "reason": "",
                    }
                ]
            )

    start_url = (
        scan.con.execute("SELECT start_url FROM scan").fetchone()[0]
        if getattr(scan, "con", None) is not None
        else result.pages[0].url
    )
    from seohead.storage import MAX_RECORD_BYTES

    # Parsing is independently bounded from retention.  Body-off scans still
    # need a bounded DOM to produce findings, while a larger parse limit must
    # never exceed the atomic writer's 64 MiB input ceiling.
    max_parse_bytes = min(settings["limits"]["max_response_bytes"], 8 * MAX_RECORD_BYTES)
    max_retained_bytes = settings["storage"]["max_body_bytes"]
    gate_kwargs = {"request_gate": request_gate} if request_gate is not None else {}

    def fetch_browser(target: str) -> dict[str, Any]:
        from seohead.storage import browser_artifacts

        artifact_kwargs = {}
        if hasattr(scan, "path") and rendering_config["artifacts"]["screenshots"]:
            artifact_kwargs["artifacts_dir"] = str(browser_artifacts.staging_dir(scan.path))
        capture_config = rendering_config
        if _policy_facts(settings, target)["credentials_used"]:
            capture_config = copy.deepcopy(rendering_config)
            capture_config["artifacts"]["screenshots"] = False
            capture_config["artifacts"]["console_errors"] = False
        fetched = render_tool.render_document(
            target,
            capture_config,
            user_agent=settings["http"]["user_agent"],
            max_html_bytes=max_parse_bytes,
            policy_facts=_policy_facts(settings, target),
            proxy_route=proxy_route,
            **gate_kwargs,
            **artifact_kwargs,
        )
        pending_artifacts[target] = fetched
        return fetched

    if mode == "js":
        # The probe's unit is split so a concurrent run keeps every SQLite
        # touch on the orchestrator thread: probe_prepare does the disk
        # preflight and reads the retained static body (or commits its
        # absence and skips the fetch), probe does only the browser fetch on
        # a worker, and probe_apply commits the probe DOM and computes the
        # verdict back on the orchestrator thread. Sequential runs call the
        # same three hooks back-to-back, so nothing about the stored shape
        # changes.
        def probe_prepare(target: str) -> Any:
            scan.preflight_capture()
            try:
                raw_html = _static_html(scan, target, max_retained_bytes)
            except Exception:
                raw_html = ""
            if raw_html:
                return raw_html
            commit_render(
                target,
                None,
                html=None,
                renderer=_unknown_renderer(target, settings),
                captured_at=_now(),
                body_state="unavailable",
                body_reason="not_in_corpus",
                content_capture=_content_capture(
                    html=None,
                    parsed=None,
                    settings=settings,
                    unavailable_reason="retained static body is unavailable",
                ),
            )
            return {
                "verdict": {
                    "ok": False,
                    "needs_escalation": False,
                    "reason": "retained static body is unavailable",
                }
            }

        def probe(target: str) -> dict[str, Any]:
            return fetch_browser(target)

        def probe_apply(target: str, raw_html: Any, fetched: dict[str, Any]) -> dict[str, Any]:
            renderer = fetched.get("renderer")
            if not isinstance(renderer, dict):
                renderer = _unknown_renderer(target, settings)
            if not fetched.get("ok"):
                commit_render(
                    target,
                    None,
                    html=None,
                    renderer=renderer,
                    captured_at=_now(),
                    body_state="truncated"
                    if fetched.get("dom_state") == "truncated"
                    else "unavailable",
                    body_reason="truncated"
                    if fetched.get("dom_state") == "truncated"
                    else "fetch_failed",
                    content_capture=_content_capture(
                        html=None,
                        parsed=None,
                        settings=settings,
                        unavailable_reason=str(fetched.get("error") or "render failed"),
                    ),
                )
                return {"ok": False, "needs_escalation": False}
            # A probe DOM is an observation in its own right.  Store it before
            # comparing, then free it; a later accepted full render gets its
            # own document and page/graph update.
            commit_render(
                target,
                None,
                html=fetched.get("html"),
                renderer=renderer,
                captured_at=_now(),
                content_capture=_content_capture(
                    html=fetched.get("html"),
                    parsed=None,
                    settings=settings,
                    unavailable_reason="rendered probe was not parsed for content evidence",
                ),
            )
            raw = render_tool._snapshot(str(raw_html or ""), target, "raw")
            rendered_html = str(fetched.get("html") or "")
            final_url = str(fetched.get("final_url") or target)
            rendered = render_tool._snapshot(rendered_html, final_url, "rendered")
            shell = render_tool.detect_empty_shell(str(raw_html or ""))
            findings = render_tool.compare(raw, rendered, str(raw_html or ""), shell)
            needs = findings != [render_tool.ALL_CLEAR]
            if settings["rendering"]["escalation"].get("policy") == "auto":
                needs = needs or render_tool.little_text(raw)
            return {
                "ok": True,
                "needs_escalation": needs,
                "empty_shell": shell,
            }

        def render_prepare(target: str) -> None:
            scan.preflight_capture()

        def render_fetch(target: str) -> dict[str, Any]:
            return fetch_browser(target)

        representation = "rendered"
    else:
        # The legacy probe never fetches: it only reads the retained static
        # body and computes its verdict, so it is a prepare that returns a
        # terminal verdict and a probe that must never be scheduled.
        def probe_prepare(target: str) -> Any:
            scan.preflight_capture()
            try:
                html = _static_html(scan, target, max_retained_bytes)
            except Exception:
                html = ""
            return {
                "verdict": {
                    "ok": bool(html),
                    "needs_escalation": render_tool.legacy_fragment_target(target, html)
                    is not None,
                    "empty_shell": render_tool.detect_empty_shell(html),
                }
            }

        def probe(_target: str) -> dict[str, Any]:
            # Unreachable: a legacy probe always resolves inside
            # probe_prepare. A defensive failure, never a fabricated verdict.
            return {"ok": False, "error": "legacy probe was not prepared"}

        pending_html: dict[str, str] = {}

        def render_prepare(target: str) -> None:
            scan.preflight_capture()
            try:
                pending_html[target] = _static_html(scan, target, max_retained_bytes)
            except Exception:
                pending_html[target] = ""

        def render_fetch(target: str) -> dict[str, Any]:
            html = pending_html.pop(target, "")
            if not html:
                return {"ok": False, "url": target, "error": "raw document unavailable"}
            legacy_kwargs = {"request_gate": request_gate} if request_gate is not None else {}
            if proxy_route is not None:
                legacy_kwargs["proxy_route"] = proxy_route
            return _legacy_fetch(target, html, settings, scan, max_parse_bytes, **legacy_kwargs)

        representation = "legacy_fragment"

    def consume(target: str, fetched: dict[str, Any], label: str) -> dict[str, Any]:
        # A browser failure has no trustworthy renderer provenance to put in
        # the artifact.  The escalation summary still records the attempted
        # URL; static evidence remains selected.
        if not fetched.get("ok"):
            renderer = fetched.get("renderer")
            commit_render(
                target,
                None,
                html=None,
                renderer=renderer
                if isinstance(renderer, dict)
                else _unknown_renderer(target, settings),
                captured_at=str(fetched.get("captured_at") or _now()),
                representation=label,
                body_state="truncated"
                if fetched.get("dom_state") == "truncated"
                else "unavailable",
                body_reason="truncated"
                if fetched.get("dom_state") == "truncated"
                else "fetch_failed",
                captures=fetched.get("captures", ()) if label == "legacy_fragment" else (),
                content_capture=_content_capture(
                    html=None,
                    parsed=None,
                    settings=settings,
                    unavailable_reason=str(fetched.get("error") or "render failed"),
                ),
                route_coverage=(
                    {
                        "representation": label,
                        "observed": 0,
                        "omitted": 0,
                        "completeness": "unavailable",
                        "reason": str(fetched.get("error") or "render failed"),
                    }
                    if settings["rendering"]["rendered_links"]["store"]
                    else None
                ),
            )
            return {
                "accepted": False,
                "state": "unavailable",
                "reason": str(fetched.get("error") or "render failed"),
            }

        record = (
            result.pages.get(target)
            if hasattr(result.pages, "get")
            else next((page for page in result.pages if page.url == target), None)
        )
        if record is None:
            return {"accepted": False, "state": "unavailable", "reason": "static page is absent"}
        raw_source_links = _source_links(scan, target, result.links)
        candidate, parsed, degenerate = _candidate(
            record,
            target,
            fetched,
            label,
            raw_source_links,
            settings,
            max_parse_bytes,
        )
        if candidate is None or parsed is None:
            commit_render(
                target,
                None,
                html=fetched.get("html") if label == "rendered" else None,
                renderer=fetched.get("renderer") or _unknown_renderer(target, settings),
                captured_at=str(fetched.get("captured_at") or _now()),
                representation=label,
                captures=fetched.get("captures", ()) if label == "legacy_fragment" else (),
                content_capture=_content_capture(
                    html=fetched.get("html") if label == "rendered" else None,
                    parsed=None,
                    settings=settings,
                    unavailable_reason=(
                        "rendered body is degenerate"
                        if degenerate
                        else "rendered body is not parseable"
                    ),
                ),
                route_coverage=(
                    {
                        "representation": label,
                        "observed": 0,
                        "omitted": 0,
                        "completeness": "unavailable",
                        "reason": "rendered body is degenerate"
                        if degenerate
                        else "rendered body is not parseable",
                    }
                    if settings["rendering"]["rendered_links"]["store"]
                    else None
                ),
            )
            return {
                "accepted": False,
                "state": "unavailable",
                "reason": "rendered body is degenerate"
                if degenerate
                else "rendered body is not parseable",
            }
        links, forms, partial_reasons, route_observations, route_coverage, candidates, decisions = (
            _rendered_batch(
                parsed,
                target_url=target,
                depth=candidate.crawl_depth,
                settings=settings,
            )
        )
        captures = fetched.get("captures", ()) if label == "legacy_fragment" else ()
        from .sqlite_adapter import _resource_observations

        resource_observations = _resource_observations(candidate, parsed, captures, settings)
        if resource_observations.get("resources_omitted"):
            partial_reasons.append("resource_declarations_omitted")
        document_id = commit_render(
            target,
            dataclasses.asdict(candidate),
            html=fetched.get("html") if label == "rendered" else None,
            renderer=fetched.get("renderer") or {},
            captured_at=str(fetched.get("captured_at") or _now()),
            links=links,
            forms=forms,
            representation=label,
            captures=captures,
            partial_reasons=partial_reasons,
            content_capture=_content_capture(
                html=fetched.get("html") if label == "rendered" else None,
                parsed=parsed,
                settings=settings,
                canonical_target=candidate.canonical,
            ),
            route_observations=route_observations,
            route_coverage=route_coverage,
            candidates=candidates,
            decisions=decisions,
            **resource_observations,
        )
        state, reason = _document_state(scan, document_id)
        _apply_committed_result(record, candidate)
        if target == start_url:
            # Keep at most this one transient DOM for the current audit gate,
            # including when retention omitted its bytes by policy.
            result._rendered_start_html = fetched.get("html")
        return {"accepted": True, "state": state, "reason": reason}

    render_pages = result.pages
    if getattr(scan, "con", None) is not None:
        attempted = {
            row[0]
            for row in scan.con.execute(
                "SELECT u.url FROM documents d JOIN urls u ON u.url_id=d.url_id "
                "WHERE d.representation=?",
                (representation,),
            )
        }
        render_pages = (page for page in result.pages if page.url not in attempted)
        rendering_config["escalation"]["max_render_urls"] = max(
            0, rendering_config["escalation"]["max_render_urls"] - len(attempted)
        )
    if exhausted:
        outcome = render_escalation.EscalationResult(mode=mode, time_budget_exhausted=True)
    else:
        save_render_elapsed(True)
        try:
            outcome = render_escalation.escalate(
                render_pages,
                rendering_config,
                probe=probe,
                render_fetch=render_fetch,
                representation_label=representation,
                render_consumer=consume,
                probe_prepare=probe_prepare,
                probe_apply=probe_apply if mode == "js" else None,
                render_prepare=render_prepare,
            )
        except KeyboardInterrupt as exc:
            interrupted_outcome = getattr(exc, "render_outcome", None)
            if getattr(scan, "con", None) is not None and isinstance(
                interrupted_outcome, render_escalation.EscalationResult
            ):
                from seohead.storage import render_summary

                if interrupted_outcome.probe_requests or interrupted_outcome.render_requests:
                    render_summary.record(scan, interrupted_outcome)
            raise
        finally:
            save_render_elapsed(False)
    if (
        hasattr(scan, "write_context")
        and settings["rendering"]["rendered_links"]["store"]
        and (not hasattr(scan, "read_context") or scan.read_context(RUN_KIND) is None)
    ):
        partial = (
            outcome.render_budget_exhausted
            or outcome.time_budget_exhausted
            or bool(outcome.patterns_unprobed)
        )
        reason = (
            "render URL budget exhausted"
            if outcome.render_budget_exhausted
            else "render time budget exhausted"
            if outcome.time_budget_exhausted
            else "one or more patterns were not probed"
            if outcome.patterns_unprobed
            else ""
        )
        scan.write_context(
            [
                run_context(
                    "partial" if partial else "complete",
                    reason,
                    True,
                    mode,
                    len(result.pages),
                    sum(outcome.render_counts.values()),
                )
            ]
        )
    if getattr(scan, "con", None) is not None:
        from seohead.storage import render_summary

        if outcome.probe_requests or outcome.render_requests:
            render_summary.record(scan, outcome)
            prior = render_escalation.EscalationResult(mode=mode)
            prior.rendered = outcome.rendered
            prior.representations = outcome.representations
            prior.empty_shell_urls = outcome.empty_shell_urls
            prior.time_budget_exhausted = outcome.time_budget_exhausted
            return render_summary.aggregate(scan.con, prior)
        return render_summary.aggregate(scan.con, outcome)
    return outcome


__all__ = ["run_render_escalation"]
