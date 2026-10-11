"""Native SQLite analysis with bounded page and report materialization."""

from __future__ import annotations

import copy
import importlib.metadata
import json
import platform
import re
import sqlite3
import subprocess
from collections.abc import Callable
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from seohead import __version__
from seohead._build.provenance import BuildProvenanceError, packaged_provenance
from seohead.core.filesystem import file_sha256

# Legacy directory materialization only; native audit.v2 has no population bridge cap.
MAX_AUDIT_PAGES = 10_000
MAX_AUDIT_FORMS = 20_000
_SHA = re.compile(r"[0-9a-f]{40}\Z")
_POST_CAPTURE_INTERRUPT = "post-collection processing interrupted; retained capture can be resumed"

# scan.v1 (evidence_version crawl.v1) retains no robots.txt or sitemap document
# (that is child H/#381 scope), so these seven checks cannot be re-measured
# from stored evidence -- only from a new crawl. Pre-registered here, by name,
# so a reanalysis never silently drops them out of the coverage denominator
# while still reporting a score (#382).
UNMEASURABLE_OFFLINE_CHECKS = (
    "SITEMAP_NOT_IN_ROBOTS",
    "ROBOTS_BLOCKS_RESOURCES",
    "SITEMAP_FETCH_INCOMPLETE",
    "SITEMAP_TOO_MANY_URLS",
    "SITEMAP_TOO_LARGE",
    "SITEMAP_URL_DUPLICATED",
    "SITEMAP_STALE_LASTMOD",
)
_UNMEASURABLE_REASON = (
    "scan.v1 retains no robots.txt/sitemap document; re-measuring this check "
    "requires a new crawl, not a reanalysis of stored evidence"
)


def _installed_version(name: str) -> str:
    try:
        return importlib.metadata.version(name)
    except importlib.metadata.PackageNotFoundError:
        return "unknown"


def _producer_provenance(producer_build: str | None) -> tuple[str, str, dict[str, str]]:
    """Return explicit, clean-checkout, or validated packaged provenance."""
    if producer_build is not None:
        if not isinstance(producer_build, str) or not _SHA.fullmatch(producer_build):
            raise ValueError("producer_build must be a full lowercase Git commit SHA")
        revision = producer_build
    else:
        root = Path(__file__).resolve().parents[2]
        try:
            top = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "--show-toplevel"],
                capture_output=True,
                check=False,
                text=True,
                timeout=5,
            )
        except (OSError, subprocess.TimeoutExpired):
            top = None
        if top is not None and not top.returncode and Path(top.stdout.strip()).resolve() == root:
            status = subprocess.run(
                ["git", "-C", str(root), "status", "--porcelain"],
                capture_output=True,
                check=False,
                text=True,
                timeout=5,
            )
            revision_result = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "HEAD"],
                capture_output=True,
                check=False,
                text=True,
                timeout=5,
            )
            revision = revision_result.stdout.strip()
            if (
                status.returncode
                or revision_result.returncode
                or status.stdout
                or not _SHA.fullmatch(revision)
            ):
                raise ValueError(
                    "native scan provenance requires a clean source checkout; pass producer_build explicitly"
                )
        else:
            try:
                packaged = packaged_provenance()
            except BuildProvenanceError as exc:
                raise ValueError(
                    "no verified source checkout or packaged producer metadata; "
                    "pass producer_build explicitly"
                ) from exc
            if packaged.version != __version__:
                raise ValueError("packaged producer metadata version disagrees with this package")
            revision = packaged.revision
    return (
        __version__,
        revision,
        {
            "python": platform.python_version(),
            "sqlite": sqlite3.sqlite_version,
            "httpx": _installed_version("httpx"),
            "lxml": _installed_version("lxml"),
            "beautifulsoup4": _installed_version("beautifulsoup4"),
        },
    )


def _rebuild_page_result(scan, *, page_view: bool = False) -> Any:
    """Restore run context with either admitted rows or a SQLite page view."""
    from seohead.crawl.collect import CrawlResult, PageRecord
    from seohead.crawl.list_scan import is_list_scan
    from seohead.crawl.spider import SpiderResult
    from seohead.storage.exports import _page_rows

    result = CrawlResult() if is_list_scan(scan.con) else SpiderResult()
    result.pages = (
        _StoredPages(scan.con)
        if page_view
        else [PageRecord(**page) for page in _page_rows(scan.con)]
    )
    result.robots_blocked = _StoredContextURLs(scan.con, "robots_blocked_url")
    result.seed_urls = _StoredContextURLs(scan.con, "seed_url")
    summary_row = scan.con.execute(
        "SELECT payload_json FROM context_items WHERE kind='robots_summary' AND item_key='run'"
    ).fetchone()
    robots_summary = json.loads(summary_row[0]) if summary_row else None
    result.robots_note = robots_summary["note"] if robots_summary is not None else ""
    snapshot = scan.resume_snapshot()
    result.partial = bool(snapshot["scan"]["crawl_partial"])
    result.finish_reason = snapshot["scan"]["finish_reason"]
    result.stopped_reason = snapshot["scan"]["finish_reason"] if result.partial else ""
    result.max_depth_reached = snapshot["runtime"]["max_depth_reached"]
    result.crawl_delay_applied = snapshot["runtime"]["crawl_delay_applied"]
    result.effective_delay = snapshot["runtime"]["throttle"]["delay_seconds"]
    result.effective_concurrency = snapshot["runtime"]["throttle"]["concurrency"]
    result.limitations = json.loads(snapshot["scan"]["limitations_json"])
    return result


class _StoredContextURLs:
    """Count or iterate retained discovery identities without materializing them."""

    def __init__(self, con, kind):
        self.con, self.kind = con, kind

    def __len__(self):
        return self.con.execute(
            "SELECT COUNT(*) FROM context_items WHERE kind=?", (self.kind,)
        ).fetchone()[0]

    def __iter__(self):
        for row in self.con.execute(
            "SELECT u.url FROM context_items c JOIN urls u ON u.url_id=json_extract(c.payload_json,'$.url_id') WHERE c.kind=? ORDER BY c.item_key",
            (self.kind,),
        ):
            yield row[0]


class _StoredPages:
    """Re-iterable page view for rendering without a full PageRecord list."""

    def __init__(self, con):
        self.con = con

    def __len__(self):
        return self.con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]

    def __iter__(self):
        from seohead.crawl.collect import PageRecord
        from seohead.storage.exports import _page_rows

        return (PageRecord(**row) for row in _page_rows(self.con))

    def get(self, url: str):
        from seohead.crawl.collect import PageRecord
        from seohead.storage.exports import _page_rows

        row = next(iter(_page_rows(self.con, url=url)), None)
        return PageRecord(**row) if row is not None else None


def _response(run, *, audit_available: bool, audit_reason: str, finalized: bool) -> dict[str, Any]:
    response = {
        "scan": run.path,
        "urls_collected": run.pages,
        "links_collected": run.links,
        "forms_collected": run.forms,
        "partial": run.partial,
        "resumed": getattr(run, "resumed", False),
        "finish_reason": run.finish_reason,
        "audit_available": audit_available,
        "audit_reason": audit_reason,
        "finalized": finalized,
        "limitations": list(run.limitations),
        # Named in the result, not only as a "partial" capability flag: a corpus
        # reduced to a fifth of the pages the same document counts over is not a
        # caveat, and nothing else in the response says how much is gone (#647).
        "html_bodies": dict(run.html_bodies),
        # Same reasoning for the rendering lane: a rendered corpus reduced to
        # nothing by a cookie the site set is not a caveat either (#656).
        "rendered_bodies": dict(run.rendered_bodies),
    }
    if run.capabilities is not None:
        response.update(corpus_partial=run.corpus_partial, capabilities=run.capabilities)
    return response


def _bridge_reason(counts: dict[str, int], start_page_gate: dict[str, Any] | None) -> str | None:
    if start_page_gate is None:
        return (
            "start-page raw evidence was not retained; audit cannot reconstruct its rendering gate"
        )
    if set(start_page_gate) != {"html", "outlinks", "external_outlinks"} or not all(
        isinstance(start_page_gate[name], str if name == "html" else int)
        for name in start_page_gate
    ):
        return "start-page transient gate is invalid; audit is unavailable"
    return None


def _has_saved_audit(scan) -> bool:
    return scan.con.execute("SELECT 1 FROM audit WHERE singleton=1").fetchone() is not None


# NativeScan.note_audit_unavailable rejects a reason over this many chars, and
# neither the exception's own message nor a caller-chosen --scan-out path was ever
# bounded with that column in mind, so the assembled reason is truncated to fit
# after both are interpolated in, not before.
_AUDIT_FAILURE_REASON_MAX = 500


def _audit_failure_reason(exc: BaseException, run) -> str:
    """Name the audit phase and the surviving artifact instead of a bare exception.

    A ``KeyError``'s ``str()`` is just the key (see #627) -- everything an
    operator needs (that collection finished, how much of it, and where to
    re-analyse it) has to come from this message, because nothing else in
    the failure carries it.
    """
    reason = (
        f"audit phase failed unexpectedly ({type(exc).__name__}: {exc}); "
        f"collection finished with {run.pages} pages retained in {run.path} -- "
        "re-analyse it with `seohead scan-reanalyze`"
    )
    if len(reason) > _AUDIT_FAILURE_REASON_MAX:
        reason = reason[: _AUDIT_FAILURE_REASON_MAX - 1] + "…"
    return reason


def _interrupted_capture(run) -> dict[str, Any]:
    """Checkpoint only the returned capture, after owned phase contexts unwind."""
    from contextlib import closing
    from dataclasses import replace

    from seohead.storage import ScanError, open_scan
    from seohead.storage.native_scan import NativeScan

    snapshot = NativeScan.inspect(run.path)
    header, counts = snapshot["scan"], snapshot["counts"]
    retained = replace(
        run,
        pages=counts["pages"],
        links=counts["links"],
        forms=counts["forms"],
        lifecycle=header["lifecycle"],
        partial=bool(header["crawl_partial"]),
        finish_reason=header["finish_reason"],
    )
    if header["lifecycle"] in {"finished", "failed"}:
        # SIGINT may arrive after the final commit. Never reopen an immutable
        # artifact or replace its completed state with an invented interruption.
        try:
            with closing(open_scan(run.path)):
                pass
        except ScanError as exc:
            available, reason = False, str(exc)
        else:
            available, reason = True, "capture finalized before interruption"
        return _response(
            retained,
            audit_available=available,
            audit_reason=reason,
            finalized=header["lifecycle"] == "finished",
        )
    reason = _POST_CAPTURE_INTERRUPT
    with NativeScan.open(run.path) as scan:
        scan.interrupt("interrupted")
        scan.note_audit_unavailable(reason)
    return _response(
        replace(retained, lifecycle="interrupted", partial=True, finish_reason="interrupted"),
        audit_available=False,
        audit_reason=reason,
        finalized=False,
    )


def resume_inputs(scan_path: str) -> dict[str, Any]:
    """Read back what a resume must repeat: the scan's own start URL and settings.

    A resume is one crawl continuing, not a second crawl aimed at the same file.
    The frontier, scope and limits already in the artifact decide what is left to
    fetch, so they are read from it rather than restated on a command line, where
    a single changed value would either be refused by the writer's configuration
    fingerprint or -- worse -- continue one crawl under another crawl's rules.

    Read-only and before any request: every refusal below is decided from the
    file alone, so a mismatched artifact costs the target site nothing.
    """
    from seohead.storage.native_scan import NativeScan

    if not isinstance(scan_path, str) or not scan_path:
        raise ValueError("resume requires the path of a SQLite scan artifact")
    if not Path(scan_path).is_file():
        raise ValueError(f"scan to resume does not exist: {scan_path}")
    header = NativeScan.inspect(scan_path)["scan"]
    if header["source_kind"] != "native":
        raise ValueError(
            f"{scan_path} is a derived {header['source_kind']} artifact, not a crawl that can "
            "be resumed"
        )
    budget_stopped = header["lifecycle"] == "finished" and (
        header["finish_reason"] == "stopped_by_budget"
    )
    if header["lifecycle"] in {"finished", "failed"} and not budget_stopped:
        raise ValueError(
            f"{scan_path} is already {header['lifecycle']} "
            f"(finish reason: {header['finish_reason']}); there is nothing left to resume"
        )
    settings = json.loads(header["config_json"])
    if settings.get("http", {}).get("proxy"):
        raise ValueError(
            "proxied scans cannot be resumed from redacted route settings; start a new scan "
            "with the original proxy configuration"
        )
    if "max_requests" not in settings.get("limits", {}):
        # Pre-budget artifacts made no total-attempt promise. Keep that recorded
        # semantics on resume instead of silently applying a later default.
        settings.setdefault("limits", {})["max_requests"] = 0
    if "graph" not in settings.get("resources", {}):
        import copy

        from seohead.crawl.settings import DEFAULTS

        if "resources" not in settings:
            settings["resources"] = copy.deepcopy(DEFAULTS["resources"])
        else:
            settings["resources"]["graph"] = copy.deepcopy(DEFAULTS["resources"]["graph"])
    if "format_version" not in settings.get("storage", {}):
        settings.setdefault("storage", {})["format_version"] = "scan.v1"
    if settings.get("http", {}).get("credential_headers"):
        # The artifact stores credential references redacted, by design, so the
        # settings read back from it are not the settings the interrupted run
        # used. Continuing under them would crawl the same site unauthenticated
        # and report the result as a continuation of an authenticated crawl.
        raise ValueError(
            f"{scan_path} was crawled with credential headers, which it stores only in redacted "
            "form; a resume cannot restore them. Continue it with the original --config and "
            "--scan-out instead of --resume"
        )
    from contextlib import closing

    from seohead.crawl.list_scan import is_list_scan
    from seohead.storage import open_scan

    with closing(open_scan(scan_path, require_audit=False)) as con:
        list_mode = is_list_scan(con)
    return {
        "start_url": header["start_url"],
        "settings": settings,
        "writer_revision": header["writer_revision"],
        "config_fingerprint": header["config_fingerprint"],
        "list_mode": list_mode,
    }


def resume_scan(
    scan_path: str,
    *,
    url: str | None = None,
    producer_build: str | None = None,
    progress: Callable[[int, int], None] | None = None,
    progress_snapshot: Callable[[dict[str, int]], None] | None = None,
    observation: Callable[[str], None] | None = None,
    shared_request_gate: Callable[[], None] | None = None,
) -> dict[str, Any]:
    """Continue an interrupted native scan from its stored frontier and throttle state.

    Refuses, by name and before the first request, a scan written by a different
    producing build or for a different start URL: continuing evidence collected
    under one build's parser or from one start URL into another produces a single
    artifact that describes neither run.
    """
    inputs = resume_inputs(scan_path)
    _version, revision, _runtime = _producer_provenance(producer_build)
    if inputs["writer_revision"] != revision:
        raise ValueError(
            f"{scan_path} was written by build {inputs['writer_revision']}, and this is build "
            f"{revision}; refusing a mixed-build resume. Check out the build that wrote it, or "
            "pass producer_build with that SHA if this source tree is that build"
        )
    if inputs["list_mode"]:
        if url is not None:
            raise ValueError("explicit URL-list resume cannot take a crawl start URL")
        return crawl_list_scan(
            None,
            scan_out=scan_path,
            settings=inputs["settings"],
            producer_build=revision,
            out_dir=inputs["settings"]["output"]["dir"] or None,
        )
    if url is not None:
        from seohead.recon.net import normalize_url

        if normalize_url(url) != inputs["start_url"]:
            raise ValueError(
                f"{scan_path} was crawled from {inputs['start_url']}, not {url}; refusing to "
                "resume one crawl as another"
            )
    return crawl_site_scan(
        inputs["start_url"],
        scan_out=scan_path,
        settings=inputs["settings"],
        producer_build=revision,
        progress=progress,
        progress_snapshot=progress_snapshot,
        observation=observation,
        shared_request_gate=shared_request_gate,
    )


def _list_collection_options(settings):
    """One projection of list settings for the eager and disk-backed adapters."""
    return {
        "max_urls": settings["limits"]["max_urls"],
        "max_requests": settings["limits"]["max_requests"],
        "max_seconds": settings["limits"]["max_crawl_seconds"],
        "min_delay": settings["speed"]["min_delay_seconds"],
        "timeout": settings["http"]["timeout_seconds"],
        "credential_headers": settings["http"]["credential_headers"],
        "max_response_bytes": settings["limits"]["max_response_bytes"],
        "max_url_length": settings["limits"]["max_url_length"],
        "retry_on_timeout": settings["http"]["retry_on_timeout"],
        "user_agent": settings["http"]["user_agent"],
        "stop_after_consecutive_timeouts": settings["speed"]["stop_after_consecutive_timeouts"],
        "max_delay_seconds": settings["speed"]["max_delay_seconds"],
        "extra_request_headers": settings["http"]["headers"] or None,
        "adaptive": settings["speed"]["adaptive"],
        "robots_policy": settings["robots"]["policy"],
        "robots_token": settings["robots"]["user_agent_token"],
        "resolve_redirect_destination": settings["discovery"]["resolve_redirect_destination"],
        "resolve_canonical_destination": settings["discovery"]["resolve_canonical_destination"],
    }


def crawl_list_scan(
    urls, *, scan_out, settings, producer_build=None, out_dir=None, proxy_route=None
):
    """Retain exact explicit-list evidence and use the existing audit representation."""
    from seohead.crawl.list_scan import ListScan
    from seohead.mcp.handlers import (
        _audit_crawl_result,
        _retire_crawl_reports,
        _rewrite_pages_sidecar,
    )

    version, revision, runtime = _producer_provenance(producer_build)
    pages_path = None
    if out_dir:
        Path(out_dir).mkdir(parents=True, exist_ok=True)
        pages_path = str(
            Path(out_dir)
            / ("pages.jsonl" if settings["output"]["write_pages_jsonl"] else ".pages_resume.jsonl")
        )
    with ListScan(
        scan_out, urls, settings, version=version, revision=revision, runtime_versions=runtime
    ) as spool:
        if pages_path and spool.resumed:
            _rewrite_pages_sidecar(pages_path, spool.pages)
        result = spool.collect(
            **_list_collection_options(settings), out_path=pages_path, proxy_route=proxy_route
        )
        discovery = {
            "mode": "list",
            "directive_policy": settings["robots"]["policy"],
            "robots_blocked": len(result.robots_blocked),
            "canonical_destination_resolution": settings["discovery"][
                "resolve_canonical_destination"
            ],
        }
        response = {
            "scan": str(spool.scan.path),
            "urls_collected": len(result.pages),
            "partial": result.partial,
            "finish_reason": result.finish_reason,
            "stopped_reason": result.stopped_reason,
            "resumed": result.resumed,
            "discovery": discovery,
            "limitations": result.limitations,
            "out_dir": out_dir,
            "requests_used": spool.requests_used,
            "input_coverage": spool.scan.read_context("list_input")["counts"],
        }
        if out_dir and len(result.pages) > MAX_AUDIT_PAGES:
            reason = f"legacy audit materialization limit exceeded (pages={len(result.pages)}/{MAX_AUDIT_PAGES}); native scan and JSONL evidence are retained"
            spool.scan.note_audit_unavailable(reason)
            finalized = spool.scan.finish_capture(result.finish_reason)
            return {
                **response,
                "audit_available": False,
                "audit_reason": reason,
                "finalized": finalized,
                "stale_reports": _retire_crawl_reports(out_dir),
            }
        extra, audit = _audit_crawl_result(
            result,
            settings=settings,
            url=None,
            sitemap_seed={"sitemap_url": None, "sitemap_urls": [], "declared": []},
            discovery=discovery,
            out_dir=out_dir,
            pages_resume_path=pages_path,
            stored_scan=spool.scan,
            offline=True,
            streaming=out_dir is None,
        )
        if out_dir is None:
            header, collections = audit
            spool.scan.save_audit_v2(header, collections)
        else:
            spool.scan.save_audit(audit)
        finalized = spool.scan.finish_capture(result.finish_reason)
        return {
            **response,
            **extra,
            "scan": str(spool.scan.path),
            "audit_available": True,
            "audit_reason": "",
            "finalized": finalized,
        }


def crawl_site_scan(
    url: str,
    *,
    scan_out: str,
    settings: dict[str, Any],
    sitemap: str | None = None,
    sitemap_only: bool = False,
    producer_build: str | None = None,
    progress: Callable[[int, int], None] | None = None,
    progress_snapshot: Callable[[dict[str, int]], None] | None = None,
    observation: Callable[[str], None] | None = None,
    shared_request_gate: Callable[[], None] | None = None,
    proxy_route=None,
) -> dict[str, Any]:
    """Collect a native scan, then audit its SQL graph with finite page/output bounds.

    ``progress`` is forwarded to the collector untouched; see
    ``seohead.crawl.sqlite_adapter.crawl_to_scan``. It covers collection only --
    the audit that follows reads an artifact that has stopped growing, so a
    progress line over it would report a crawl that is already over.
    """
    if not isinstance(url, str) or not url:
        raise ValueError("url is required for a SQLite scan crawl")
    if sitemap_only and not sitemap:
        raise ValueError("sitemap_only requires an explicit sitemap")
    if not isinstance(scan_out, str) or not scan_out:
        raise ValueError("scan_out is required for a SQLite scan crawl")
    discovery = settings.get("discovery", {})
    external = discovery.get("external", {}) if isinstance(discovery, dict) else {}
    # Bridge callers from before discovery.external existed still inject the
    # small settings subset their audit needs.  The effective default is off;
    # only a validated modern config may opt into the separate external phase.
    external_crawl = bool(external.get("crawl", False)) if isinstance(external, dict) else False
    if external_crawl and settings["storage"]["format_version"] != "scan.v2":
        settings = copy.deepcopy(settings)
        settings["storage"]["format_version"] = "scan.v2"
    producer_version, producer_revision, runtime_versions = _producer_provenance(producer_build)
    sitemap_seed = {
        "sitemap_url": sitemap,
        "sitemap_urls": [sitemap] if sitemap else [],
        "declared": [],
    }

    from seohead.mcp.scan_sitemaps import initial_sitemaps, load_sitemaps

    def seed_loader(scan, emit_seeds, *, request_gate):
        load_sitemaps(
            scan,
            emit_seeds,
            settings=settings,
            result=sitemap_seed,
            request_gate=request_gate,
            proxy_route=proxy_route,
            start_url=url,
        )

    from seohead.crawl.sqlite_adapter import crawl_to_scan
    from seohead.storage.native_scan import NativeScan

    def announce(name: str) -> None:
        if observation is None:
            return
        try:
            observation(name)
        except (OSError, ValueError):
            return

    announce("collection")
    run = None
    _response_data = {}
    try:
        run = crawl_to_scan(
            url,
            scan_out=scan_out,
            settings=settings,
            producer_version=producer_version,
            producer_revision=producer_revision,
            runtime_versions=runtime_versions,
            initial_sitemaps=initial_sitemaps(
                sitemap, source="sitemap-only" if sitemap_only else "explicit"
            ),
            seed_loader=seed_loader,
            sitemap_only=sitemap_only,
            progress=progress,
            progress_snapshot=progress_snapshot,
            shared_request_gate=shared_request_gate,
            proxy_route=proxy_route,
        )
        external_summary = None
        if external_crawl:
            announce("external")
            from time import monotonic
            from urllib.parse import urlsplit

            from seohead.crawl.external import ExternalCheck, ExternalPolicy, run_external_checks
            from seohead.crawl.spider import Scope
            from seohead.crawl.sql_graph import StoredGraph
            from seohead.recon.net import http_client

            rules = Scope.from_config(settings["scope"])
            start_host = (urlsplit(url).hostname or "").lower()
            started = monotonic()
            client, _http2 = http_client(
                settings["http"]["timeout_seconds"],
                headers={"User-Agent": settings["http"]["user_agent"]},
            )
            try:
                with (
                    NativeScan.open(run.path) as external_scan,
                    StoredGraph(external_scan.con) as graph,
                ):
                    done = [
                        ExternalCheck.from_dict(item) for item in external_scan.external_checks()
                    ]

                    def emit(check: ExternalCheck) -> None:
                        external_scan.record_external_check(len(done), check.as_dict())
                        done.append(check)

                    external_summary = run_external_checks(
                        graph.iter_links(),
                        policy=ExternalPolicy.from_config(settings["external_checks"]),
                        is_internal=lambda target: rules.is_internal(target, start_host),
                        excluded_host=lambda target: bool(
                            any(
                                (urlsplit(target).hostname or "").lower() == host
                                or (urlsplit(target).hostname or "").lower().endswith("." + host)
                                for host in rules.exclude_hosts
                            )
                        ),
                        emit=emit,
                        done=done,
                        client=client,
                        headers_for_url=lambda target: {},
                        user_agent=settings["http"]["user_agent"],
                        max_response_bytes=settings["limits"]["max_response_bytes"],
                        retry_on_timeout=settings["http"]["retry_on_timeout"],
                        dispatch_gate=run.dispatch_gate,
                        time_exhausted=lambda: (
                            settings["limits"]["max_crawl_seconds"] > 0
                            and monotonic() - started >= settings["limits"]["max_crawl_seconds"]
                        ),
                    )
                    external_scan.record_external_checks_summary(external_summary)
            finally:
                client.close()
        if (
            settings.get("rendering", {}).get("rendered_links", {}).get("crawl", False)
            and settings.get("rendering", {}).get("mode", "raw") != "raw"
        ):
            announce("render")
            from dataclasses import replace

            from seohead.crawl.sqlite_render import run_render_escalation

            initial_start_page_gate = run.start_page_gate
            render_cycles = 0
            while True:
                with NativeScan.open(run.path) as rendered_scan:
                    rendered_result = SimpleNamespace(
                        pages=_StoredPages(rendered_scan.con), links=[]
                    )
                    run_render_escalation(
                        rendered_scan,
                        rendered_result,
                        settings,
                        request_gate=run.dispatch_gate.wait_turn
                        if run.dispatch_gate is not None
                        else None,
                        proxy_route=proxy_route,
                    )
                    queued_before = rendered_scan.resume_snapshot()["counts"]["queued"]
                if not queued_before or run.partial:
                    break
                prior_pages = run.pages
                run = crawl_to_scan(
                    url,
                    scan_out=scan_out,
                    settings=settings,
                    producer_version=producer_version,
                    producer_revision=producer_revision,
                    runtime_versions=runtime_versions,
                    progress=progress,
                    progress_snapshot=progress_snapshot,
                    shared_request_gate=shared_request_gate,
                    proxy_route=proxy_route,
                )
                render_cycles += 1
                run = replace(run, start_page_gate=initial_start_page_gate)
                if (
                    run.partial
                    or run.pages <= prior_pages
                    or render_cycles >= settings["rendering"]["escalation"]["max_render_urls"]
                ):
                    break
        with NativeScan.open(run.path) as scan:
            snapshot = scan.resume_snapshot(include_edges=True)
            stored = snapshot.get("scan", {})
            if (
                getattr(run, "resumed", False)
                and run.finish_reason == "finished"
                and stored.get("lifecycle") == "interrupted"
                and stored.get("finish_reason") == "interrupted"
                and not snapshot["counts"].get("queued", 0)
                and not snapshot["counts"].get("inflight", 0)
                and any(
                    note in json.loads(stored["limitations_json"])
                    for note in (
                        "audit unavailable: " + _POST_CAPTURE_INTERRUPT,
                        "audit unavailable: analysis interrupted; retained capture can be resumed",
                    )
                )
            ):
                # A drained frontier makes the collector skip begin_collection.
                # Explicit resume still owns the unfinished analysis/finalization;
                # reset only that stop while retaining measured omission flags.
                from dataclasses import replace

                scan.begin_collection()
                snapshot = scan.resume_snapshot(include_edges=True)
                run = replace(
                    run,
                    lifecycle=snapshot["scan"]["lifecycle"],
                    partial=bool(snapshot["scan"]["crawl_partial"]),
                    capabilities=json.loads(snapshot["scan"]["capabilities_json"]),
                )
            roots = scan.sitemap_roots()
            resumed_sitemap_only = bool(getattr(run, "resumed", False)) and any(
                root["source"] == "sitemap-only" for root in roots
            )
            sitemap_seed.update(
                sitemap_url=roots[0]["url"] if roots else None,
                sitemap_urls=[root["url"] for root in roots],
                declared=[],
            )
            counts = {table: snapshot["counts"][table] for table in ("pages", "links", "forms")}
            reason = _bridge_reason(counts, run.start_page_gate)
            if reason is not None and run.start_page_gate is None and _has_saved_audit(scan):
                finalized = scan.finish_capture(reason="finished")
                return _response(
                    run,
                    audit_available=True,
                    audit_reason="reused current saved audit after interrupted finalization",
                    finalized=finalized,
                )
            if reason is not None:
                scan.note_audit_unavailable(reason)
                finalized = scan.finish_capture(reason=run.finish_reason)
                return _response(
                    run, audit_available=False, audit_reason=reason, finalized=finalized
                )
            # The audit consumes the retained scan. Keep its first input layer on
            # disk instead of rebuilding a PageRecord list only for build_evidence
            # to immediately project it into the analyzer frame.
            result = _rebuild_page_result(scan, page_view=True)
            # ``_rebuild_page_result`` deliberately reconstructs only crawl-page
            # evidence.  The opt-in external phase is a separate, typed v2 table,
            # so attach its just-captured coverage explicitly before the shared
            # audit assembler writes the run header.
            result.external_summary = external_summary
            result.start_page_evidence = dict(run.start_page_gate)
            result.resumed = getattr(run, "resumed", False)
            result.finish_reason = run.finish_reason
            if result.partial and result.stopped_reason in {"running", "finished", ""}:
                result.stopped_reason = (
                    "; ".join(note for note in run.limitations if "observations_omitted" in note)
                    or "collection evidence is partial"
                )
            discovery = {
                "mode": "spider",
                "directive_policy": settings["robots"]["policy"],
                "robots_blocked": len(result.robots_blocked),
                "sitemap_url": sitemap_seed["sitemap_url"],
                "sitemap_urls": sitemap_seed["sitemap_urls"],
                "sitemap_seeded": len(result.seed_urls),
            }
            if external_summary is not None:
                discovery["external_checks"] = external_summary
            from dataclasses import replace

            from seohead.crawl.sql_sitemap import prepare_sitemap_reconciliation
            from seohead.mcp.handlers import _audit_crawl_result
            from seohead.storage.native_audit import AuditSizeError

            # Collection already committed its rows and closed successfully by this point
            # (``run`` above), so anything raised from here on is the *audit* misbehaving,
            # never the crawl -- and the 600-page artifact behind #627 proves collection's
            # own evidence survives an audit crash untouched. An operator reading a bare
            # exception (a KeyError's str() is just the key) has no way to tell that apart
            # from a lost crawl, so an unexpected failure here is named by phase and points
            # at the retained, re-analysable artifact instead of propagating as-is.
            try:
                announce("analysis")
                with prepare_sitemap_reconciliation(scan.con, start_url=url) as reconciliation:
                    _response_data, audit = _audit_crawl_result(
                        result,
                        settings=settings,
                        url=url,
                        sitemap_seed=sitemap_seed,
                        discovery=discovery,
                        out_dir=None,
                        pages_resume_path=None,
                        stored_scan=scan,
                        stored_sitemap=reconciliation,
                        # A resumed sitemap-only scan owns a complete retained XML
                        # population. Re-fetching the root would mix a changed live
                        # declaration into that resumed population.
                        offline=resumed_sitemap_only,
                        dispatch_gate=run.dispatch_gate,
                        proxy_route=proxy_route,
                        # Finding density is independent of URL count. Keep every
                        # retained-native audit streamed, including small dense scans.
                        streaming=True,
                    )

                if settings.get("rendering", {}).get("mode", "raw") != "raw":
                    from seohead.storage.corpus import rendered_body_retention

                    current = scan.resume_snapshot(include_edges=True)
                    run = replace(
                        run,
                        partial=bool(current["scan"]["crawl_partial"]),
                        links=current["counts"]["links"],
                        forms=current["counts"]["forms"],
                        limitations=tuple(json.loads(current["scan"]["limitations_json"])),
                        corpus_partial=bool(current["scan"]["corpus_partial"]),
                        capabilities=json.loads(current["scan"]["capabilities_json"]),
                        # Rendered documents are written by the escalation that just
                        # ran, not by the collector, so this is the first point where
                        # the corpus can be asked how many DOMs it kept (#656).
                        rendered_bodies=rendered_body_retention(scan.con),
                    )

                if isinstance(audit, tuple):
                    header, collections = audit
                    scan.save_audit_v2(header, collections)
                else:  # compatibility for injected audit bridges
                    scan.save_audit(audit)
            except AuditSizeError as exc:
                reason = str(exc)
                scan.note_audit_unavailable(reason)
                finalized = scan.finish_capture(reason=run.finish_reason)
                return _response(
                    run, audit_available=False, audit_reason=reason, finalized=finalized
                )
            except Exception as exc:
                reason = _audit_failure_reason(exc, run)
                scan.note_audit_unavailable(reason)
                finalized = scan.finish_capture(reason=run.finish_reason)
                return _response(
                    run, audit_available=False, audit_reason=reason, finalized=finalized
                )
            announce("finalizing")
            finalized = scan.finish_capture(reason=run.finish_reason)
        _response_data.update(
            _response(run, audit_available=True, audit_reason="", finalized=finalized)
        )
        return _response_data
    except KeyboardInterrupt:
        # The collector owns its interruption/checkpoint context. Only a returned
        # capture belongs to this post-collection boundary, never an arbitrary
        # pre-existing --scan-out path from an interrupted admission.
        if run is None:
            raise
        interrupted = _interrupted_capture(run)
        return {**_response_data, **interrupted} if interrupted["finalized"] else interrupted


def reanalyze_scan(
    scan_in: str,
    *,
    producer_build: str | None = None,
) -> dict[str, Any]:
    """Re-run today's parser/check registry over a scan.v1 already on disk.

    Offline by construction: the shared audit assembly receives the retained
    start URL so it can replay graph and form checks, but ``offline=True``
    prevents render escalation and names the sitemap/robots checks that have
    no retained source document. No HTTP, DNS, or browser request is
    dispatched on this path.

    ``scan_in`` is opened only through a private working copy -- the
    original file is hashed up front and never reopened for writing, so its
    bytes (and that hash) are provably unchanged by this call.
    """
    if not isinstance(scan_in, str) or not scan_in:
        raise ValueError("scan_in is required")
    source_path = Path(scan_in)
    if not source_path.is_file():
        raise ValueError(f"scan_in does not exist: {scan_in}")
    producer_version, producer_revision, runtime_versions = _producer_provenance(producer_build)
    source_sha256 = file_sha256(str(source_path))

    import shutil
    import tempfile
    import uuid as uuid_mod

    from seohead.crawl.sql_sitemap import prepare_sitemap_reconciliation
    from seohead.mcp.handlers import _audit_crawl_result
    from seohead.storage import ScanError, open_scan
    from seohead.storage.native_scan import NativeScan

    # The original audit (if any), read through the validated read-only path --
    # never the writer -- purely to report how this reanalysis' coverage
    # compares to what the original run measured.
    original_audit: dict[str, Any] | None = None
    original_scan_uuid: str | None = None
    try:
        con = open_scan(scan_in, require_audit=False)
        try:
            scan_row = con.execute("SELECT scan_uuid FROM scan WHERE singleton=1").fetchone()
            original_scan_uuid = scan_row["scan_uuid"] if scan_row else None
            audit_row = con.execute("SELECT document_json FROM audit WHERE singleton=1").fetchone()
            if audit_row is not None:
                original_audit = json.loads(audit_row["document_json"])
        finally:
            con.close()
    except ScanError as exc:
        return {
            "reanalysis": True,
            "source_scan": scan_in,
            "source_sha256": source_sha256,
            "audit_available": False,
            "audit_reason": f"cannot read source scan: {exc}",
        }

    with tempfile.TemporaryDirectory(prefix="seohead-reanalyze-") as tmpdir:
        work_copy = str(Path(tmpdir) / "scan.sqlite")
        shutil.copyfile(scan_in, work_copy)
        with NativeScan.open(work_copy) as scan:
            snapshot = scan.resume_snapshot()
            config = json.loads(snapshot["scan"]["config_json"])
            start_url = snapshot["scan"]["start_url"]
            result = _rebuild_page_result(scan)
            result.start_page_evidence = {}
            result.resumed = False
            with prepare_sitemap_reconciliation(
                scan.con, start_url=start_url or ""
            ) as reconciliation:
                _response_data, audit = _audit_crawl_result(
                    result,
                    settings=config,
                    url=start_url,
                    sitemap_seed={"sitemap_url": None, "sitemap_urls": [], "declared": []},
                    discovery={
                        "mode": "spider",
                        "directive_policy": config["robots"]["policy"],
                        "robots_blocked": len(result.robots_blocked),
                    },
                    stored_scan=scan,
                    stored_sitemap=reconciliation,
                    offline=True,
                    captured_render_summary=(original_audit or {})
                    .get("run", {})
                    .get("render_escalation"),
                )

    audit["run"]["input_mode"] = "reanalysis"
    audit["run"]["source"] = start_url or audit["run"].get("source")

    coverage = audit.get("summary", {}).get("check_coverage", {})
    original_coverage = (
        (original_audit or {}).get("summary", {}).get("check_coverage", {})
        if original_audit
        else None
    )

    return {
        "reanalysis": True,
        "reanalysis_uuid": str(uuid_mod.uuid4()),
        "source_scan": scan_in,
        "source_sha256": source_sha256,
        "parent_scan_uuid": original_scan_uuid,
        "analyzer_version": producer_version,
        "analyzer_revision": producer_revision,
        "runtime_versions": runtime_versions,
        "generated_at": audit["run"]["generated_at"],
        "unmeasurable_checks": list(UNMEASURABLE_OFFLINE_CHECKS),
        "unmeasurable_reason": _UNMEASURABLE_REASON,
        "coverage": coverage,
        "original_coverage": original_coverage,
        "original_audit_available": original_audit is not None,
        "audit_available": True,
        "audit": audit,
    }
