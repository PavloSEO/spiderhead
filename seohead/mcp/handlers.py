"""Shared handler layer over the core, used by both the CLI and local stdio MCP server.

Each function takes/returns plain JSON-serializable objects (headless). Add new
behavior to the core + a handler here, then surface it in each face.
"""

from __future__ import annotations

import contextlib
import sqlite3
import sys
import tempfile
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

from seohead.checks import (
    asset_weight,
    clusterer,
    downloader,
    optimizer,
    parser,
    sitemap,
)
from seohead.checks import (
    headers as headers_core,
)
from seohead.checks import (
    hreflang as hreflang_core,
)
from seohead.checks import (
    links as links_core,
)
from seohead.checks import (
    robots as robots_core,
)
from seohead.core import runlog
from seohead.core.filesystem import atomic_write_bytes
from seohead.core.models import ParseManyResult, RobotsCheckResult

LARGE_EVIDENCE_JOIN_SCAN_PAGES = 100_000
MAX_CONTENT_SEARCH_PACKAGE_BYTES = 512 * 1024 * 1024
MAX_CONTENT_SEARCH_MANIFEST_BYTES = 1024 * 1024


def handler_failed(result: Any) -> bool:
    """A handler reports its own failure to fetch, parse, or reach a provider via ``ok: False``
    in the returned dict, rather than raising (see ``docs/ARCHITECTURE.md``'s "the network never
    kills a tool" invariant). The CLI and the MCP server both call this instead of re-deriving
    the check, so the two surfaces cannot drift on what counts as a failure — see the exit-code
    contract in ``docs/USAGE.md``.
    """
    return isinstance(result, dict) and result.get("ok") is False


def _warn_ignored_robots(settings: dict[str, Any], url: str | None, urls: list[str] | None) -> None:
    """Make an explicit robots bypass visible at each CLI or MCP run boundary."""
    from seohead.recon.net import normalize_url

    if settings["robots"]["policy"] != "ignore":
        return
    if not url and (not isinstance(urls, (list, tuple)) or len(urls) > 1000):
        print(
            "warning: robots.txt bypass enabled for the explicit URL list (policy: ignore)",
            file=sys.stderr,
        )
        return
    targets = [url] if url else (urls or [])
    hosts = sorted(
        {(urlsplit(normalize_url(target)).hostname or "").lower() for target in targets if target}
    )
    for host in hosts:
        if host:
            print(
                f"warning: robots.txt bypass enabled for {host} (policy: ignore)", file=sys.stderr
            )


def _retire_crawl_reports(out_dir):
    """Keep old reports accessible while withholding their stale clean filenames."""
    import os

    retained_reports = {}
    for name in ("audit.json", "tasks.json", "tasks.md"):
        previous = Path(out_dir) / name
        if os.path.lexists(previous):
            retained = previous.with_name(f".{name}.stale-{time.time_ns()}")
            if os.path.lexists(retained):
                raise FileExistsError(f"stale report destination already exists: {retained}")
            os.rename(previous, retained)
            retained_reports[name] = str(retained)
    return retained_reports


def _default_scan_path(url: str, producer_build: str | None) -> str:
    """Reserve a caller-local, collision-safe scan name after provenance validates."""
    import os
    import uuid
    from pathlib import Path

    from seohead.mcp.scan_handlers import _producer_provenance
    from seohead.storage.history import new_scan_path

    # Do this before making the caller-local directory: a bad/unknown producer
    # must not leave a plausible-looking output location behind.
    _producer_provenance(producer_build)
    directory = Path.cwd() / "scans"
    if os.path.lexists(directory) and directory.is_symlink():
        raise ValueError(
            "default scans directory must not be a symlink; pass --scan-out explicitly"
        )
    if not directory.exists():
        directory.mkdir()
    if not directory.is_dir():
        raise ValueError(
            "default scans path exists but is not a directory; pass --scan-out explicitly"
        )
    return str(new_scan_path(directory, url, str(uuid.uuid4())))


# SEO core is extracted BY DEFAULT (the caller can turn any field off with False).
DEFAULT_PARSE_OPTIONS: dict[str, bool] = {
    "meta": True,
    "canonical": True,
    "og": True,
    "headings": True,
    "jsonld": True,
    "links": True,
    "text": True,
}


def parse(
    url: str | None = None, urls: list[str] | None = None, options: dict[str, Any] | None = None
) -> ParseManyResult:
    targets = urls if isinstance(urls, list) else ([url] if url else [])
    if not targets:
        raise ValueError("url or urls[] required")
    opts = {**DEFAULT_PARSE_OPTIONS, **(options or {})}
    results = [parser.parse_url(str(u), opts) for u in targets]
    return {"count": len(results), "results": results}


def redirects_generate(
    redirects: list[dict] | None = None,
    fmt: str = "apache-rewrite-rule",
    default_url: str = "/",
    custom_template: str = "",
) -> dict[str, Any]:
    from seohead.checks import redirects as redirects_core

    items = redirects if isinstance(redirects, list) else []
    return {
        "rules": redirects_core.generate_rules(items, fmt, default_url, custom_template),
        "analysis": redirects_core.analyze_redirect_map(items, default_url),
    }


def redirects_check(
    url: str | None = None, options: dict[str, Any] | None = None
) -> dict[str, Any]:
    if not url:
        raise ValueError("url required")
    if options is not None and not isinstance(options, dict):
        raise ValueError("redirects-check options must be an object")
    method = str((options or {}).get("method") or "HEAD").upper()
    if method not in {"GET", "HEAD"}:
        raise ValueError("redirects-check method must be GET or HEAD")
    from seohead.checks import redirects as redirects_core

    return {"chain": redirects_core.check_chain(url, options or {})}


def sitemap_crawl(
    url: str | None = None, concurrency: int = 3, project: str | None = None
) -> dict[str, Any]:
    """Expand one sitemap, or discover one from a site root through robots.txt.

    A root reads ``robots.txt`` first and uses its ``Sitemap:`` declarations;
    when none are usable, it tries ``/sitemap.xml``.  The result is failed only
    when no sitemap document can be parsed. Partial results keep ``ok: true``
    and name failed sources in ``errors``.
    """
    if not url:
        raise ValueError("url required")
    if project is None:
        return sitemap.crawl(url, concurrency)
    from hashlib import sha256
    from json import dumps

    from seohead.projects.run_observation import finish, finish_sitemap, start
    from seohead.projects.workspace import open_project

    opened = open_project(project)
    source = urlsplit(url)
    target = urlsplit(opened["project"]["site"]["target"])
    if (
        source.scheme not in {"http", "https"}
        or source.username is not None
        or source.password is not None
        or source.query
        or source.fragment
        or (source.scheme, source.hostname, source.port)
        != (target.scheme, target.hostname, target.port)
    ):
        raise ValueError(
            "observed sitemap URL must share the project origin without credentials, query or fragment"
        )
    root = opened["path"]
    run = start(
        root,
        kind="sitemap",
        mode="sitemap",
        max_urls=0,
        config_fingerprint=sha256(
            dumps({"url": url, "concurrency": concurrency}, sort_keys=True).encode()
        ).hexdigest(),
        artifact=None,
        counters={"fetched": None, "queued": None, "inflight": None, "excluded": None},
    )
    try:
        result = sitemap.crawl(url, concurrency)
    except BaseException:
        finish(root, run["id"], state="failed", reason="sitemap collection interrupted or failed")
        raise
    finish_sitemap(root, run["id"], result)
    return result


def _seed_urls_from_sitemap(
    url: str,
    sitemap: str | None,
    auto_discover: bool,
    *,
    request_gate: Callable[[], None] | None = None,
    robots_token: str = "*",
    throttle=None,
    proxy_route=None,
) -> dict[str, Any]:
    """Resolve and expand the sitemap(s) that should seed a crawl, if any.

    Returns ``{"sitemap_url": <first source, or None>, "sitemap_urls": [...],
    "declared": [...]}``. An explicit ``sitemap`` wins and is the sole source;
    otherwise, with ``auto_discover``, robots.txt can declare more than one
    ``Sitemap:`` directive and every one of them is independent (RFC-wise there
    is no "the" sitemap), so all are fetched and their URLs unioned. Neither
    given means no seeding — the crawl behaves exactly as it did before this
    feature existed.
    """
    from seohead.checks import sitemap as sitemap_tool

    if sitemap:
        targets = [sitemap]
        skipped: list[str] = []
    elif auto_discover:
        from urllib.parse import urljoin, urlsplit, urlunsplit

        from seohead.checks.robots import check_robots

        options = {"request_gate": request_gate} if request_gate is not None else {}
        if proxy_route is not None:
            options["proxy_route"] = proxy_route
        checked = check_robots(url, **options)
        # A robots.txt ``Sitemap:`` line may be relative (``Sitemap: /sitemap.xml``);
        # it resolves against the crawled site root, and one that still does not
        # make an absolute http(s) URL is skipped with a record, never an abort (#1552).
        parts = urlsplit(url)
        base = (
            urlunsplit((parts.scheme, parts.netloc, "/", "", ""))
            if parts.scheme and parts.netloc
            else url
        )
        targets, skipped = [], []
        for declared in checked.get("sitemaps") or []:
            resolved = urljoin(base, str(declared).strip())
            resolved_parts = urlsplit(resolved)
            if resolved_parts.scheme in ("http", "https") and resolved_parts.netloc:
                targets.append(resolved)
            else:
                skipped.append(str(declared))
        if throttle is not None and checked.get("ok") and isinstance(checked.get("groups"), list):
            from seohead.checks.robots import politeness_delay

            asked = politeness_delay(
                {"groups": checked["groups"], "sitemaps": targets}, robots_token
            )
            if asked is not None and asked > throttle.min_delay:
                throttle.min_delay = asked
                throttle.delay = max(throttle.delay, asked)
    else:
        targets = []
        skipped = []
    if not targets:
        return {
            "sitemap_url": None,
            "sitemap_urls": [],
            "declared": [],
            "skipped_sitemaps": skipped,
        }
    declared: list[str] = []
    seen: set[str] = set()
    for target in targets:
        options = {"request_gate": request_gate} if request_gate is not None else {}
        if proxy_route is not None:
            options["proxy_route"] = proxy_route
        expanded = sitemap_tool.crawl(target, **options)
        for entry in expanded.get("urls") or []:
            loc = entry.get("loc")
            if loc and loc not in seen:
                seen.add(loc)
                declared.append(loc)
    return {
        "sitemap_url": targets[0],
        "sitemap_urls": targets,
        "declared": declared,
        "skipped_sitemaps": skipped,
    }


def _run_render_escalation(
    result: Any,
    rendering_config: dict[str, Any],
    settings: dict[str, Any],
    *,
    request_gate: Callable[[], None] | None = None,
    proxy_route=None,
) -> Any:
    """Bind the escalation orchestrator to a real probe and re-fetch.

    This is the interface layer's own job, same as ``crawl_site`` above:
    ``seohead.crawl.render_escalation`` stays free of Playwright and the
    network so it can be unit-tested with fake callables, and this function
    is what supplies the real ones for an actual run. "js" mode reuses
    ``render_check`` for the sample probe -- the raw-versus-rendered
    comparison #18 asks for reuse of -- and the cheaper ``render_document``
    for the full re-fetch of every page an escalated pattern contains.
    "legacy_fragment" mode needs no browser at all: probing and re-fetching
    are both a plain HTTP GET.
    """
    import os

    from seohead.checks import render as render_tool
    from seohead.crawl import render_escalation
    from seohead.recon.net import http_client, validate_url

    mode = rendering_config["mode"]
    browser_cfg = rendering_config["browser"]
    timeout = settings["http"]["timeout_seconds"]
    artifacts_dir = (
        os.path.join(settings["output"]["dir"], "render_artifacts")
        if settings["output"]["dir"]
        else None
    )

    if mode == "js":
        gate_kwargs = {"request_gate": request_gate} if request_gate is not None else {}
        try:
            effective_viewport = render_tool.resolve_viewport(browser_cfg)
        except ValueError:
            effective_viewport = None
        probe_transport_kwargs = {}
        if browser_cfg.get("transport", "local") == "remote":
            probe_transport_kwargs["transport_config"] = {
                name: browser_cfg[name]
                for name in (
                    "transport",
                    "remote_protocol",
                    "remote_endpoint_env",
                    "remote_playwright_version",
                )
            }
        if proxy_route is not None:
            gate_kwargs["proxy_route"] = proxy_route

        def probe(target: str) -> dict[str, Any]:
            # The probe launches the same engine, size and emulation the full
            # render will use, so a pattern is not escalated by a browser that
            # differs from the one producing its evidence.
            probed = render_tool.render_check(
                target,
                timeout=timeout,
                wait=browser_cfg["wait_until"],
                viewport=browser_cfg["viewport"],
                engine=browser_cfg.get("engine", "chromium"),
                viewport_size=effective_viewport,
                mobile_emulation=bool(browser_cfg.get("mobile_emulation")),
                touch_emulation=bool(browser_cfg.get("touch_emulation")),
                **gate_kwargs,
                **probe_transport_kwargs,
            )
            verdict = probed.get("js_dependent")
            if verdict is None and probed.get("ok"):
                # render_check reached no verdict -- the DOM was read at an
                # earlier milestone than the one requested, so "no difference"
                # may only mean "no scripts had run yet" (#642). bool(None) is
                # False, which escalate() would read as a measured "this pattern
                # needs no rendering". Hand it the failed-probe shape instead, so
                # the pattern lands in patterns_unprobed with a reason (#626).
                reason = (probed.get("findings") or [""])[0] or "the probe reached no verdict"
                probed = dict(probed, ok=False, error=reason)
            needs = bool(verdict)
            if rendering_config.get("escalation", {}).get("policy") == "auto" and probed.get("ok"):
                needs = needs or render_tool.little_text(probed.get("raw") or {})
            probed["needs_escalation"] = needs
            return probed

        def render_fetch(target: str) -> dict[str, Any]:
            # settings["http"]["user_agent"] is what the static crawl fetched
            # every other page with, and render_document falls back to the
            # toolkit's own default when it is empty -- the same resolution
            # collect.py applies, so both halves of one crawl present one
            # identity to the origin.
            return render_tool.render_document(
                target,
                rendering_config,
                artifacts_dir=artifacts_dir,
                user_agent=settings["http"]["user_agent"],
                **gate_kwargs,
            )

        label = "rendered"
    else:  # legacy_fragment

        def _fetch_raw(target: str) -> str:
            try:
                validate_url(target)
            except ValueError:
                return ""
            options = {}
            if request_gate is not None:
                options["event_hooks"] = {"request": [lambda _request: request_gate()]}
            from seohead.recon.net import crawl_transport_options

            client, _ = http_client(timeout, **crawl_transport_options(proxy_route), **options)
            try:
                return client.get(target).text
            except Exception:
                return ""
            finally:
                client.close()

        def probe(target: str) -> dict[str, Any]:
            html = _fetch_raw(target)
            return {
                "ok": bool(html),
                "needs_escalation": render_tool.legacy_fragment_target(target, html) is not None,
                "empty_shell": render_tool.detect_empty_shell(html),
            }

        def render_fetch(target: str) -> dict[str, Any]:
            html = _fetch_raw(target)
            escaped = render_tool.legacy_fragment_target(target, html)
            if not escaped:
                return {"ok": False}
            fetched_html = _fetch_raw(escaped)
            if not fetched_html:
                return {"ok": False}
            return {"ok": True, "url": target, "final_url": escaped, "html": fetched_html}

        label = "legacy_fragment"

    return render_escalation.escalate(
        result.pages,
        rendering_config,
        probe=probe,
        render_fetch=render_fetch,
        representation_label=label,
    )


def _sitemap_comparable_pages(result: Any, start_url: str) -> list[str]:
    """The pages an XML sitemap of this site is supposed to declare.

    A sitemap lists indexable HTML pages of one host. Comparing it against every link
    destination the crawl recorded answers a different question and answers it wrongly: on a
    live 124-page site that comparison produced 392 URL_NOT_IN_SITEMAP findings — 307 .jpg and
    55 .webp files the gallery links to directly, five off-host links, and 30 URLs the crawl
    never fetched — which was 74% of the entire report (issue #94). So the population is built
    here from what the crawl actually fetched:

    * fetched at all, with a real status — a URL nobody requested is evidence of nothing;
    * 2xx — a 404 or a redirect is not a page a sitemap should declare;
    * HTML by its own Content-Type — not an image, a PDF or a feed;
    * on the start URL's host — a sitemap may not declare someone else's domain;
    * indexable — a noindex page (meta robots or X-Robots-Tag) is deliberately excluded,
      and so is one this same crawl's own evidence marks ``robots_blocked`` (#316):
      ``build_evidence()``'s ``_indexability()`` already projects a robots-blocked URL as
      non-indexable, and a report-only robots policy still fetches and links the page, so
      leaving it comparable here let one page be simultaneously non-indexable (BLOCKED_BY_ROBOTS)
      and reported as an indexable page the sitemap forgot (URL_NOT_IN_SITEMAP) -- two
      first-class projections of the same crawl contradicting each other.

    The 2xx+HTML re-check below looks like the gap ``AuditContext.html_pages`` had before
    issue #133 (it isn't calling that method), but ``result.pages`` here are
    ``seohead.crawl.collect.PageRecord`` from a native crawl, not ``sf.core.models.Page`` — a
    different type with no ``AuditContext`` in scope at this point, and this population also
    needs the host and indexable filters ``html_pages`` never applied. Fixing #133 narrowed
    ``html_pages`` to match this function's already-correct logic; it did not make this
    re-check redundant, since there is no shared call to route through.
    """
    from urllib.parse import urlsplit

    from seohead.checks.parser import robots_directives
    from seohead.recon.net import normalize_url as _normalize_host_url

    try:
        host = urlsplit(_normalize_host_url(start_url)).netloc.lower()
    except Exception:
        host = ""

    robots_blocked = set(getattr(result, "robots_blocked", None) or [])

    comparable: list[str] = []
    for page in getattr(result, "pages", []) or []:
        status = getattr(page, "status_code", None)
        if status is None or not (200 <= status < 300):
            continue
        if not getattr(page, "is_html", False):
            continue
        if host and urlsplit(page.url).netloc.lower() != host:
            continue
        if "noindex" in robots_directives(page.meta_robots, page.x_robots):
            continue
        if page.url in robots_blocked:
            continue
        comparable.append(page.url)
    return comparable


def _rewrite_pages_sidecar(path: str, pages: list[Any]) -> None:
    """Replace the streamed page sidecar with every page's current field values.

    ``path`` is the same file the spider appended to line-by-line as pages were
    fetched (``pages_resume_path`` in ``crawl_site``), written before render
    escalation exists to mutate ``result.pages`` in place. Called only once
    escalation has actually re-fetched something, so the file catches up to
    whatever changed rather than being rewritten on every run for nothing.
    Written to a temp file in the same directory first and swapped in with
    ``os.replace``, which POSIX and Windows both guarantee is atomic within one
    filesystem, so a process killed mid-write leaves the previous, still
    internally-consistent version in place rather than a half-written file
    (issue #244's third acceptance criterion).
    """
    import contextlib
    import dataclasses
    import json
    import os
    import tempfile

    directory = os.path.dirname(os.path.abspath(path)) or "."
    fd, tmp_path = tempfile.mkstemp(dir=directory, prefix=".pages-rewrite-")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            for page in pages:
                fh.write(json.dumps(dataclasses.asdict(page)) + "\n")
        os.replace(tmp_path, path)
    except BaseException:
        with contextlib.suppress(FileNotFoundError):
            os.remove(tmp_path)
        raise


def _segment_counts(
    pages,
    issues,
    scope_config: dict[str, Any],
    analysis_segments: list[dict[str, Any]] | None = None,
) -> dict[str, dict[str, int]]:
    """Page and issue counts per named segment (#358).

    Every page and every issue -- including an audit-wide finding with no
    single ``target_url``, such as ``TITLE_TEMPLATED`` -- is assigned to
    exactly one bucket: a declared segment, or the built-in "default", so
    these counts always sum to the ungrouped totals reported elsewhere in
    the same audit (#441).

    The bucketing itself is delegated to ``sf.core.segments.segment_report``,
    the tested engine that already carries this exact invariant (#456),
    rather than re-deriving it here with ``Scope.segment_for`` alone, which
    only ever looked at issues that had a ``target_url``.
    """
    from urllib.parse import urlsplit

    from seohead.crawl.spider import Scope
    from seohead.sf.core.segments import UNSEGMENTED, assign_segments

    scope_rules = Scope.from_config(scope_config)
    engine_segments = list(analysis_segments or ())
    if not engine_segments:
        engine_segments = [
            {
                "name": rule.name,
                "rules": [
                    r
                    for r in (
                        {"op": "prefix", "field": "path", "value": rule.prefix}
                        if rule.prefix
                        else None,
                        {"op": "eq", "field": "host", "value": rule.host} if rule.host else None,
                        {"op": "regex", "field": "url", "value": rule.pattern.pattern}
                        if rule.pattern
                        else None,
                    )
                    if r is not None
                ],
            }
            for rule in scope_rules.segments
        ]
    if not engine_segments:
        return {}

    def record(page: Any) -> dict[str, Any]:
        url = page if isinstance(page, str) else page.url
        parts = urlsplit(url)
        values = dict(vars(page)) if not isinstance(page, str) else {}
        return {
            **values,
            "url": url,
            "path": parts.path,
            "host": (parts.hostname or "").lower(),
        }

    def bucket(name: str | None) -> dict[str, int]:
        bucket_name = "default" if name in (None, UNSEGMENTED) else name
        return counts.setdefault(bucket_name, {"pages": 0, "issues": 0})

    counts: dict[str, dict[str, int]] = {}

    # Pages decide segment membership first, so dependency rules see the full
    # collected population. An issue targeting a collected page reuses that
    # assignment below; an audit-only target is evaluated on its own evidence.
    if analysis_segments:
        for name in [item["name"] for item in engine_segments]:
            bucket(name)
    page_primary: dict[str, str | None] = {}
    for page in pages:
        page_record = record(page)
        assignment = assign_segments([page_record], engine_segments)
        primary = assignment["primary"].get(page_record["url"])
        page_primary[page_record["url"]] = primary
        bucket(primary)["pages"] += 1

    for issue in issues:
        target = issue.get("target_url")
        if target:
            if target in page_primary:
                bucket(page_primary[target])["issues"] += 1
            else:
                issue_primary = assign_segments([record(target)], engine_segments)["primary"]
                bucket(issue_primary.get(target))["issues"] += 1
        else:
            # An audit-wide finding with no single target still counts
            # somewhere, so segment sums keep matching issues_total (#441).
            bucket(None)["issues"] += 1
    return counts


def _project_start_url(document: dict[str, Any]) -> str:
    """The URL a project crawl starts from: a competitor workspace keeps the URL as entered."""
    from seohead.projects.workspace import _target

    target = document["site"]["target"]
    for fact in document.get("facts", []):
        value = fact.get("value")
        if (
            fact.get("name") == "entered_url"
            and isinstance(value, str)
            and _target(value) == target
        ):
            return value
    return target


def _evidence_root(project_root: Path, start_url: str | None) -> Path | None:
    """The checklist a crawl may update: only the project whose own site it started from.

    A competitor's crawl is recorded in the competitor's workspace (run with that directory
    as the project); a crawl of a competitor URL run inside the owner's project updates none
    of the owner's items.
    """
    from seohead.projects.workspace import _load, _target

    if start_url is None:
        return project_root
    _, primary = _load(project_root)
    return project_root if _target(start_url) == primary["site"]["target"] else None


def _record_scoped_evidence(project_root: Path, start_url: str | None, scan: str | Path):
    """Record a crawl only into the checklist of the site it started from; never fails the crawl."""
    try:
        root = _evidence_root(project_root, start_url)
    except (OSError, ValueError) as exc:
        return {"error": str(exc)}
    if root is None:
        return {"skipped": "start URL is neither this project's site nor a recorded competitor"}
    return _record_crawl_evidence(root, scan)


def _record_scoped_failure(project_root: Path, start_url: str | None, reason: str) -> None:
    with contextlib.suppress(OSError, ValueError):
        root = _evidence_root(project_root, start_url)
        if root is not None:
            _record_crawl_failure(root, reason)


def _record_crawl_evidence(project_root: Path, scan: str | Path) -> dict[str, Any]:
    """Attach a finished project crawl to its automatic checklist items; never fails the crawl."""
    from seohead.projects.crawl_evidence import record_scan

    try:
        return record_scan(project_root, Path(scan))
    except (OSError, ValueError) as exc:
        return {"error": str(exc)}


def _record_crawl_failure(project_root: Path, reason: str) -> None:
    from seohead.projects.crawl_evidence import record_failure

    with contextlib.suppress(OSError, ValueError):
        record_failure(project_root, reason)


def crawl_control(project: str, run_id: str, action: str) -> dict[str, Any]:
    """Request a checkpointed pause or stop for an active native project crawl."""
    from seohead.projects.run_control import request

    return request(project, run_id, action)


def crawl_site(
    url: str | None = None,
    urls: list[str] | None = None,
    urls_file: str | None = None,
    config: str | None = None,
    profile: str | None = None,
    max_urls: int | None = None,
    max_depth: int | None = None,
    min_delay: float | None = None,
    concurrency: int | None = None,
    robots: str | None = None,
    out_dir: str | None = None,
    sitemap: str | None = None,
    sitemap_only: bool | None = None,
    scan_out: str | None = None,
    producer_build: str | None = None,
    overrides: dict[str, Any] | None = None,
    resume: str | None = None,
    progress: Callable[[int, int], None] | None = None,
    project: str | None = None,
    approve_large_crawl: bool = False,
    user_agent: str | None = None,
    observer_run_id: str | None = None,
) -> dict[str, Any]:
    """Crawl a site from a start URL, or fetch an explicit list, then audit it.

    The interface layer is where collector and analyzer are allowed to meet:
    ``seohead.crawl`` gathers evidence and never imports the analyzer,
    ``seohead.sf`` judges it and never imports the collector, and this function
    hands the projection from one to the other.

    ``min_delay`` defaults to half a second because the target is somebody's
    production site: polite by accident beats fast by accident.

    ``sitemap`` (or ``sitemaps.auto_discover`` in ``config``) seeds the crawl
    from a sitemap's declared URLs, in addition to following links from
    ``url``, and reconciles the two sources — see
    ``seohead.crawl.reconcile.reconcile_sitemap``. The same URL also feeds
    ``seohead.sf.core.sitemap_coverage.run_sitemap``, which independently
    re-fetches it to check the sitemap protocol's own limits and whether
    robots.txt declares it; with none given, those checks skip by name
    rather than guess at a default sitemap location.

    ``resume`` names an interrupted SQLite scan artifact to continue instead of
    describing a new crawl. It is the whole input: the start URL and every
    crawler setting are read back from the artifact, because they are what the
    stored frontier was built under. Passing any other crawl-shaping argument
    alongside it is therefore an error rather than an override -- see
    ``seohead.mcp.scan_handlers.resume_scan`` for the refusals decided
    before the first request.
    ``progress`` is a live-progress callback taking ``(fetched, queued)``. It
    is an interface-layer concern rather than a crawler setting -- only a
    caller with a terminal has anywhere to put a progress line -- so it is a
    plain callable here and has no place in ``config`` or the run manifest.
    List mode is not covered: its workload is the list, known in full before
    the first request, and the honest report of a known total is a different
    line than this one (see ``seohead.crawl.progress``).
    """
    project_root = None
    project_settings_policy = None
    if profile is None and config is not None:
        from seohead.crawl.profiles import name_for_path

        profile = name_for_path(config)
        if profile is not None:
            config = None
    if sitemap_only and not sitemap:
        raise ValueError("sitemap_only requires an explicit sitemap")
    if project is not None:
        from seohead.projects.runtime import project_policy
        from seohead.projects.workspace import open_project

        opened = open_project(project)
        project_root = Path(opened["path"])
        project_settings_policy = project_policy(str(project_root))["policy"]
        if resume is None and config is None and profile is None:
            profile = project_settings_policy.get("crawl_profile")
        if (
            resume is None
            and url is None
            and urls is None
            and urls_file is None
            and not sitemap_only
        ):
            url = _project_start_url(opened["project"])
    if sitemap_only and url is None:
        url = sitemap
    if resume is not None:
        # ``is not None`` rather than truthiness: --min-delay 0 and --max-urls 0 are
        # settings the caller stated, and silently accepting them here would let a
        # resume run under a value it then refuses to apply.
        conflicting = [
            name
            for name, value in (
                ("urls", urls),
                ("urls_file", urls_file),
                ("config", config),
                ("profile", profile),
                ("max_urls", max_urls),
                ("max_depth", max_depth),
                ("min_delay", min_delay),
                ("concurrency", concurrency),
                ("robots", robots),
                ("out_dir", out_dir),
                ("sitemap", sitemap),
                ("sitemap_only", sitemap_only),
                ("overrides", overrides),
                ("user_agent", user_agent),
            )
            if value is not None and value != "" and value not in ([], {})
        ]
        if scan_out is not None and scan_out != resume:
            conflicting.append("scan_out")
        if conflicting:
            raise ValueError(
                "resume continues the crawl its artifact already describes and reads every "
                "setting from it; it cannot be combined with " + ", ".join(sorted(conflicting))
            )
        from seohead.mcp.scan_handlers import resume_scan

        resume_data = None
        if project_root is not None:
            from seohead.mcp.scan_handlers import resume_inputs
            from seohead.projects.runtime import admission

            resume_data = resume_inputs(resume)
            gate = admission(
                str(project_root), resume_data["settings"], approved=approve_large_crawl
            )
            if not gate["ok"]:
                return {"ok": False, "error": gate["reason"], "admission": gate}
            from seohead.projects.run_control import ControlledRunReporter
            from seohead.projects.run_observation import finish, start

            try:
                within_project = Path(resume).resolve().is_relative_to(project_root.resolve())
            except OSError:
                within_project = False
            if within_project:
                from math import isfinite

                from seohead.crawl.settings import effective_request_rate, rate_fields
                from seohead.projects.origin_pacing import ProjectOriginPacer

                rate = effective_request_rate(resume_data["settings"])
                pacer = ProjectOriginPacer(
                    project_root,
                    resume_data["start_url"],
                    minimum_delay_seconds=resume_data["settings"]["speed"]["min_delay_seconds"],
                    max_requests_per_second=max(2.0, rate) if isfinite(rate) else 0.0,
                )
                observed = start(
                    project_root,
                    kind="native",
                    mode="spider",
                    max_urls=resume_data["settings"]["limits"]["max_urls"],
                    max_requests=resume_data["settings"]["limits"].get("max_requests", 0),
                    max_crawl_seconds=resume_data["settings"]["limits"].get("max_crawl_seconds", 0),
                    max_requests_per_second=float(rate) if isfinite(rate) else None,
                    config_fingerprint=str(resume_data.get("config_fingerprint") or "unknown"),
                    artifact=resume,
                    resumed=True,
                    origin=pacer.origin,
                    aggregate_max_requests_per_second=pacer.max_requests_per_second,
                    run_id=observer_run_id,
                )
                reporter = ControlledRunReporter(project_root, observed["id"], progress)
                try:
                    result = resume_scan(
                        resume,
                        url=url,
                        producer_build=producer_build,
                        progress=reporter,
                        observation=reporter.enter,
                        progress_snapshot=reporter.observe_counts,
                        shared_request_gate=pacer.wait_turn,
                    )
                except BaseException as exc:
                    with contextlib.suppress(OSError, ValueError):
                        finish(
                            project_root,
                            observed["id"],
                            state="failed",
                            reason=type(exc).__name__,
                            counters=reporter.counters(),
                        )
                    _record_crawl_failure(project_root, type(exc).__name__)
                    raise
                with contextlib.suppress(OSError, ValueError):
                    finish(
                        project_root,
                        observed["id"],
                        state=(
                            "partial"
                            if result.get("partial") or result.get("audit_available") is False
                            else "finished"
                        ),
                        reason=(
                            f"operator_{reporter.control_action}"
                            if reporter.control_action
                            else str(
                                result.get("audit_reason") or "audit_unavailable"
                                if result.get("audit_available") is False
                                else result.get("finish_reason") or "finished"
                            )
                        ),
                        counters=reporter.counters(),
                    )
                return {
                    **result,
                    **rate_fields(resume_data["settings"]),
                    "observer_run_id": observed["id"],
                    **(
                        {
                            "control": {
                                "action": reporter.control_action,
                                "state": "paused"
                                if reporter.control_action == "pause"
                                else "stopped",
                            }
                        }
                        if reporter.control_action
                        else {}
                    ),
                    "checklist_evidence": _record_crawl_evidence(project_root, resume),
                }
        from seohead.crawl.settings import rate_fields
        from seohead.mcp.scan_handlers import resume_inputs

        # Read the settings before resuming: the scan is finished once resume_scan returns.
        rate = rate_fields(resume_inputs(resume)["settings"])
        resumed = resume_scan(resume, url=url, producer_build=producer_build, progress=progress)
        return {**resumed, **rate}

    import os

    from seohead.crawl import cache as http_cache
    from seohead.crawl import settings as crawl_config
    from seohead.crawl.collect import collect_urls
    from seohead.crawl.spider import crawl_site as _spider

    if urls_file:
        if url or urls:
            raise ValueError("urls_file cannot be combined with url or urls")
        from seohead.crawl.list_input import iter_url_list

        urls = iter_url_list(urls_file)
    if not url and not urls:
        raise ValueError("url, urls, or urls_file required")

    if profile is not None:
        if config is not None:
            raise ValueError("profile and config are mutually exclusive")
        from seohead.crawl.profiles import path_for

        config = path_for(profile)

    # Defaults, then file, then environment, then these explicit arguments.
    # ``overrides`` carries whatever the caller reached for by dotted path -- the
    # CLI's --set and --max-urls-per-second. Only a named argument that was
    # actually given wins over it: updating with None would erase a caller's
    # override and silently fall back to the default, which is how a rate cap
    # becomes a no-op.
    resolved_overrides: dict[str, Any] = dict(overrides or {})
    for path, value in (
        ("limits.max_urls", max_urls),
        ("limits.max_depth", max_depth),
        ("speed.min_delay_seconds", min_delay),
        ("speed.concurrency", concurrency),
        ("robots.policy", robots),
        ("http.user_agent", user_agent),
        ("output.dir", out_dir),
    ):
        if value is not None:
            resolved_overrides[path] = value
    base_overrides = None
    if project_settings_policy is not None:
        base_overrides = project_settings_policy["crawl_overrides"]
        if profile is not None:
            # A named profile is the project's base configuration; project policy
            # overrides and explicit call arguments layer over it.
            resolved_overrides = {**base_overrides, **resolved_overrides}
            base_overrides = None
    if project_root is not None:
        from seohead.projects.runtime import admission
    if user_agent is not None and user_agent != "":
        # A preset name or one custom string sets the request identity and the robots token
        # together, so the rules matched are the rules of the crawler the site sees.
        from seohead.crawl.user_agents import resolve as resolve_user_agent

        identity, token = resolve_user_agent(user_agent)
        resolved_overrides["http.user_agent"] = identity
        resolved_overrides.setdefault("robots.user_agent_token", token)
    settings = crawl_config.load(
        config, overrides=resolved_overrides, base_overrides=base_overrides
    )
    if max_urls is not None or "limits.max_urls" in resolved_overrides:
        crawl_config.bind_budgets_to_page_budget(settings, set(resolved_overrides))
    # A storage-only synthetic capacity marker never enters a live crawl route.
    from seohead.crawl.settings import checked_url_budget

    if settings.get("storage", {}).get("capacity_profile", "stable") != "stable":
        raise ValueError(
            "experimental_synthetic capacity profile is storage-only, not a live crawl"
        )
    if (
        project_root is not None
        and not scan_out
        and not out_dir
        and not settings["output"]["dir"]
        and (not url or urls)
    ):
        import uuid

        from seohead.storage.history import new_scan_path

        # A URL list has no start URL; an empty host makes the name fall back to "scan".
        scan_out = str(new_scan_path(project_root / "scans", "", str(uuid.uuid4())))
    from seohead.crawl.settings import MAX_MATERIALIZED_URLS

    native_list = not url and (
        bool(scan_out)
        or (
            bool(settings["output"]["dir"])
            and (
                settings["limits"]["max_urls"] == 0
                or settings["limits"]["max_urls"] > MAX_MATERIALIZED_URLS
            )
        )
    )
    checked_url_budget(
        settings["limits"]["max_urls"], materialized=not bool(url) and not native_list
    )
    # Check selectors refer to the SF finding registry. Validate them at this
    # shared CLI/MCP boundary before the crawl can issue its first request.
    from seohead.sf.config import load_config as load_audit_config
    from seohead.sf.config import validate_config as validate_audit_config

    audit_config = load_audit_config(None)
    audit_config["finding_exclusions"] = settings["analysis"]["finding_exclusions"]
    audit_config["canonical_policy"] = settings["analysis"]["canonical_policy"]
    audit_config["thresholds"].update(settings["analysis"].get("asset_thresholds", {}))
    validate_audit_config(audit_config)
    proxy_route = crawl_config.resolve_proxy(settings)
    if project_root is not None:
        gate = admission(str(project_root), settings, approved=approve_large_crawl)
        if not gate["ok"]:
            return {"ok": False, "error": gate["reason"], "admission": gate}
    if (
        project_root is not None
        and not scan_out
        and url
        and not urls
        and not settings["output"]["dir"]
    ):
        import uuid

        from seohead.storage.history import new_scan_path

        scan_out = str(new_scan_path(project_root / "scans", url, str(uuid.uuid4())))
    analysis_segments = settings["analysis"]["segments"]
    if analysis_segments:
        from seohead.sf.core.segments import SegmentError, resolve_order

        try:
            resolve_order(analysis_segments)
        except SegmentError as exc:
            raise crawl_config.ConfigError(f"analysis.segments: {exc}") from exc
    _warn_ignored_robots(settings, url, urls)
    legacy_output = bool(out_dir or settings["output"]["dir"])
    if not scan_out and not legacy_output:
        if not url or urls:
            raise ValueError(
                "list mode has no default SQLite artifact; pass --out-dir for the legacy directory route"
            )
        if settings["cache"]["mode"] != "off":
            raise ValueError(
                f"cache.mode={settings['cache']['mode']!r} is unavailable for the default native "
                "SQLite capture; pass --out-dir for the legacy directory route"
            )
        scan_out = _default_scan_path(url, producer_build)
    if scan_out and settings["cache"]["mode"] != "off":
        raise ValueError(
            f"cache.mode={settings['cache']['mode']!r} is unavailable for native SQLite capture; "
            "pass --out-dir for the legacy directory route"
        )
    if scan_out and settings["discovery"]["external"]["crawl"]:
        # External-destination checking is wired into the legacy directory
        # route first (#746) — the same staged direction cache live/replay
        # took. Refusing by name is better than silently dropping an option
        # the operator asked for.
        raise ValueError(
            "discovery.external.crawl is unavailable for native SQLite capture; "
            "pass --out-dir for the legacy directory route"
        )
    if not url and settings["discovery"]["external"]["crawl"]:
        # The phase checks the edges a site crawl recorded; list mode keeps no
        # link edges, so the option can never take effect there — refuse by
        # name rather than silently dropping it (#746).
        raise ValueError(
            "discovery.external.crawl requires a site crawl (--url): "
            "list mode keeps no external edges to check"
        )
    if native_list:
        if scan_out and legacy_output:
            raise ValueError("scan_out and a legacy output directory cannot be combined")
        from seohead.mcp.scan_handlers import crawl_list_scan

        directory = settings["output"]["dir"] or None
        list_scan_out = scan_out or str(Path(directory) / ".list.seohead")
        observed = None
        if project_root is not None:
            from seohead.projects.run_observation import finish, start

            try:
                within_project = (
                    Path(list_scan_out).resolve().is_relative_to(project_root.resolve())
                )
            except OSError:
                within_project = False
            if within_project:
                from math import isfinite

                rate = crawl_config.effective_request_rate(settings)
                observed = start(
                    project_root,
                    kind="native",
                    mode="list",
                    max_urls=settings["limits"]["max_urls"],
                    max_requests=settings["limits"]["max_requests"],
                    max_crawl_seconds=settings["limits"]["max_crawl_seconds"],
                    max_requests_per_second=float(rate) if isfinite(rate) else None,
                    config_fingerprint=crawl_config.fingerprint(settings),
                    artifact=list_scan_out,
                    run_id=observer_run_id,
                )
        try:
            result = crawl_list_scan(
                urls,
                scan_out=list_scan_out,
                settings=settings,
                producer_build=producer_build,
                out_dir=directory,
                proxy_route=proxy_route,
            )
        except BaseException as exc:
            if observed is not None:
                with contextlib.suppress(OSError, ValueError):
                    finish(project_root, observed["id"], state="failed", reason=type(exc).__name__)
            raise
        if observed is None:
            return {**result, **crawl_config.rate_fields(settings)}
        unavailable = result.get("audit_available") is False
        with contextlib.suppress(OSError, ValueError):
            finish(
                project_root,
                observed["id"],
                state="partial" if result.get("partial") or unavailable else "finished",
                reason=(
                    str(result.get("audit_reason") or "audit_unavailable")
                    if unavailable
                    else str(result.get("finish_reason") or "finished")
                ),
            )
        return {**result, **crawl_config.rate_fields(settings), "observer_run_id": observed["id"]}
    if settings.get("resources", {}).get("fetch") and not scan_out:
        raise ValueError("resources.fetch requires a SQLite scan artifact")
    if scan_out:
        if not url or urls:
            raise ValueError(
                "SQLite scan mode requires a start URL; pass --out-dir for the legacy list-mode route"
            )
        if out_dir or settings["output"]["dir"]:
            raise ValueError("scan_out and a legacy output directory cannot be combined")
        from seohead.mcp.scan_handlers import crawl_site_scan

        observed = None
        reporter = None
        pacer = None
        if project_root is not None:
            from seohead.projects.origin_pacing import ProjectOriginPacer
            from seohead.projects.run_observation import start

            try:
                within_project = Path(scan_out).resolve().is_relative_to(project_root.resolve())
            except OSError:
                within_project = False
            if within_project:
                from math import isfinite

                rate = crawl_config.effective_request_rate(settings)
                pacer = ProjectOriginPacer(
                    project_root,
                    url,
                    minimum_delay_seconds=settings["speed"]["min_delay_seconds"],
                    max_requests_per_second=max(2.0, rate) if isfinite(rate) else 0.0,
                )
                observed = start(
                    project_root,
                    kind="native",
                    mode="spider",
                    max_urls=settings["limits"]["max_urls"],
                    max_requests=settings["limits"]["max_requests"],
                    max_crawl_seconds=settings["limits"]["max_crawl_seconds"],
                    max_requests_per_second=float(rate) if isfinite(rate) else None,
                    config_fingerprint=crawl_config.fingerprint(settings),
                    artifact=scan_out,
                    origin=pacer.origin,
                    aggregate_max_requests_per_second=pacer.max_requests_per_second,
                    run_id=observer_run_id,
                )
                from seohead.projects.run_control import ControlledRunReporter

                reporter = ControlledRunReporter(project_root, observed["id"], progress)
        if pacer is None:
            # Outside a project the schedule lives in the user state directory, so
            # concurrent crawls of one host still share one aggregate ceiling.
            from math import isfinite

            from seohead.projects.origin_pacing import ProjectOriginPacer

            rate = crawl_config.effective_request_rate(settings)
            pacer = ProjectOriginPacer(
                None,
                url,
                minimum_delay_seconds=settings["speed"]["min_delay_seconds"],
                max_requests_per_second=max(2.0, rate) if isfinite(rate) else 0.0,
            )
        try:
            result = crawl_site_scan(
                url,
                scan_out=scan_out,
                settings=settings,
                sitemap=sitemap,
                sitemap_only=bool(sitemap_only),
                producer_build=producer_build,
                progress=reporter or progress,
                observation=reporter.enter if reporter is not None else None,
                progress_snapshot=reporter.observe_counts if reporter is not None else None,
                shared_request_gate=pacer.wait_turn,
                proxy_route=proxy_route,
            )
        except BaseException as exc:
            if observed is not None and reporter is not None:
                from seohead.projects.run_observation import finish

                with contextlib.suppress(OSError, ValueError):
                    finish(
                        project_root,
                        observed["id"],
                        state="failed",
                        reason=type(exc).__name__,
                        counters=reporter.counters(),
                    )
                _record_scoped_failure(project_root, url, type(exc).__name__)
            raise
        if observed is not None and reporter is not None:
            from seohead.projects.run_observation import finish

            with contextlib.suppress(OSError, ValueError):
                finish(
                    project_root,
                    observed["id"],
                    state=(
                        "partial"
                        if result.get("partial") or result.get("audit_available") is False
                        else "finished"
                    ),
                    reason=(
                        f"operator_{reporter.control_action}"
                        if reporter.control_action
                        else str(
                            result.get("audit_reason") or "audit_unavailable"
                            if result.get("audit_available") is False
                            else result.get("finish_reason") or "finished"
                        )
                    ),
                    counters=reporter.counters(),
                )
            return {
                **result,
                **crawl_config.rate_fields(settings),
                "observer_run_id": observed["id"],
                **(
                    {
                        "control": {
                            "action": reporter.control_action,
                            "state": "paused" if reporter.control_action == "pause" else "stopped",
                        }
                    }
                    if reporter.control_action
                    else {}
                ),
                "checklist_evidence": _record_scoped_evidence(project_root, url, scan_out),
            }
        return {**result, **crawl_config.rate_fields(settings)}
    dispatch_gate = None
    if url:
        from seohead.crawl.throttle import DispatchGate, Throttle

        throttle = Throttle(
            min_delay=settings["speed"]["min_delay_seconds"],
            max_delay=settings["speed"]["max_delay_seconds"],
            max_concurrency=settings["speed"]["concurrency"],
            adaptive=settings["speed"]["adaptive"],
        )
        dispatch_gate = DispatchGate(
            throttle, time.sleep, max_requests=settings["limits"]["max_requests"]
        )
    out_dir = settings["output"]["dir"] or None
    if (
        proxy_route is not None
        and out_dir
        and os.path.exists(os.path.join(out_dir, "crawl_state.json"))
    ):
        raise ValueError(
            "proxied legacy crawls cannot resume from a saved frontier; start with a new output directory"
        )
    if out_dir:
        os.makedirs(out_dir, exist_ok=True)
    # The human-readable export: absent whenever the operator turned it off. Only
    # Both legacy routes retain a private sidecar when the public export is off,
    # so an unavailable audit still leaves the collected evidence.
    pages_export_path = (
        os.path.join(out_dir, "pages.jsonl")
        if out_dir and settings["output"]["write_pages_jsonl"]
        else None
    )
    # Tied to out_dir alone, not the write_pages_jsonl toggle -- same shape as
    # links_path below. spider.crawl_site's resume mechanism reloads previously
    # fetched pages from whatever path it was given as out_path; passing None
    # here whenever the export was off used to defeat that reload silently, so a
    # resumed run reported fewer pages than the interrupted one had already
    # found (issue #242). When the export is on the two paths are the same file;
    # when it is off, a private sidecar the operator never asked to see still
    # carries what a resume needs, and is removed once there is nothing left to
    # resume (see the "finished" cleanup after the spider call below).
    pages_resume_path = pages_export_path or (
        os.path.join(out_dir, ".pages_resume.jsonl") if out_dir else None
    )
    # Tied to out_dir alone, not the write_pages_jsonl toggle: this sidecar is what makes a
    # resumed run's result.links whole again (see spider.crawl_site), a correctness need
    # distinct from whether the operator also wants pages.jsonl as a human-readable export.
    links_path = os.path.join(out_dir, "links.jsonl") if out_dir else None
    # Same "tied to out_dir, gated by its own toggle" shape as pages_export_path: a
    # decision log is a diagnostic artifact, not something a resumed run
    # depends on (issue #134).
    decisions_path = (
        os.path.join(out_dir, "decisions.jsonl")
        if out_dir and settings["output"]["write_decisions_jsonl"]
        else None
    )
    # Tied to out_dir like links_path above: the external check's append-only
    # record of per-destination outcomes — and what a resumed run reads back
    # as its "already decided" set (#746).
    external_checks_path = os.path.join(out_dir, "external_checks.jsonl") if out_dir else None
    max_seconds = settings["limits"]["max_crawl_seconds"]
    # One cache per run, shared by every worker thread a concurrent crawl starts — see
    # seohead.crawl.cache for the freshness policy and seohead.crawl.settings for cache.mode /
    # cache.invalidate. A directory this session cannot trust (missing, world-writable) degrades
    # to no cache rather than failing the run.
    cache = http_cache.build(
        http_cache.resolve_dir(),
        mode=settings["cache"]["mode"],
        invalidate=settings["cache"]["invalidate"],
    )

    sitemap_seed = {"sitemap_url": None, "sitemap_urls": [], "declared": []}
    if url and (sitemap or settings["sitemaps"]["auto_discover"]):
        sitemap_seed = _seed_urls_from_sitemap(
            url,
            sitemap,
            settings["sitemaps"]["auto_discover"],
            request_gate=dispatch_gate.wait_turn,
            robots_token=settings["robots"]["user_agent_token"],
            throttle=throttle,
            proxy_route=proxy_route,
        )

    if url:
        result = _spider(
            url,
            max_urls=settings["limits"]["max_urls"],
            max_requests=settings["limits"]["max_requests"],
            max_depth=settings["limits"]["max_depth"],
            max_seconds=max_seconds,
            min_delay=settings["speed"]["min_delay_seconds"],
            timeout=settings["http"]["timeout_seconds"],
            robots_policy=settings["robots"]["policy"],
            scope=settings["scope"],
            seed_urls=sitemap_seed["declared"] or None,
            out_path=pages_resume_path,
            links_path=links_path,
            forms_path=os.path.join(out_dir, ".forms_resume.jsonl") if out_dir else None,
            decisions_path=decisions_path,
            credential_headers=settings["http"]["credential_headers"],
            http_auth=settings["http"]["auth"],
            # Checkpointed only when there is somewhere durable to put it; a
            # crawl with no out_dir has nothing to resume into anyway.
            state_path=os.path.join(out_dir, "crawl_state.json") if out_dir else None,
            config_fingerprint=crawl_config.fingerprint(settings),
            concurrency=settings["speed"]["concurrency"],
            max_response_bytes=settings["limits"]["max_response_bytes"],
            max_url_length=settings["limits"]["max_url_length"],
            max_query_variants_per_path=settings["limits"]["max_query_variants_per_path"],
            retry_on_timeout=settings["http"]["retry_on_timeout"],
            user_agent=settings["http"]["user_agent"],
            robots_token=settings["robots"]["user_agent_token"],
            unavailable_means_stop=settings["robots"]["unavailable_means_stop"],
            stop_after_consecutive_timeouts=settings["speed"]["stop_after_consecutive_timeouts"],
            max_delay_seconds=settings["speed"]["max_delay_seconds"],
            follow_nofollow=settings["discovery"]["follow_nofollow"],
            classify_links=settings["link_position"]["classify"],
            link_position_rules=settings["link_position"]["rules"] or None,
            cache=cache,
            extra_request_headers=crawl_config.request_headers(settings),
            adaptive=settings["speed"]["adaptive"],
            store_hyperlinks=settings["discovery"]["hyperlinks"]["store"],
            crawl_hyperlinks=settings["discovery"]["hyperlinks"]["crawl"],
            store_external_links=settings["discovery"]["external"]["store"],
            crawl_redirects=settings["discovery"]["redirects"]["crawl"],
            capture_link_attributes=settings["link_attributes"]["capture"],
            crawl_external_links=settings["discovery"]["external"]["crawl"],
            external_policy=settings["external_checks"],
            external_path=(
                external_checks_path if settings["discovery"]["external"]["crawl"] else None
            ),
            dispatch_gate=dispatch_gate,
            progress=progress,
            spool_evidence=True,
            proxy_route=proxy_route,
            normalize_tracking_parameters=settings["scope"]["normalize_tracking_parameters"],
        )
        discovery = {
            "mode": "spider",
            # #332: named here too, matching list mode -- a robots-blocked count
            # without the policy that produced it is not self-explanatory.
            "directive_policy": settings["robots"]["policy"],
            "max_depth_reached": result.max_depth_reached,
            "links_seen": result.link_count if result.spooled_evidence else len(result.links),
            "excluded": result.excluded,
            "robots_note": result.robots_note,
            "robots_blocked": len(result.robots_blocked),
            "crawl_delay_applied": result.crawl_delay_applied,
            "effective_delay_seconds": round(result.effective_delay, 3),
            "effective_concurrency": result.effective_concurrency,
            "resume_note": result.resume_note,
            "sitemap_url": sitemap_seed["sitemap_url"],
            "sitemap_urls": sitemap_seed["sitemap_urls"],
            "sitemap_seeded": len(result.seed_urls),
        }
        if sitemap_seed.get("skipped_sitemaps"):
            # Relative or otherwise unusable robots-declared sitemaps were skipped,
            # not fatal (#1552): say so in the crawl's own coverage statement.
            discovery["skipped_sitemaps"] = sitemap_seed["skipped_sitemaps"]
        if result.external_summary:
            # The external phase's own coverage statement: policy used,
            # per-outcome counts and why it stopped — kept distinct from the
            # internal frontier's finish_reason above (#746).
            discovery["external_checks"] = result.external_summary
        if settings["scope"]["segments_only"]:
            # #358's acceptance criterion: a crawl scoped to a segment must say so
            # in its own run output, not leave it to be inferred from which URLs
            # happen to be missing.
            discovery["segments_only"] = settings["scope"]["segments_only"]
    else:
        from seohead.mcp.scan_handlers import _list_collection_options

        result = collect_urls(
            urls or [],
            **_list_collection_options(settings),
            out_path=pages_resume_path,
            cache=cache,
            proxy_route=proxy_route,
        )
        discovery = {
            "mode": "list",
            # #21: the configured policy must be stated, not merely applied — a report that
            # says nothing here is indistinguishable from one that silently ignored it.
            "directive_policy": settings["robots"]["policy"],
            "robots_blocked": len(result.robots_blocked),
            "canonical_destination_resolution": settings["discovery"][
                "resolve_canonical_destination"
            ],
        }

    if not url or result.spooled_evidence:
        page_count = result.page_count if url else len(result.pages)
        link_count = result.link_count if url else 0
        form_count = result.form_count if url else 0
        from seohead.crawl.spider import (
            _read_forms_jsonl,
            _read_links_jsonl,
            _read_pages_jsonl,
        )
        from seohead.mcp.scan_handlers import MAX_AUDIT_FORMS, MAX_AUDIT_PAGES

        # The existing materialized audit is only admitted within explicit
        # bounds. #816 owns its versioned streaming replacement.
        max_legacy_audit_links = 1_500_000
        if result.finish_reason == "robots_unavailable":
            reason = (
                "legacy audit unavailable: robots.txt could not be read; "
                "any prior JSONL sidecars were not reconciled"
            )
        elif (
            page_count > MAX_AUDIT_PAGES
            or form_count > MAX_AUDIT_FORMS
            or link_count > max_legacy_audit_links
        ):
            reason = (
                "legacy audit materialization limit exceeded "
                f"(pages={page_count}/{MAX_AUDIT_PAGES}, "
                f"forms={form_count}/{MAX_AUDIT_FORMS}, "
                f"links={link_count}/{max_legacy_audit_links}); "
                "JSONL collection evidence is retained"
            )
        else:
            reason = ""
        if reason:
            stale_reports = _retire_crawl_reports(out_dir)
            return {
                "urls_collected": page_count,
                "links_collected": link_count,
                "forms_collected": form_count,
                "audit_available": False,
                "audit_reason": reason,
                "partial": result.partial,
                "stopped_reason": result.stopped_reason,
                "finish_reason": result.finish_reason,
                "resumed": result.resumed,
                "discovery": discovery,
                "limitations": result.limitations,
                "out_dir": out_dir,
                "cache_replay": result.cache_replay,
                "cache_stats": result.cache_stats,
                "stale_reports": stale_reports,
            }
        if url:
            result.pages = _read_pages_jsonl(pages_resume_path)
            result.links = _read_links_jsonl(links_path)
            result.forms = _read_forms_jsonl(os.path.join(out_dir, ".forms_resume.jsonl"))
            if (
                len(result.pages),
                len(result.links),
                len(result.forms),
            ) != (page_count, link_count, form_count):
                raise ValueError("legacy evidence sidecar counts changed during audit preparation")

    response, _audit = _audit_crawl_result(
        result,
        settings=settings,
        url=url,
        sitemap_seed=sitemap_seed,
        discovery=discovery,
        out_dir=out_dir,
        pages_resume_path=pages_resume_path,
        dispatch_gate=dispatch_gate,
        proxy_route=proxy_route,
    )
    if (
        url
        and result.spooled_evidence
        and pages_resume_path
        and pages_resume_path != pages_export_path
        and result.finish_reason == "finished"
    ):
        with contextlib.suppress(FileNotFoundError):
            os.remove(pages_resume_path)
    if url and result.spooled_evidence and result.finish_reason == "finished":
        with contextlib.suppress(FileNotFoundError):
            os.remove(os.path.join(out_dir, ".forms_resume.jsonl"))
    return {**response, **crawl_config.rate_fields(settings)}


def _audit_crawl_result(
    result,
    *,
    settings,
    url,
    sitemap_seed,
    discovery,
    out_dir=None,
    pages_resume_path=None,
    stored_scan=None,
    stored_sitemap=None,
    offline: bool = False,
    captured_render_summary: dict[str, Any] | None = None,
    dispatch_gate=None,
    proxy_route=None,
    streaming: bool = False,
):
    """Run the existing native analysis over a complete, admitted population."""
    import json
    import os
    from datetime import datetime, timezone
    from urllib.parse import urlsplit

    from seohead.crawl import settings as crawl_config
    from seohead.crawl.evidence import build_evidence
    from seohead.crawl.reconcile import reconcile_sitemap
    from seohead.sf.config import load_config, validate_config

    audit_config = load_config(None)
    audit_config["finding_exclusions"] = settings.get("analysis", {}).get("finding_exclusions", [])
    audit_config["thresholds"].update(settings.get("analysis", {}).get("asset_thresholds", {}))
    audit_config["content_area"] = settings.get("evidence", {}).get("content_area", {})
    validate_config(audit_config)
    from seohead.sf.core.aggregate import aggregate
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.eeat import run_eeat
    from seohead.sf.core.heuristics import run_heuristics
    from seohead.sf.core.inlinks import run_inlinks
    from seohead.sf.core.loader import LoadedExports
    from seohead.sf.core.rules import run_rules
    from seohead.sf.core.sitemap_coverage import run_sitemap

    if type(offline) is not bool:
        raise ValueError("offline must be a boolean")
    if captured_render_summary is not None and not isinstance(captured_render_summary, dict):
        raise ValueError("captured_render_summary must be an object when supplied")

    requires_rendering = False
    requires_rendering_reason = ""
    render_summary: dict[str, Any] = {}
    if url:
        from seohead.crawl import render_escalation
        from seohead.recon.net import normalize_url

        start_norm = normalize_url(url)
        rendering_config = settings["rendering"]
        escalation = None
        if offline and rendering_config["mode"] != "raw":
            render_summary = (
                dict(captured_render_summary)
                if captured_render_summary is not None
                else {
                    "mode": rendering_config["mode"],
                    "state": "unavailable",
                    "reason": "offline reanalysis has no captured render summary",
                }
            )
        elif rendering_config["mode"] != "raw" and result.pages:
            if stored_scan is None:
                escalation = _run_render_escalation(
                    result,
                    rendering_config,
                    settings,
                    request_gate=dispatch_gate.wait_turn if dispatch_gate is not None else None,
                    proxy_route=proxy_route,
                )
                render_escalation.apply_rendered_evidence(result.pages, result.links, escalation)
                # The spider already streamed pages_resume_path during the crawl, before
                # this escalation existed to mutate result.pages -- so whichever pages
                # were re-fetched now have audit-and-memory evidence the file on disk
                # does not, and a resumed run or a pages.jsonl reader would see stale
                # static evidence next to an audit.json that says rendered (#244).
                if pages_resume_path and escalation.render_requests:
                    _rewrite_pages_sidecar(pages_resume_path, result.pages)
            else:
                from seohead.crawl.sqlite_render import run_render_escalation

                # The SQLite path commits each DOM and its observations before
                # admitting it to this transient audit view.  It deliberately
                # leaves HTML out of ``EscalationResult`` so a large scan never
                # accumulates every serialized DOM in memory.
                escalation = run_render_escalation(
                    stored_scan,
                    result,
                    settings,
                    request_gate=dispatch_gate.wait_turn if dispatch_gate is not None else None,
                    proxy_route=proxy_route,
                )
                if not offline:
                    from seohead.crawl.sqlite_resources import capture_resources

                    resource_kwargs = {}
                    if dispatch_gate is not None:
                        resource_kwargs = {
                            "throttle": dispatch_gate.throttle,
                            "dispatch_gate": dispatch_gate,
                        }
                    capture_resources(
                        stored_scan, settings, proxy_route=proxy_route, **resource_kwargs
                    )
                coverage = stored_scan.con.execute(
                    "SELECT crawl_partial,limitations_json FROM scan"
                ).fetchone()
                result.partial = bool(coverage[0])
                result.limitations = json.loads(coverage[1])
            render_summary = {
                "mode": escalation.mode,
                "patterns_sampled": escalation.patterns_sampled,
                "patterns_escalated": escalation.patterns_escalated,
                "probe_requests": escalation.probe_requests,
                "render_requests": escalation.render_requests,
                "render_budget_exhausted": escalation.render_budget_exhausted,
                # Set independently of render_budget_exhausted (#198): max_render_urls and
                # max_render_seconds are two different operator-set limits that run out for
                # unrelated reasons, and a report must say which one cut this run short.
                "time_budget_exhausted": escalation.time_budget_exhausted,
                # Which escalated patterns the budget actually reached, and
                # which it ran out on before finishing -- patterns_escalated
                # alone cannot tell the two apart (#147).
                "render_counts": escalation.render_counts,
                "patterns_partially_rendered": escalation.patterns_partially_rendered,
                "patterns_unprobed": escalation.patterns_unprobed,
                "patterns_unprobed_reasons": escalation.patterns_unprobed_reasons,
                # The resolved browser-page bound the escalation ran under --
                # read from settings rather than the aggregated result so a
                # resumed scan's merged summary still reports it correctly.
                "render_page_concurrency": int(
                    settings["rendering"]["browser"].get("page_concurrency") or 1
                ),
            }

        # Re-evaluated after escalation so a run that actually renders its
        # start page is judged on that rendered evidence, not the raw
        # snapshot escalation was meant to fix -- but the gate itself is
        # static-only (see start_page_gate) so it still fires for "raw" mode,
        # which has no render to fall back on.
        start_record = next((p for p in result.pages if p.url == start_norm), None)
        if start_record is not None:
            # Stored scans reconstruct this from the retained *static*
            # document before analysis.  Rendering is a later representation
            # and must not trigger a new request merely to satisfy the raw
            # start-page gate on resume.
            rendered_start = (
                escalation.rendered.get(start_norm) if escalation and stored_scan is None else None
            )
            start_html = (
                getattr(result, "_rendered_start_html", None)
                or (rendered_start or {}).get("html")
                or result.start_page_evidence.get("html", "")
            )
            gate = render_escalation.start_page_gate(
                start_norm,
                max(start_record.outlinks - start_record.external_outlinks, 0),
                start_html,
            )
            requires_rendering, requires_rendering_reason = gate.requires_rendering, gate.reason

    # Keep scalar SQL lookups alive only while the native row stream is copied
    # into the disk-backed analysis context. No full page frame or inlink map.
    from contextlib import ExitStack

    stored_graph_available = False
    from seohead.crawl.list_scan import is_list_scan

    stored_list = stored_scan is not None and is_list_scan(stored_scan.con)
    with ExitStack() as evidence_stack:
        if stored_scan is None:
            evidence = build_evidence(result)
        else:
            from seohead.crawl.sql_graph import StoredGraph

            stored_graph_available = (
                not stored_list
                and stored_scan.con.execute("SELECT 1 FROM links LIMIT 1").fetchone() is not None
            )
            graph = evidence_stack.enter_context(StoredGraph(stored_scan.con))

            def is_robots_blocked(page_url):
                return (
                    stored_scan.con.execute(
                        "SELECT 1 FROM urls u JOIN context_items c "
                        "ON c.kind='robots_blocked_url' AND c.item_key='url:' || u.url_id "
                        "WHERE u.url=? LIMIT 1",
                        (page_url,),
                    ).fetchone()
                    is not None
                )

            evidence = build_evidence(
                result,
                inlink_counts=graph.inlink_counts() if stored_graph_available else None,
                stored_graph_available=stored_graph_available,
                streaming=True,
                is_robots_blocked=is_robots_blocked,
            )
        exports = LoadedExports()
        exports.frames.update(evidence["frames"])
        exports.found = list(evidence["found"])
        exports.missing = list(evidence["missing"])

        audit_config["canonical_policy"] = settings["analysis"]["canonical_policy"]
        ctx = AuditContext(exports, audit_config, disk_backed_pages=stored_scan is not None)
        ctx.scan_con = stored_scan.con if stored_scan is not None else None
    saved_corpus = None
    if stored_scan is not None:
        from seohead.sf.core.corpus_derivations import derive

        saved_corpus = derive(stored_scan.con, streaming=streaming)
        if streaming:
            ctx._saved_corpus = saved_corpus
        ctx.native_hreflang = saved_corpus["internationalization"]
        from seohead.storage.target_probes import load as load_target_probes

        ctx.target_probes = load_target_probes(stored_scan.con)
    # Where this crawl actually began. A native crawl knows; nothing else does,
    # and pages.crawl_depth is not a substitute -- a sitemap-seeded crawl records
    # 0 for every seeded URL, so the click-depth walk would start from an
    # arbitrary page (#634). A URL-list run has no start URL and must say so
    # rather than invent one.
    ctx.start_url = start_norm if url else None
    available_exports = set(exports.frames)
    if ctx.native_hreflang is not None and ctx.native_hreflang["declarations"]:
        available_exports.add("all_hreflang")
    from seohead.sf.core.rules import native_check_ids

    ctx.skip_unsupported(available_exports, native=native_check_ids(ctx))
    run_rules(ctx)
    if stored_list:
        unavailable_origins = stored_scan.con.execute(
            "SELECT COUNT(*) FROM context_items WHERE kind='list_robots' AND completeness='unavailable'"
        ).fetchone()[0]
        if unavailable_origins:
            ctx.skip(
                "BLOCKED_BY_ROBOTS",
                f"robots policy was unavailable for {unavailable_origins} explicit-list origins",
            )
    # Same pipeline the Screaming Frog export path runs (seohead/sf/core/audit.py)
    # -- omitting it here left every inlinks-derived check (anchor text, hreflang,
    # link score, discovery path, inlink composition, insecure subresources)
    # neither fired nor skipped: silently uninvoked rather than honestly absent
    # (issue #128). ``all_inlinks`` is populated above from the crawl's own
    # hyperlink graph when one exists (see crawl/evidence.py), so the checks it
    # feeds now answer for real instead of only reaching their skip branch.
    if stored_graph_available:
        from seohead.sf.core.inlinks import _site_host
        from seohead.sf.core.normalize import norm_url
        from seohead.storage.analysis_graph import AnalysisGraph

        with AnalysisGraph(stored_scan.con, normalize=norm_url, site_host=_site_host(ctx)) as graph:
            ctx.graph_access = graph
            run_inlinks(ctx)
            run_eeat(ctx)
        ctx.graph_access = None
    else:
        run_inlinks(ctx)
        run_eeat(ctx)
    # Native DOM and content comparisons read each page's selected retained
    # document through ctx.scan_con. The legacy non-stored route has no bodies
    # and reports its real unavailable reason. Weight/title checks use the
    # scalar evidence already supplied by build_evidence.
    run_heuristics(ctx)

    # ``run_sitemap`` covers the sitemap-protocol and robots.txt checks that need a
    # live network fetch (SITEMAP_TOO_LARGE, SITEMAP_STALE_LASTMOD, SITEMAP_NOT_IN_ROBOTS,
    # ROBOTS_BLOCKS_RESOURCES, ...) and, before this, was never called from crawl_site
    # either (issue #165) -- those checks were silently uninvoked on every native crawl,
    # sitemap or not. It is given the same sitemap URL used to seed this crawl, if any;
    # with none, it reaches its own honest per-check skip branches instead of guessing
    # at a default sitemap location. Its own declared-vs-crawled comparison
    # (SITEMAP_DESYNC and the "in_sitemap_and_linked"-shaped summary keys) is cruder
    # than the dedicated reconciliation below, so those three summary keys are
    # overwritten by it further down rather than the other way around.
    if stored_scan is not None:
        from seohead.sf.core.native_sitemap import run_retained_sitemap

        measured = run_retained_sitemap(ctx, stored_sitemap, start_url=url or "")
    elif offline:
        reason = "offline reanalysis has no retained sitemap XML, robots.txt, or lastmod evidence"
        for check in (
            "SITEMAP_NOT_IN_ROBOTS",
            "ROBOTS_BLOCKS_RESOURCES",
            "SITEMAP_FETCH_INCOMPLETE",
            "SITEMAP_TOO_MANY_URLS",
            "SITEMAP_TOO_LARGE",
            "SITEMAP_URL_DUPLICATED",
            "SITEMAP_STALE_LASTMOD",
            "SITEMAP_URL_4XX_5XX",
            "SITEMAP_URL_3XX",
            "SITEMAP_URL_NON_INDEXABLE",
        ):
            ctx.skip(check, reason)
        measured: dict[str, Any] = {}
    else:
        sitemap_kwargs = {}
        if dispatch_gate is not None:
            sitemap_kwargs["request_gate"] = dispatch_gate.wait_turn
        if proxy_route is not None:
            sitemap_kwargs["proxy_route"] = proxy_route
        measured = run_sitemap(
            ctx,
            sitemap_url=sitemap_seed["sitemap_url"],
            sitemap_urls=sitemap_seed["sitemap_urls"],
            compare_with_crawl=stored_scan is None and not sitemap_seed["declared"],
            crawl_partial=bool(getattr(result, "partial", False)),
            **sitemap_kwargs,
        )
        if stored_scan is not None:
            from seohead.sf.core.sitemap_coverage import settle_native_sitemap_status

            settle_native_sitemap_status(ctx, "no sitemap was compared by this crawl")
    # Only surfaced when something was actually measured. run_sitemap always
    # returns its keys, and a run with no sitemap at all would otherwise report
    # urls_in_sitemap: 0 -- which reads as "the sitemap is empty" when the truth
    # is "there was no sitemap". The checks themselves still ran and skipped by
    # name above; it is the summary that must not invent a zero.
    sitemap_summary: dict[str, Any] = (
        dict(measured)
        if (measured.get("sitemaps") or measured.get("declared_in_robots") is not None)
        else {}
    )
    if stored_scan is not None:
        sitemap_summary = measured
    if stored_scan is None and sitemap_seed["declared"]:
        # "Reached by following links" — not merely fetched, since a seeded
        # URL is fetched regardless of whether anything links to it. Three
        # disjoint facts, reported under the check ids the Screaming Frog
        # pipeline already uses for the same distinction (SITEMAP_ORPHAN,
        # URL_NOT_IN_SITEMAP), so audit.json has one schema either way.
        if stored_sitemap is None:
            observed = [edge.destination for edge in result.links]
            reconciled = reconcile_sitemap(
                sitemap_seed["declared"], observed, _sitemap_comparable_pages(result, url)
            )
        else:
            reconciled = stored_sitemap.materialize(100_000)
            reconciled.pop("available", None)
            reconciled.pop("reason", None)
            reconciled.pop("crawl_partial", None)
        reconciled["sitemap_url"] = sitemap_seed["sitemap_url"]
        reconciled["sitemap_urls"] = sitemap_seed["sitemap_urls"]
        for orphan_url in reconciled["in_sitemap_not_linked"]:
            ctx.add("SITEMAP_ORPHAN", target_url=orphan_url, details={"in_sitemap": True})
        for extra_url in reconciled["linked_not_in_sitemap"]:
            ctx.add("URL_NOT_IN_SITEMAP", target_url=extra_url)
        # Legacy unstored crawls retain the sitemap seed and fetched page records;
        # saved scans use the dedicated retained sitemap taxonomy above.
        from urllib.parse import urlsplit as _sitemap_split

        from seohead.crawl.sitemap_findings import sitemap_url_problems

        sitemap_host = _sitemap_split(start_norm).hostname or "" if url else ""
        for item in sitemap_url_problems(
            sitemap_seed["declared"],
            getattr(result, "pages", []) or [],
            getattr(result, "robots_blocked", None) or [],
            sitemap_host,
        ):
            ctx.add(
                item["check"],
                target_url=item["target_url"],
                details={
                    "status_code": item["status_code"],
                    "reasons": item["reasons"],
                },
            )
        # The site-level verdict belongs to whichever module did the comparison,
        # and here that is this one -- run_sitemap was told to skip it by name
        # (compare_with_crawl above) precisely so the two never answer the same
        # question with two different degrees of rigour. The per-URL findings
        # above say which URLs disagree; this says whether the disagreement is
        # large enough to be a fact about the site rather than a handful of URLs.
        comparable_total = max(
            len(reconciled["in_sitemap_and_linked"]) + len(reconciled["linked_not_in_sitemap"]), 1
        )
        crawl_only_pct = round(100 * len(reconciled["linked_not_in_sitemap"]) / comparable_total, 1)
        sitemap_only_pct = round(
            100
            * len(reconciled["in_sitemap_not_linked"])
            / max(
                len(sitemap_seed["declared"])
                if stored_sitemap is None
                else stored_sitemap.declared_raw_count,
                1,
            ),
            1,
        )
        threshold = ctx.thresholds["sitemap_desync_pct_warn"]
        if getattr(result, "partial", False):
            # A URL-limited or otherwise incomplete native crawl can still count sitemap
            # URLs it never reached as "unlinked" -- but the unfetched frontier may hold
            # exactly the link that would prove them linked, so a thresholded site-wide
            # verdict from this graph is unsound (#362). The per-URL findings above
            # (SITEMAP_ORPHAN, URL_NOT_IN_SITEMAP) still fire on what was actually
            # observed; only the whole-graph percentage verdict is withheld.
            ctx.skip(
                "SITEMAP_DESYNC",
                "crawl is partial: a sitemap-versus-link-graph verdict cannot be proven "
                "when the crawl did not reach every URL",
            )
        elif crawl_only_pct >= threshold or sitemap_only_pct >= threshold:
            ctx.add(
                "SITEMAP_DESYNC",
                target_url=url,
                details={
                    "in_crawl_not_in_sitemap": len(reconciled["linked_not_in_sitemap"]),
                    "in_sitemap_not_in_crawl": len(reconciled["in_sitemap_not_linked"]),
                    "crawl_not_in_sitemap_pct": crawl_only_pct,
                    "sitemap_not_in_crawl_pct": sitemap_only_pct,
                    "examples_missing_from_sitemap": reconciled["linked_not_in_sitemap"][:20],
                    "examples_in_sitemap_not_crawled": reconciled["in_sitemap_not_linked"][:20],
                },
            )
        sitemap_summary.update(reconciled)

    # Same "native crawl produces evidence the SF export never carries" shape
    # as the sitemap reconciliation above: classification runs inside the
    # spider's own link recording (see crawl/spider.py), never through the
    # analyzer, and only meets the SF-shaped audit here.
    link_position: dict[str, Any] = {}
    if url and settings["link_position"]["classify"]:
        from urllib.parse import urlsplit

        from seohead.crawl.linkgraph import inlink_composition

        if stored_scan is None:
            # The crawled site's own host: the in-memory graph holds every edge the
            # crawl recorded, external destinations included, and without a host it
            # would advise adding a contextual link to somebody else's site (#208).
            link_position = inlink_composition(result.links, urlsplit(url).hostname or "")
        else:
            # The stored graph needs no equivalent: its population is built from
            # destinations that appear in the pages table, so an uncrawled external
            # URL is outside it by construction -- a stricter filter than a host
            # match, and it labels itself "crawled_destinations" rather than
            # claiming to have judged the whole graph.
            from seohead.crawl.sql_graph import StoredGraph
            from seohead.crawl.sql_graph_output import composition

            with StoredGraph(stored_scan.con) as graph:
                link_position = composition(graph, max_pages=100_000)
        # "Never linked from body content" is a claim about every inlink a page
        # has -- exactly the shape #246 asks graph-wide claims to withhold on a
        # partial crawl: the missing frontier could still hold the one content
        # link that would clear this page. Unlike LOW_LINK_SCORE and its three
        # siblings (see aggregate.GRAPH_WIDE_FINDING_CHECKS), this is computed
        # here rather than in seohead.sf, so it needs its own guard instead of
        # joining that withholding pass.
        if result.partial:
            ctx.skip(
                "INLINK_BOILERPLATE_ONLY",
                "crawl is partial: 'never linked from body content' cannot be "
                "proven when the crawl did not reach every URL",
            )
        else:
            for page in link_position["pages"]:
                if page["boilerplate_only"]:
                    ctx.add(
                        "INLINK_BOILERPLATE_ONLY",
                        target_url=page["url"],
                        occurrences_count=page["inlinks_total"],
                        details={"by_position": page["by_position"]},
                    )

    # Same shape again (issue #125): pure functions over the crawl's own LinkEdge/FormEdge
    # evidence, never through the SF-export analyzer.  A URL-list run has no such retained
    # evidence, but a saved scan or SpiderResult can still answer the three predicates that do
    # not need one site host.  FOLLOW_AND_NOFOLLOW_INLINKS is different: without a crawl start
    # URL, a multi-host URL list has no defensible definition of "internal" and must say so.
    from contextlib import nullcontext

    from seohead.crawl import link_findings
    from seohead.crawl.sql_graph import StoredGraph

    links = getattr(result, "links", None)
    forms = getattr(result, "forms", None)
    has_link_evidence = not stored_list and (stored_scan is not None or links is not None)
    has_form_evidence = not stored_list and (stored_scan is not None or forms is not None)
    link_rel_summary: dict[str, Any] = {
        "measured": False,
        "reason": "full link attributes were not retained",
    }
    with StoredGraph(stored_scan.con) if stored_scan else nullcontext(None) as graph:
        if has_link_evidence:
            for item in (
                graph.iter_localhost_findings()
                if graph
                else link_findings.outlinks_to_localhost(links)
            ):
                ctx.add("OUTLINK_TO_LOCALHOST", target_url=item["target_url"], details=item)
            crawl_host = (urlsplit(start_norm).hostname or "") if url else ""
            if crawl_host:
                # Preserve the established owner for the mixed-state verdict.
                # The detail helper enriches only destinations this predicate
                # already proved, rather than becoming an untracked parallel
                # route that can silently drift from the audit's coverage gate.
                mixed = (
                    None
                    if graph
                    else set(link_findings.follow_and_nofollow_inlinks(links, crawl_host))
                )
                for item in link_findings.follow_and_nofollow_inlink_details(
                    graph.iter_links() if graph else links, crawl_host
                ):
                    if mixed is not None and item["target_url"] not in mixed:
                        continue
                    ctx.add(
                        "FOLLOW_AND_NOFOLLOW_INLINKS", target_url=item["target_url"], details=item
                    )
                for item in link_findings.internal_nofollow_outlinks(
                    graph.iter_links() if graph else links, crawl_host
                ):
                    ctx.add(
                        "INTERNAL_NOFOLLOW_OUTLINKS", target_url=item["target_url"], details=item
                    )
                if settings["link_attributes"]["capture"]:
                    safely_upgraded = {
                        page.url
                        for page in ctx.pages
                        if page.status_code is not None
                        and 300 <= int(page.status_code) <= 399
                        and str(page.metrics.get("_record", {}).get("redirect_url") or "")
                        .lower()
                        .startswith("https://")
                    }
                    for item in link_findings.http_links_on_https_pages(
                        graph.iter_links() if graph else links,
                        crawl_host,
                        safely_upgraded,
                    ):
                        ctx.add("HTTP_LINK_ON_HTTPS", target_url=item["target_url"], details=item)
                else:
                    ctx.skip(
                        "HTTP_LINK_ON_HTTPS",
                        "link_attributes.capture is false; original href schemes were not retained",
                    )
            else:
                no_host = "no crawl start URL is available to identify one site's internal links"
                ctx.skip("FOLLOW_AND_NOFOLLOW_INLINKS", no_host)
                ctx.skip("INTERNAL_NOFOLLOW_OUTLINKS", no_host)
        else:
            reason = "crawl-list input retains no link-edge evidence"
            ctx.skip("OUTLINK_TO_LOCALHOST", reason)
            ctx.skip("FOLLOW_AND_NOFOLLOW_INLINKS", reason)
            ctx.skip("INTERNAL_NOFOLLOW_OUTLINKS", reason)
            ctx.skip("HTTP_LINK_ON_HTTPS", reason)

        if has_form_evidence:
            for item in (
                graph.iter_insecure_forms() if graph else link_findings.form_url_insecure(forms)
            ):
                ctx.add("FORM_URL_INSECURE", target_url=item["target_url"], details=item)
            for item in (
                graph.iter_password_forms_on_http()
                if graph
                else link_findings.forms_on_http_pages_with_password(forms)
            ):
                ctx.add("FORM_ON_HTTP_URL", target_url=item["target_url"], details=item)
        else:
            reason = "crawl-list input retains no form evidence"
            ctx.skip("FORM_URL_INSECURE", reason)
            ctx.skip("FORM_ON_HTTP_URL", reason)

        # These two need optional LinkEdge attributes and retain their existing capture gate.
        # They are not part of the always-recorded link/form evidence contract above.
        if url and has_link_evidence and settings["link_attributes"]["capture"]:
            for item in (
                graph.iter_unsafe_cross_origin()
                if graph
                else link_findings.unsafe_cross_origin_links(links)
            ):
                ctx.add("UNSAFE_CROSS_ORIGIN_LINK", target_url=item["target_url"], details=item)
            for item in (
                graph.iter_protocol_relative()
                if graph
                else link_findings.protocol_relative_links(links)
            ):
                ctx.add("PROTOCOL_RELATIVE_LINK", target_url=item["target_url"], details=item)
            site_host = urlsplit(start_norm).hostname or ""
            for item in (
                graph.iter_internal_sponsored_ugc(site_host)
                if graph
                else link_findings.internal_sponsored_ugc_links(links, site_host)
            ):
                ctx.add("INTERNAL_LINK_SPONSORED_UGC", target_url=item["target_url"], details=item)
            for item in graph.iter_mixed_rel() if graph else link_findings.mixed_rel_links(links):
                ctx.add("MIXED_LINK_REL", target_url=item["target_url"], details=item)
            link_rel_summary = graph.rel_summary() if graph else link_findings.rel_summary(links)
            from seohead.sf.core.rules import _body_unavailable

            html_total = html_unavailable = 0
            for page in ctx.html_pages():
                html_total += 1
                html_unavailable += int(_body_unavailable(page.metrics.get("_record", {})))
            link_rel_summary["coverage"] = {
                "html_pages": html_total,
                "html_pages_unavailable": html_unavailable,
                "scope": "captured link occurrences; unparsed pages are excluded",
            }
            link_rel_summary["state"] = "partial" if html_unavailable else "complete"
            if html_unavailable:
                link_rel_summary["reason"] = (
                    f"{html_unavailable} HTML page(s) were not parsed; their rel and disclosure evidence was not measured"
                )
            if html_total == html_unavailable and not link_rel_summary["by_rel"]["total"]:
                reason = "no parsed HTML page or captured link occurrence provides rel evidence"
                link_rel_summary.update(measured=False, state="unavailable", reason=reason)
                link_rel_summary["by_rel"] = {key: None for key in link_rel_summary["by_rel"]}
                ctx.skip("MIXED_LINK_REL", reason)
                ctx.skip("OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE", reason)
            if graph:
                disclosure = graph.sponsored_disclosures(site_host)
                link_rel_summary["disclosure"] = {
                    key: value for key, value in disclosure.items() if key != "findings"
                }
                for item in disclosure["findings"]:
                    ctx.add(
                        "OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE",
                        target_url=item["target_url"],
                        details=item,
                    )
                if disclosure["coverage"]["unavailable"]:
                    ctx.skip(
                        "OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE",
                        "some sponsored-link contexts are unavailable; see summary.link_rel.disclosure",
                    )
            else:
                reason = "crawl has no retained complete source documents for disclosure replay"
                link_rel_summary["disclosure"] = {"state": "unavailable", "reason": reason}
                ctx.skip("OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE", reason)
        else:
            reason = (
                "link_attributes.capture is false; full rel tokens were not retained"
                if not settings["link_attributes"]["capture"]
                else "link evidence or crawl site host is unavailable"
            )
            link_rel_summary["reason"] = reason
            ctx.skip("MIXED_LINK_REL", reason)
            ctx.skip("OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE", reason)

    # Pages With JavaScript Errors (#1015): read from the retained render console sidecars,
    # never re-rendered here. Unreadable evidence is a stated skip, not a clean page.
    if stored_scan is not None and not stored_list and hasattr(stored_scan, "path"):
        from seohead.storage import browser_artifacts

        console = browser_artifacts.console_error_pages(stored_scan.con, stored_scan.path)
        # A skip is retracted by any sibling add(), so an unreadable record only becomes
        # the stated reason when no readable page produced a finding.
        if console["unreadable"] and not console["pages"]:
            ctx.skip(
                "JS_CONSOLE_ERRORS",
                f"{console['unreadable']} retained browser console record(s) could not be read",
            )
        elif not console["captured"]:
            ctx.skip(
                "JS_CONSOLE_ERRORS",
                "no browser console was retained; enable rendering.artifacts.console_errors",
            )
        for item in console["pages"]:
            ctx.add(
                "JS_CONSOLE_ERRORS",
                target_url=item["target_url"],
                details={"error_count": item["error_count"], "errors": item["errors"]},
            )
    else:
        # The legacy graph path keeps no render sidecars, so it states the same
        # skip the stored-scan path states for an uncaptured console (keeps parity).
        ctx.skip(
            "JS_CONSOLE_ERRORS",
            "no browser console was retained; enable rendering.artifacts.console_errors",
        )
    # Per-URL security headers (#1013): the final response of each HTML page, judged from the
    # retained scan. Without a stored scan there is no per-URL header evidence to judge.
    from seohead.crawl import security_headers

    if stored_scan is not None:
        header_evidence = security_headers.evaluate(stored_scan.con)
        for check_id, items in header_evidence["findings"].items():
            for item in items:
                ctx.add(check_id, target_url=item["target_url"], details=item)
            if items:
                continue
            if header_evidence["pages_unmeasured"]:
                ctx.skip(
                    check_id,
                    f"{header_evidence['pages_unmeasured']} HTML pages have no parseable "
                    "stored response headers",
                )
            elif not header_evidence["pages_measured"]:
                ctx.skip(check_id, "no HTML page with a 2xx response was stored")
    else:
        for check_id in security_headers.CHECK_IDS:
            ctx.skip(check_id, "crawl was not stored; per-URL response headers are not retained")

    if stored_scan is not None:
        from seohead.storage.retained_link_findings import RetainedLinkFindings

        with RetainedLinkFindings(
            stored_scan.con,
            max_locations=audit_config.get("output", {}).get("max_locations_per_issue", 200),
            hop_cap=ctx.thresholds.get("redirect_hop_cap", 20),
        ) as retained_links:
            for item in retained_links.findings():
                ctx.add(item.pop("check"), **item)
            if stored_scan.con.execute("SELECT 1 FROM links LIMIT 1").fetchone() is None:
                for check in (
                    "BROKEN_INTERNAL_LINK",
                    "LINK_TO_5XX",
                    "INTERNAL_LINK_TO_REDIRECT",
                    "INTERNAL_LINK_NO_ANCHOR",
                ):
                    ctx.skip(check, "native scan retains no hyperlink occurrences")
            external_summary = getattr(result, "external_summary", None)
            if not external_summary:
                ctx.skip(
                    "EXTERNAL_NO_RESPONSE",
                    "native external destination checks were not captured; enable discovery.external.crawl",
                )
            elif not sum(int(external_summary.get(key) or 0) for key in ("fetched", "failed")):
                ctx.skip(
                    "EXTERNAL_NO_RESPONSE",
                    "captured external phase attempted no measurable destinations; its summary records skipped or blocked targets",
                )

    # A broken bookmark is not a link-status problem: the fragment resolves
    # inside the retained destination document, which only a native scan keeps
    # (issue #827). The evaluation is read-only and offline -- nothing is
    # fetched to answer it, and missing or incomplete bodies stay skipped
    # rather than becoming findings.
    from seohead.storage import fragment_links

    fragment_evaluation: dict[str, Any] | None = None
    if stored_scan is not None:
        storage = settings.get("storage")
        body_limit = storage.get("max_body_bytes") if isinstance(storage, dict) else None
        if type(body_limit) is not int or body_limit <= 0:
            body_limit = fragment_links.DEFAULT_MAX_DECODED_BYTES
        fragment_evaluation = fragment_links.evaluate(stored_scan.con, max_decoded_bytes=body_limit)
        fragment_states = fragment_evaluation["states"]
        if fragment_evaluation["coverage"]["source_documents_evaluated"]:
            bookmark_findings = fragment_links.findings(fragment_evaluation)
            for item in bookmark_findings:
                ctx.add(
                    "BROKEN_BOOKMARK",
                    target_url=item["target_url"],
                    occurrences_count=item["occurrences_count"],
                    locations=item["locations"],
                    details={
                        "fragment": item["fragment"],
                        "decoded_fragment": item["decoded_fragment"],
                        "destination_representation": item["destination_representation"],
                        "locations_omitted": item["locations_omitted"],
                        "occurrences_skipped": fragment_states["skipped"],
                        "coverage": fragment_evaluation["coverage"]["state"],
                    },
                )
            if not bookmark_findings and fragment_evaluation["coverage"]["state"] != "complete":
                # Evaluated sources but no finding to fire, while part of the
                # anchors went unanswered (skipped destinations, unavailable
                # lanes, unresolvable hrefs, capped or truncated inventories):
                # silent here would read as a clean pass the partial evidence
                # cannot support.
                ctx.skip(
                    "BROKEN_BOOKMARK",
                    "fragment evidence is partial; summary.fragment_links names "
                    "each skipped occurrence and unavailable source",
                )
        else:
            ctx.skip(
                "BROKEN_BOOKMARK",
                "scan retains no complete HTML document for fragment resolution",
            )
    else:
        ctx.skip(
            "BROKEN_BOOKMARK",
            "input retains no HTML/DOM bodies; fragment targets are measured "
            "only from a native retained scan",
        )

    native_run = {}
    if stored_scan is not None:
        retained = stored_scan.con.execute(
            "SELECT scan_uuid,corpus_partial,source_kind,config_fingerprint FROM scan WHERE singleton=1"
        ).fetchone()
        native_run = {
            "scan_uuid": retained["scan_uuid"],
            "corpus_partial": bool(retained["corpus_partial"]),
            "source_kind": retained["source_kind"],
            "config_fingerprint": retained["config_fingerprint"],
        }
    audit_result = aggregate(
        ctx,
        {
            **native_run,
            "input_mode": "crawl" if url else "crawl-list",
            "source": url or "url-list",
            "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "collector": "seohead.crawl",
            "crawl_partial": result.partial,
            "crawl_stopped_reason": result.stopped_reason,
            # Categorical and always present, unlike stopped_reason which is
            # only a sentence when something went wrong.
            "crawl_finish_reason": result.finish_reason,
            "crawl_resumed": result.resumed,
            # The opt-in external-destination phase's own coverage statement
            # (#746): policy used, per-outcome counts and its finish reason —
            # null when the phase was off, so an audit reader can always tell
            # "not checked" apart from "checked and clean".
            # getattr rather than a plain attribute: list mode hands this a
            # collect_urls CrawlResult, which has no external phase at all —
            # None is the honest value there, not an AttributeError.
            "external_checks": getattr(result, "external_summary", None) or None,
            # Resolved values of every setting that can change what was found.
            # Without these two reports on the same site are not comparable.
            "crawl_config": crawl_config.manifest(settings),
            "effective_max_requests_per_second": (
                "unbounded"
                if settings["speed"]["min_delay_seconds"] == 0
                else crawl_config.effective_request_rate(settings)
            ),
            # "The site is fine" and "the site was fine when we last looked" are different
            # claims — cache_replay says which one this report can support, and cache_stats
            # says how much of the corpus was measured now versus remembered.
            "cache_replay": result.cache_replay,
            "cache_stats": result.cache_stats,
            # Whether this run is a false-green that must not reach a health
            # score (#18), and the escalation that ran (if any) to check.
            "requires_rendering": requires_rendering,
            "requires_rendering_reason": requires_rendering_reason,
            "render_escalation": render_summary or None,
        },
        {},
        sitemap_summary,
    )
    if streaming:
        header, collections = audit_result.audit_v2_parts()
        header["summary"]["link_rel"] = link_rel_summary
        analysis_segments = settings["analysis"]["segments"]
        header["segments"] = (
            _segment_counts(
                result.pages, collections["/issues"], settings["scope"], analysis_segments
            )
            if settings["scope"]["segments"] or analysis_segments
            else {}
        )
        if fragment_evaluation is not None:
            header["summary"]["fragment_links"] = {
                "analysis": fragment_evaluation["analysis"],
                "states": fragment_evaluation["states"],
                "coverage": fragment_evaluation["coverage"],
            }
        if stored_scan is not None:
            from seohead.sf.core.evidence_contract import (
                attach_contract_parts,
                attach_saved_corpus_header,
            )

            identity = stored_scan.con.execute(
                "SELECT scan_uuid FROM scan WHERE singleton=1"
            ).fetchone()[0]
            header, collections["/issues"] = attach_contract_parts(
                header, collections["/issues"], scan_uuid=identity, con=stored_scan.con
            )
            header = attach_saved_corpus_header(
                header, stored_scan.con, derived=saved_corpus, collections=collections
            )
        # The lazy page/group factories retain the disk-backed context until
        # the writer has consumed every collection.
        for rows in collections.values():
            rows._context_owner = ctx
        return {
            "summary": header["summary"],
            "segments": header["segments"],
            "requires_rendering": requires_rendering,
            "requires_rendering_reason": requires_rendering_reason,
            "render_escalation": render_summary,
        }, (header, collections)
    audit = audit_result.to_json()
    audit["summary"]["link_rel"] = link_rel_summary
    # Page and issue counts per named segment (#358) -- only when the operator
    # actually declared segments, so a plain crawl's audit.json is unchanged.
    analysis_segments = settings["analysis"]["segments"]
    audit["segments"] = (
        _segment_counts(result.pages, audit["issues"], settings["scope"], analysis_segments)
        if settings["scope"]["segments"] or analysis_segments
        else {}
    )
    if fragment_evaluation is not None:
        audit["summary"]["fragment_links"] = {
            "analysis": fragment_evaluation["analysis"],
            "states": fragment_evaluation["states"],
            "coverage": fragment_evaluation["coverage"],
        }

    if stored_scan is not None:
        from seohead.sf.core.evidence_contract import attach_contract, attach_saved_corpus

        scan_identity = stored_scan.con.execute(
            "SELECT scan_uuid FROM scan WHERE singleton=1"
        ).fetchone()[0]
        audit = attach_contract(audit, scan_uuid=scan_identity, con=stored_scan.con)
        audit = attach_saved_corpus(audit, stored_scan.con, derived=saved_corpus)
    # ``to_json`` above has copied the report payload. The native page store is
    # no longer needed and must not leave its temporary SQLite file behind.
    ctx.close()

    tasks_written: dict[str, str] = {}
    if out_dir:
        with open(os.path.join(out_dir, "audit.json"), "w", encoding="utf-8") as fh:
            json.dump(audit, fh, ensure_ascii=False, indent=2)
        if settings["output"]["write_tasks"]:
            # build_tasks has always taken an audit document, and a native crawl
            # has always produced one -- the two were simply never joined, so a
            # crawl done without Screaming Frog produced findings and no list of
            # what to do about them. Same pipeline `sf run --tasks` drives, over
            # this crawl's own audit: no network, no second pass.
            from seohead.sf.tasks import build_tasks, write_tasks

            # None, not this crawl's own config: tasks_pipeline lives in the
            # sf config (seohead/sf/config.py), a different document from the
            # crawler settings, and passing the crawler's path here would fail
            # on the first .get(). Defaults it is, until someone asks for the
            # two to be joined.
            backlog = build_tasks(audit, None)
            json_path, md_path = write_tasks(
                backlog,
                os.path.join(out_dir, "tasks.json"),
                os.path.join(out_dir, "tasks.md"),
            )
            tasks_written = {"tasks_json": json_path, "tasks_md": md_path}

    response = {
        "urls_collected": len(result.pages),
        "tasks": tasks_written,
        "partial": result.partial,
        "stopped_reason": result.stopped_reason,
        "finish_reason": result.finish_reason,
        "resumed": result.resumed,
        "discovery": discovery,
        "limitations": result.limitations,
        "link_position": link_position,
        "summary": audit["summary"],
        "segments": audit["segments"],
        "checks_skipped": len(audit["run"].get("checks_skipped", [])),
        "out_dir": out_dir,
        # See the matching keys in audit["run"] for why these are surfaced twice: a caller
        # reading only this dict (no out_dir, no audit.json) must still be able to tell a
        # replayed answer from a measured one.
        "cache_replay": result.cache_replay,
        "cache_stats": result.cache_stats,
        "requires_rendering": requires_rendering,
        "requires_rendering_reason": requires_rendering_reason,
        "render_escalation": render_summary,
    }

    return response, audit


def crawl_describe_settings() -> dict[str, Any]:
    """Every crawl-site config setting: path, type, default, and description.

    The MCP half of #23: an agent can ask what it can configure instead of
    guessing a key name or reading the source. Backed by the same
    ``describe_settings`` that ``crawl-site --config-help`` prints, so the CLI
    and MCP surfaces cannot describe the same setting two different ways.
    """
    from seohead.crawl import settings as crawl_config

    return {
        "settings": crawl_config.describe_settings(),
        "capabilities": {"sitemap_only_retained": True, "full_site_native_sqlite": True},
    }


def images_download(
    urls: list[str] | None = None,
    output_dir: str | None = None,
    options: dict[str, Any] | None = None,
) -> dict[str, Any]:
    if not output_dir:
        raise ValueError("output_dir required")
    results = downloader.download_images(urls or [], output_dir, options or {})
    return {"count": len(results), "results": results}


def images_optimize(
    files: list[str] | None = None,
    settings: dict[str, Any] | None = None,
    scan: str | None = None,
    max_images: int = 50,
) -> dict[str, Any]:
    """Optimize image files with explicit output semantics.

    Provide ``settings.out_dir`` for non-destructive output. Rewriting source files requires the
    caller to opt in with ``settings.in_place=true``; the optimizer's backup safeguards still apply.
    """
    if scan:
        if files:
            raise ValueError("provide files[] or scan, not both")
        normalized = dict(settings or {})
        if not normalized.get("out_dir") or normalized.get("in_place"):
            raise ValueError("scan image batches require settings.out_dir and forbid in_place")
        if type(max_images) is not int or not 1 <= max_images <= 50:
            raise ValueError("max_images must be 1..50")
        from seohead.mcp.evidence_handlers import scan_evidence

        images: dict[str, list[dict[str, Any]]] = {}
        offset = 0
        scanned = 0
        total = None
        while scanned < 10_000 and len(images) < max_images:
            page = scan_evidence(
                scan, section="resources", offset=offset, limit=min(1000, 10_000 - scanned)
            )
            evidence = page.get("evidence") or {}
            if evidence.get("state") == "unavailable":
                return {
                    "ok": False,
                    "state": "unavailable",
                    "reason": evidence.get("reason"),
                    "results": [],
                }
            total = evidence.get("total")
            occurrences = evidence.get("occurrences") or []
            if not occurrences:
                break
            for occurrence in occurrences:
                scanned += 1
                if occurrence.get("kind") != "image" or occurrence.get("state") in {
                    "excluded",
                    "failed",
                    "budget",
                }:
                    continue
                url = occurrence.get("resolved_url")
                if not isinstance(url, str) or not url:
                    continue
                images.setdefault(url, []).append(
                    {
                        "occurrence_id": occurrence.get("occurrence_id"),
                        "source_page_url_id": occurrence.get("page_url_id"),
                        "source_document_id": occurrence.get("source_document_id"),
                        "representation": occurrence.get("representation"),
                        "ordinal": occurrence.get("ordinal"),
                    }
                )
                if len(images) >= max_images:
                    break
            offset += len(occurrences)
            if not evidence.get("has_more"):
                break
        has_more = scanned < (total or 0)
        if not images:
            return {
                "ok": False,
                "state": "empty",
                "reason": "the retained scan has no eligible image declarations",
                "count": 0,
                "results": [],
            }
        with tempfile.TemporaryDirectory(prefix="seohead-scan-images-") as temp_dir:
            download_budget = 100 * 1024 * 1024
            downloaded_bytes = 0
            downloads = []
            for url in images:
                remaining = download_budget - downloaded_bytes
                if remaining <= 0:
                    downloads.append(
                        {
                            "url": url,
                            "ok": False,
                            "error": "scan image download byte budget exhausted",
                        }
                    )
                    continue
                row = downloader.download_images(
                    [url],
                    temp_dir,
                    {"max_bytes": min(25 * 1024 * 1024, remaining)},
                )[0]
                downloads.append(row)
                if row.get("ok"):
                    downloaded_bytes += int(row.get("bytes") or 0)
            downloaded = {row["url"]: row for row in downloads}
            paths = [row["path"] for row in downloads if row.get("ok") and row.get("path")]
            optimized = optimizer.optimize_files(paths, normalized) if paths else {"results": []}
            by_source = {row.get("file"): row for row in optimized.get("results", [])}
            results = []
            for url, instances in images.items():
                download = downloaded.get(
                    url, {"ok": False, "error": "image download result unavailable"}
                )
                result = by_source.get(download.get("path"))
                download_report = {key: value for key, value in download.items() if key != "path"}
                optimization_report = (
                    {key: value for key, value in result.items() if key not in {"source", "file"}}
                    if result
                    else None
                )
                results.append(
                    {
                        "url": url,
                        "instances": instances,
                        "download": download_report,
                        "optimization": optimization_report,
                    }
                )
        optimization_ok = all(
            row.get("download", {}).get("ok")
            and isinstance(row.get("optimization"), dict)
            and row["optimization"].get("ok")
            for row in results
        )
        any_optimized = any(
            isinstance(row.get("optimization"), dict) and row["optimization"].get("ok")
            for row in results
        )
        totals_available = all(
            isinstance(row.get("optimization"), dict)
            and row["optimization"].get("before_bytes") is not None
            and row["optimization"].get("after_bytes") is not None
            for row in results
        )
        return {
            "ok": any_optimized,
            "partial": not optimization_ok,
            "source": "retained_scan_resource_graph",
            "state": "partial" if has_more or not optimization_ok else "complete",
            "scan_resources_read": scanned,
            "scan_resources_total": total,
            "downloaded_bytes": downloaded_bytes,
            "download_byte_budget": 100 * 1024 * 1024,
            "has_more": has_more,
            "count": len(results),
            "total_before": sum(row["optimization"]["before_bytes"] for row in results)
            if totals_available
            else None,
            "total_after": sum(row["optimization"]["after_bytes"] for row in results)
            if totals_available
            else None,
            "total_saved": sum(
                row["optimization"]["before_bytes"] - row["optimization"]["after_bytes"]
                for row in results
            )
            if totals_available
            else None,
            "totals_reason": "all selected images were measured"
            if totals_available
            else "one or more selected images could not be optimized",
            "results": results,
        }
    if not files:
        raise ValueError("files[] required")
    return optimizer.optimize_files(files, settings or {})


def keywords_cluster(**params: Any) -> dict[str, Any]:
    return clusterer.run_clusterer(params)


def robots_check(
    url: str | None = None, user_agent: str = "*", paths: list[str] | None = None
) -> RobotsCheckResult:
    if not url:
        raise ValueError("url required")
    return robots_core.check_robots(url, user_agent=user_agent, paths=paths)


def headers_check(url: str | None = None, method: str = "GET") -> dict[str, Any]:
    if not url:
        raise ValueError("url required")
    method = str(method).upper()
    if method not in {"GET", "HEAD"}:
        raise ValueError("headers-check method must be GET or HEAD")
    return headers_core.check_headers(url, method=method)


def asset_weight_check(
    url: str | None = None, file_size_threshold: int | None = None
) -> dict[str, Any]:
    if not url:
        raise ValueError("url required")
    kwargs: dict[str, Any] = {}
    if file_size_threshold is not None:
        kwargs["file_size_threshold"] = int(file_size_threshold)
    return asset_weight.analyze_page_asset_weight(url, **kwargs)


def links_check(
    url: str | None = None, internal_only: bool = False, limit: int = 200
) -> dict[str, Any]:
    if not url:
        raise ValueError("url required")
    return links_core.check_links(url, internal_only=internal_only, limit=limit)


def hreflang_check(url: str | None = None) -> dict[str, Any]:
    if not url:
        raise ValueError("url required")
    return hreflang_core.check_hreflang(url)


def domain_profile(domain: str | None = None, with_tls: bool = True) -> dict[str, Any]:
    if not domain:
        raise ValueError("domain required")
    from seohead.recon import domain as domain_core

    return domain_core.profile_domain(domain, with_tls=with_tls)


def cdn_check(url: str | None = None) -> dict[str, Any]:
    if not url:
        raise ValueError("url required")
    from seohead.recon import cdn as cdn_core

    return cdn_core.check_cdn(url)


def tech_detect(url: str | None = None) -> dict[str, Any]:
    if not url:
        raise ValueError("url required")
    from seohead.recon import tech as tech_core

    return tech_core.detect_tech(url)


def security_check(url: str | None = None, probe_paths: bool = False) -> dict[str, Any]:
    if not url:
        raise ValueError("url required")
    from seohead.recon import security as security_core

    return security_core.check_security(url, probe_paths=bool(probe_paths))


def schema_check(url: str | None = None, html: str | None = None) -> dict[str, Any]:
    if not url and not html:
        raise ValueError("url or html required")
    from seohead.checks import schema_org as schema_core

    return schema_core.check_schema(url=url, html=html)


def schema_build(
    url: str | None = None, html: str | None = None, override_type: str | None = None
) -> dict[str, Any]:
    if not url and not html:
        raise ValueError("url or html required")
    from seohead.checks import schema_build as builder

    return builder.build_schema(url=url, html=html, override_type=override_type)


def log_analyze(
    path: str | None = None,
    verify_bots: bool = False,
    max_lines: int = 5_000_000,
    offset: int = 0,
    limit: int = 100,
    max_dns: int = 20,
    bot: str | None = None,
    ip: str | None = None,
) -> dict[str, Any]:
    if not path:
        raise ValueError("path required (web server access-log file)")
    from seohead.checks import logs as logs_core

    return logs_core.analyze_log(
        path,
        verify_bots=verify_bots,
        max_lines=max_lines,
        offset=offset,
        limit=limit,
        max_dns=max_dns,
        bot=bot,
        ip=ip,
    )


def logs_import(
    path: str, project: str, max_lines: int = 100_000_000, max_files: int = 1000
) -> dict[str, Any]:
    from seohead.checks.logstore import import_logs

    return import_logs(path, project, max_lines=max_lines, max_files=max_files)


def logs_summary(
    source: str,
    view: str = "urls",
    limit: int = 100,
    offset: int = 0,
    bot: str = "",
    family: str = "",
    ip: str = "",
    date_from: str = "",
    date_to: str = "",
    status: int | None = None,
    section: str = "",
) -> dict[str, Any]:
    from seohead.checks.logstore import log_summary

    return log_summary(
        source,
        view=view,
        limit=limit,
        offset=offset,
        bot=bot,
        family=family,
        ip=ip,
        date_from=date_from,
        date_to=date_to,
        status=status,
        section=section,
    )


def logs_join(
    source: str,
    scan: str,
    view: str = "scan_without_bot_visits",
    base_url: str = "",
    family: str = "",
    bot: str = "",
    date_from: str = "",
    date_to: str = "",
    limit: int = 100,
    offset: int = 0,
) -> dict[str, Any]:
    from seohead.checks.logstore import join_log_scan

    return join_log_scan(
        source,
        scan,
        view=view,
        base_url=base_url,
        family=family,
        bot=bot,
        date_from=date_from,
        date_to=date_to,
        limit=limit,
        offset=offset,
    )


def logs_bots(
    source: str,
    verify_bots: bool = False,
    limit: int = 100,
    offset: int = 0,
    bot: str = "",
    family: str = "",
    ip: str = "",
    verdict: str = "",
    max_dns: int = 20,
) -> dict[str, Any]:
    from seohead.checks.logbots import bot_verification

    return bot_verification(
        source,
        verify_bots=verify_bots,
        limit=limit,
        offset=offset,
        bot=bot,
        family=family,
        ip=ip,
        verdict=verdict,
        max_dns=max_dns,
    )


def regions_check(
    url: str | None = None, extra: list[str] | None = None, limit: int = 12, render: bool = False
) -> dict[str, Any]:
    if not url:
        raise ValueError("url required (any site page, usually the home page)")
    from seohead.recon import regions as regions_core

    return regions_core.analyze_regions(url, extra=extra or [], limit=limit, render=bool(render))


def site_audit(
    url: str | None = None,
    urls: list[str] | None = None,
    limit: int = 25,
    concurrency: int = 5,
    render: bool = False,
    skip: list[str] | None = None,
    crux_evidence: dict[str, Any] | None = None,
) -> dict[str, Any]:
    if not url:
        raise ValueError("url required (site home page)")
    from seohead.audit.site import audit_site

    return audit_site(
        url,
        urls=urls,
        limit=limit,
        concurrency=concurrency,
        render=bool(render),
        skip=skip,
        tools=HANDLERS,
        crux_evidence=crux_evidence,
    )


def report_build(
    audit: Any = None,
    fmt: str = "xlsx",
    out: str | None = None,
    project: str | None = None,
    view: str | None = None,
    offset: int = 0,
    lang: str = "en",
    pdf_policy: str | None = None,
) -> dict[str, Any]:
    if audit is None:
        raise ValueError("audit required: audit document or path to its JSON representation")
    from seohead.reports import build_report

    options = {"pdf_policy": pdf_policy} if pdf_policy is not None else {}
    return build_report(
        audit, fmt=fmt, path=out, project=project, view=view, offset=offset, lang=lang, **options
    )


def facts_export(sites: list[dict[str, Any]] | None = None) -> dict[str, Any]:
    """Build a facts.v1 multi-site export from already-produced crawl/site
    audits. Makes zero network requests and picks no winner -- see
    seohead.reports.facts for the state machine and the two failure modes
    it refuses (site-identity mismatch, duplicate registrable domain)."""
    if not sites:
        raise ValueError("sites required: a list of {label, crawl_audit, site_audit} descriptors")
    from seohead.reports.facts import build_facts_export

    try:
        result = build_facts_export(sites)
    except ValueError as exc:
        return {"ok": False, "error": str(exc)}
    return {"ok": True, **result}


def _load_audit(
    value: Any, label: str, diagnostics: list[dict[str, str]] | None = None
) -> dict[str, Any]:
    """Accept a document, JSON path or validated scan without changing its contents."""
    from seohead.storage.inputs import load_audit_document

    return load_audit_document(value, label, diagnostics)


def compare_crawls(
    before: Any = None,
    after: Any = None,
    force: bool = False,
    correspondence: Any = None,
    out_dir: str | None = None,
    compression: str = "none",
    resume: bool = False,
    progress: Any = None,
    cancel_event: Any = None,
) -> dict[str, Any]:
    """Diff two audits: which findings were fixed, which are new, which pages
    dropped out of the crawl entirely. See seohead.sf.core.compare for why
    "fixed" and "no longer crawled" are kept apart rather than merged."""
    from seohead.sf.core.compare import compare
    from seohead.storage.inputs import load_audit_source

    diagnostics: list[dict[str, str]] = []
    before_doc = load_audit_source(before, "before", diagnostics)
    after_doc = load_audit_source(after, "after", diagnostics)
    try:
        result = compare(
            before_doc,
            after_doc,
            force=force,
            correspondence=correspondence,
            out_dir=out_dir,
            compression=compression,
            resume=resume,
            progress=progress,
            cancel_event=cancel_event,
        )
    finally:
        if not hasattr(before, "iter_collection") and hasattr(before_doc, "close"):
            before_doc.close()
        if (
            not hasattr(after, "iter_collection")
            and after_doc is not before_doc
            and hasattr(after_doc, "close")
        ):
            after_doc.close()
    if diagnostics:
        result["input_diagnostics"] = diagnostics
    return result


def compare_read(
    manifest_path: str,
    file: str,
    offset: int = 0,
    limit: int = 200,
    sort: str = "url",
    direction: str = "asc",
    contains: str | None = None,
    segment: str | None = None,
    change: str | None = None,
    max_bytes: int = 1_048_576,
) -> dict[str, Any]:
    """Read a bounded page from a validated compare.v2 file."""
    from seohead.sf.core.compare_store import read_compare_page

    try:
        return read_compare_page(
            manifest_path,
            file,
            offset=offset,
            limit=limit,
            sort=sort,
            direction=direction,
            contains=contains,
            segment=segment,
            change=change,
            max_bytes=max_bytes,
        )
    except (ValueError, OSError, TypeError) as exc:
        return {"ok": False, "state": "unavailable", "error": str(exc)}


def compare_status(out_dir: str) -> dict[str, Any]:
    """Read the current checkpoint or completed manifest for a comparison output path."""
    import json

    from seohead.core.filesystem import lock_exclusive, open_lock, unlock

    destination = Path(out_dir).absolute()
    if destination.is_symlink():
        raise ValueError("comparison output path must not be a symbolic link")
    manifest = destination / "compare.json"
    if manifest.is_file() and not manifest.is_symlink():
        result = json.loads(manifest.read_text(encoding="utf-8"))
        job = result.get("job") if isinstance(result, dict) else None
        return {
            "ok": True,
            "format": "compare.job-status.v1",
            "state": "finished",
            "stage": "complete",
            "job_id": job.get("id") if isinstance(job, dict) else None,
            "completed": 1,
            "total": 1,
            "resumable": False,
            "cache_available": True,
        }
    work = destination.with_name(f".{destination.name}.compare-work")
    if work.is_symlink() or not work.is_dir():
        return {"ok": False, "state": "unavailable", "error": "comparison job was not found"}
    checkpoint = work / "job.json"
    if checkpoint.is_symlink() or not checkpoint.is_file():
        return {"ok": False, "state": "unavailable", "error": "comparison checkpoint is unreadable"}
    data = json.loads(checkpoint.read_text(encoding="utf-8"))
    active = False
    try:
        fd = open_lock(work / ".writer.lock", create=False)
    except OSError:
        fd = None
    if fd is not None:
        try:
            lock_exclusive(fd)
        except OSError:
            active = True
        else:
            unlock(fd)
        finally:
            import os

            os.close(fd)
    return {
        "ok": True,
        "format": "compare.job-status.v1",
        **{
            key: data.get(key)
            for key in ("job_id", "state", "stage", "completed", "total", "reason")
        },
        "resumable": not active and data.get("state") in {"failed", "paused", "running"},
        "active": active,
        "cache_available": False,
    }


def compare_report(manifest_path: str, out_file: str, max_bytes: int = 65_536) -> dict[str, Any]:
    """Write a bounded Markdown summary to a new path from a validated compare.v2 package."""
    import hashlib
    import os
    import tempfile

    from seohead.sf.core.compare_store import compare_markdown

    report = compare_markdown(manifest_path, max_bytes=max_bytes)
    destination = Path(out_file).absolute()
    if destination.exists():
        raise FileExistsError(f"report output already exists: {destination}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".compare-report-", dir=destination.parent)
    data = report.encode("utf-8")
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.link(temporary, destination)
    finally:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(temporary)
    return {
        "ok": True,
        "format": "compare.v2.markdown",
        "report": str(destination),
        "bytes": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
    }


def compare_url(
    before: Any,
    after: Any,
    url: str,
    *,
    max_bytes: int = 65_536,
    max_lines: int = 200,
    left_representation: str = "static",
    right_representation: str = "static",
) -> dict[str, Any]:
    """Return exact-URL before/after facts and finding deltas without absence claims."""
    from collections.abc import Mapping

    from seohead.sf.core.compare import _iter_rows
    from seohead.storage.inputs import load_audit_source

    if not isinstance(url, str) or not url or len(url) > 8192:
        raise ValueError("url must be nonempty exact URL text of at most 8192 characters")
    if type(max_bytes) is not int or not 1 <= max_bytes <= 65_536:
        raise ValueError("max_bytes must be an integer 1..65536")
    if type(max_lines) is not int or not 1 <= max_lines <= 200:
        raise ValueError("max_lines must be an integer 1..200")
    representations = {"static", "rendered", "legacy_fragment"}
    if left_representation not in representations or right_representation not in representations:
        raise ValueError("representations must be static, rendered, or legacy_fragment")

    sources = []
    opened = []
    try:
        for value, label in ((before, "before"), (after, "after")):
            is_source = hasattr(value, "iter_collection")
            source = value if is_source else load_audit_source(value, label)
            sources.append(source)
            if not is_source and hasattr(source, "close"):
                opened.append(source)
    except BaseException:
        for source in opened:
            source.close()
        raise
    try:
        cards = []
        findings = []
        fields = (
            "status_code",
            "title",
            "meta_description",
            "h1",
            "word_count",
            "inlinks",
            "internal_outlinks",
            "canonical",
            "response_time",
            "size_bytes",
            "indexability",
        )
        aliases = {"response_time": "response_time_seconds", "size_bytes": "html_size_bytes"}

        def value_of(page: Mapping[str, Any], field: str) -> Any:
            metrics = page.get("metrics") if isinstance(page.get("metrics"), Mapping) else {}
            if field == "indexability":
                val = page.get(field, metrics.get(field))
                return val
            if field in page:
                return page[field]
            if field in metrics:
                return metrics[field]
            return None

        for source in sources:
            page = next(
                (item for item in _iter_rows(source, "pages") if item.get("url") == url), None
            )
            if page is None:
                cards.append({"state": "absent", "url": url, "fields": {}})
            else:
                facts = {}
                for field in fields:
                    value = value_of(page, field)
                    name = aliases.get(field, field)
                    facts[name] = {
                        "state": "measured" if value is not None else "unavailable",
                        "value": value,
                        **(
                            {}
                            if value is not None
                            else {"reason": f"source does not retain {field}"}
                        ),
                    }
                cards.append({"state": "present", "url": url, "fields": facts})
            findings.append(
                [
                    dict(issue)
                    for issue in _iter_rows(source, "issues")
                    if issue.get("target_url") == url
                ]
            )

        if cards[0]["state"] != "present" or cards[1]["state"] != "present":
            comparison_state = "not_rechecked"
            reason = (
                "both scans must contain this URL before field or finding deltas can be interpreted"
            )
            changes = {}
            finding_delta = {
                "state": comparison_state,
                "reason": reason,
                "entered": [],
                "left": [],
            }
        else:
            comparison_state = "compared"
            reason = None
            changes = {}
            for name in cards[0]["fields"]:
                left, right = cards[0]["fields"][name], cards[1]["fields"][name]
                if left["state"] != "measured" or right["state"] != "measured":
                    changes[name] = {
                        "state": "unavailable",
                        "reason": "field was not measured on both scans",
                    }
                elif left["value"] != right["value"]:
                    changes[name] = {
                        "state": "changed",
                        "before": left["value"],
                        "after": right["value"],
                    }
                else:
                    changes[name] = {"state": "unchanged", "value": left["value"]}
            before_by_check = {row.get("check"): row for row in findings[0]}
            after_by_check = {row.get("check"): row for row in findings[1]}
            from seohead.sf.core.compare import _run

            unavailable = []
            invalid_run = False
            for source in sources:
                run = _run(source)
                invalid_run = invalid_run or run.get("crawl_valid") is False
                for field in ("checks_skipped", "checks_disabled"):
                    unavailable.extend(
                        item.get("id")
                        for item in (run.get(field) or [])
                        if isinstance(item, dict) and isinstance(item.get("id"), str)
                    )
            unavailable = set(unavailable)
            if invalid_run:
                unavailable.update(before_by_check.keys() | after_by_check.keys())
            entered, left, unmeasured = [], [], []
            for check in sorted(after_by_check.keys() - before_by_check.keys()):
                if check in unavailable:
                    unmeasured.append(
                        {
                            "check": check,
                            "state": "not_measured",
                            "reason": "check coverage is incomplete",
                        }
                    )
                else:
                    entered.append(after_by_check[check])
            for check in sorted(before_by_check.keys() - after_by_check.keys()):
                if check in unavailable:
                    unmeasured.append(
                        {
                            "check": check,
                            "state": "not_measured",
                            "reason": "check coverage is incomplete",
                        }
                    )
                else:
                    left.append(before_by_check[check])
            finding_delta = {
                "state": "compared",
                "entered": entered,
                "left": left,
                "not_comparable": unmeasured,
                "unchanged": sorted(before_by_check.keys() & after_by_check.keys()),
            }

        body = {
            "state": "not_requested",
            "reason": "text diff requires native scan.v1 SQLite inputs",
        }
        if all(
            isinstance(value, (str, Path)) and Path(value).suffix.lower() in {".sqlite", ".db"}
            for value in (before, after)
        ):
            from seohead.storage.body_diff import body_diff

            try:
                body = body_diff(
                    before,
                    after,
                    url,
                    text=True,
                    max_bytes=max_bytes,
                    max_lines=max_lines,
                    left_representation=left_representation,
                    right_representation=right_representation,
                )
            except (OSError, ValueError, sqlite3.Error) as exc:
                body = {"state": "unavailable", "reason": str(exc)[:2048]}
        return {
            "ok": True,
            "format": "compare-url.v1",
            "url": url,
            "state": comparison_state,
            **({"reason": reason} if reason else {}),
            "before": cards[0],
            "after": cards[1],
            "changes": changes,
            "findings": finding_delta,
            "body_diff": body,
        }
    finally:
        for source in opened:
            source.close()


def verify_fixes(
    baseline: Any = None,
    finding_ids: list[str] | None = None,
    view: Any = None,
    urls: list[str] | None = None,
    urls_file: str | None = None,
    after: Any = None,
    config: str | None = None,
    out_dir: str | None = None,
) -> dict[str, Any]:
    """Recheck selected findings and save a separate, immutable verification.

    An existing ``after`` audit provides an offline classification path. Otherwise
    the selected baseline pages are fetched through the existing bounded crawler:
    list mode for static evidence, or one URL per run for a recorded JS policy.
    Results-affecting settings are replayed or checked before the first request.
    """
    import contextlib
    import json
    import os
    import tempfile
    from datetime import datetime, timezone
    from pathlib import Path

    from seohead.core.verification import (
        classify,
        compact_after_source,
        digest,
        markdown,
        offline_observation_gap,
        select_source,
    )
    from seohead.crawl import settings as crawl_settings
    from seohead.crawl.list_input import read_url_list
    from seohead.storage.inputs import load_audit_source

    if not out_dir:
        raise ValueError("out_dir required: a new directory for immutable verification evidence")
    if isinstance(view, (str, os.PathLike)):
        saved_view = json.loads(Path(view).read_text(encoding="utf-8"))
    else:
        saved_view = view
    requested_urls = list(urls or [])
    if urls_file:
        requested_urls.extend(read_url_list(urls_file))
    baseline_is_source = hasattr(baseline, "iter_collection")
    baseline_source = baseline if baseline_is_source else load_audit_source(baseline, "baseline")
    try:
        baseline_doc, selected, targets, baseline_identity = select_source(
            baseline_source, finding_ids=finding_ids, urls=requested_urls, view=saved_view
        )
    finally:
        if not baseline_is_source and hasattr(baseline_source, "close"):
            baseline_source.close()

    destination = Path(out_dir).absolute()
    if destination.exists():
        raise FileExistsError(f"verification output already exists: {destination}")
    destination.mkdir(parents=True)
    observations: dict[str, dict[str, Any]] = {}
    collection: dict[str, Any] = {"state": "offline" if after is not None else "not_run"}

    if after is not None:
        after_is_source = hasattr(after, "iter_collection")
        after_source = after if after_is_source else load_audit_source(after, "after")
        try:
            after_doc, after_identity = compact_after_source(after_source, selected)
        finally:
            if not after_is_source and hasattr(after_source, "close"):
                after_source.close()
        gap = offline_observation_gap(baseline_doc, after_doc)
        if gap is None:
            observations = dict.fromkeys(targets, after_doc)
        collection.update(
            state="offline" if gap is None else "not_verifiable",
            audit_sha256=after_identity["audit_sha256"],
            scan_uuid=after_identity["scan_uuid"],
            generated_at=after_identity["generated_at"],
        )
        if gap is not None:
            collection["reason"] = gap
    elif targets:
        recorded = (baseline_doc.get("run") or {}).get("crawl_config")
        if not isinstance(recorded, dict):
            collection["reason"] = "baseline has no recorded crawl configuration"
        elif (
            any(
                value == "REDACTED"
                for setting in ("http.headers", "http.credential_headers")
                for value in _verification_values(recorded.get(setting))
            )
            and not config
        ):
            collection["reason"] = (
                "baseline redacts authentication settings; provide the original config"
            )
        else:
            try:
                replay_overrides = dict(recorded)
                proxy = replay_overrides.get("http.proxy")
                if isinstance(proxy, dict):
                    if proxy != {"mode": "direct", "endpoint": None, "authenticated": False}:
                        if not config:
                            raise ValueError(
                                "baseline records a proxy; provide the original config"
                            )
                    else:
                        replay_overrides["http.proxy"] = ""
                settings = crawl_settings.load(
                    config, overrides=None if config else replay_overrides
                )
                measured = crawl_settings.manifest(settings)
                changed = sorted(
                    key
                    for key in set(recorded) | set(measured)
                    # The request and time budgets are derived from the page budget
                    # (bind_budgets_to_page_budget), so they are not policy to replay.
                    if key
                    not in {
                        "limits.max_urls",
                        "limits.max_depth",
                        "limits.max_requests",
                        "limits.max_crawl_seconds",
                    }
                    and recorded.get(key) != measured.get(key)
                )
                if settings["cache"]["mode"] != "off":
                    collection["reason"] = "fresh verification requires cache.mode=off"
                elif changed:
                    collection["reason"] = "recorded crawl policy differs: " + ", ".join(changed)
                else:
                    mode = settings["rendering"]["mode"]
                    common = {"config": config, "overrides": None if config else replay_overrides}
                    if mode == "raw":
                        folder = destination / "recrawl"
                        crawl_site(
                            urls=targets, out_dir=str(folder), max_urls=len(targets), **common
                        )
                        audit = json.loads((folder / "audit.json").read_text(encoding="utf-8"))
                        observations = dict.fromkeys(targets, audit)
                        collection = {
                            "state": "measured",
                            "mode": "list",
                            "audits": [{"path": "recrawl/audit.json", "sha256": digest(audit)}],
                        }
                    else:
                        audits = []
                        collection = {"state": "partial", "mode": mode, "audits": audits}
                        for index, target in enumerate(targets):
                            folder = destination / "recrawl" / f"url-{index + 1:04d}"
                            crawl_site(url=target, out_dir=str(folder), max_urls=1, **common)
                            audit = json.loads((folder / "audit.json").read_text(encoding="utf-8"))
                            observations[target] = audit
                            audits.append(
                                {
                                    "url": target,
                                    "path": str((folder / "audit.json").relative_to(destination)),
                                    "sha256": digest(audit),
                                }
                            )
                        collection["state"] = "measured"
            except (OSError, ValueError, RuntimeError) as exc:
                collection["state"] = "partial" if observations else "not_run"
                collection["reason"] = str(exc)

    findings = classify(
        baseline_doc,
        selected,
        observations,
        missing_reason=collection.get("reason") or "selected URL was not recrawled",
    )
    summary = {
        name: sum(item["status"] == name for item in findings)
        for name in ("resolved", "persisting", "changed", "not_verifiable")
    }
    document = {
        "schema_version": "verification.v1",
        "observed_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "baseline": {
            "audit_sha256": baseline_identity["audit_sha256"],
            "scan_uuid": baseline_identity["scan_uuid"],
            "generated_at": baseline_identity["generated_at"],
            "results_policy_fingerprint": baseline_identity["results_policy_fingerprint"],
        },
        "selection": {"finding_ids": [item.get("id") for item in selected], "urls": targets},
        "collection": collection,
        "summary": summary,
        "findings": findings,
    }

    def write_new(path: Path, data: str) -> None:
        fd, temporary = tempfile.mkstemp(prefix=".verification-", dir=destination)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as stream:
                stream.write(data)
                stream.flush()
                os.fsync(stream.fileno())
            os.link(temporary, path)
        finally:
            with contextlib.suppress(FileNotFoundError):
                os.unlink(temporary)

    write_new(destination / "verification.json", json.dumps(document, ensure_ascii=False, indent=2))
    write_new(destination / "verification.md", markdown(document))
    return {
        "ok": True,
        "verification": str(destination / "verification.json"),
        "report": str(destination / "verification.md"),
        **document,
    }


def _verification_values(value: Any) -> list[Any]:
    """Flatten recorded authentication metadata for redaction detection only."""
    if isinstance(value, dict):
        return [item for nested in value.values() for item in _verification_values(nested)]
    if isinstance(value, list):
        return [item for nested in value for item in _verification_values(nested)]
    return [value]


def crawl_import(manifest_path: str | None = None) -> dict[str, Any]:
    """Read an explicitly mapped third-party CSV crawl bundle offline.

    The result keeps source identity and field coverage under
    ``third_party_crawl.v1``. It is not a native scan or an SF Analyzer audit.
    """
    if not isinstance(manifest_path, str) or not manifest_path.strip():
        return {"ok": False, "error": "manifest_path must be a non-empty local path"}
    from seohead.crawl.external_import import (
        ExternalCrawlImportError,
        import_third_party_crawl,
    )

    try:
        return {"ok": True, **import_third_party_crawl(manifest_path)}
    except ExternalCrawlImportError as exc:
        return {"ok": False, "error": str(exc)}


def crawl_enrich(
    audit: Any = None,
    external_csv: str | None = None,
    url_column: str = "url",
    ignore_query: bool = False,
    ignore_scheme: bool = False,
    casefold_path: bool = False,
    out_urls: str | None = None,
    visits_column: str | None = None,
    bounce_column: str | None = None,
) -> dict[str, Any]:
    """Join an existing crawl's pages to an offline traffic/search CSV.

    This never changes the original audit. It returns each matched crawl page
    next to its external row, makes every non-match direction explicit, and
    can write reliable same-origin external-only URLs as a list-mode input.
    A partial crawl cannot prove an external-only URL is an orphan, so that
    list is refused rather than silently turning an incomplete population into
    an orphan claim. ``visits_column`` and ``bounce_column`` name CSV columns
    for the analytics findings; without them those findings are skipped.
    """
    if not external_csv:
        raise ValueError("external_csv required")
    from pathlib import Path

    from seohead.checks.analytics_findings import analytics_findings
    from seohead.checks.external_join import (
        join_external_data,
        load_csv_rows,
        normalize_join_key,
        orphan_urls,
    )

    diagnostics: list[dict[str, str]] = []
    document = _load_audit(audit, "audit", diagnostics)
    rows = load_csv_rows(external_csv, url_column=url_column)
    for column in (visits_column, bounce_column):
        if column and rows and column not in rows[0]:
            raise ValueError(f"column {column!r} not found in {external_csv!r}")

    def key(value: str | None) -> str | None:
        return normalize_join_key(
            value,
            ignore_query=ignore_query,
            ignore_scheme=ignore_scheme,
            casefold_path=casefold_path,
        )

    joined = join_external_data(
        document.get("pages") or [], rows, url_column=url_column, key_fn=key
    )
    partial = bool((document.get("run") or {}).get("crawl_partial"))
    candidates = [] if partial else orphan_urls(joined, url_column=url_column)
    reason = (
        "crawl is partial; external-only URLs are not labelled as orphans"
        if partial
        else "crawl completed; external-only same-origin URLs can enter list mode"
    )
    if out_urls:
        if partial:
            raise ValueError("cannot write an orphan list from a partial crawl")
        target = Path(out_urls)
        target.parent.mkdir(parents=True, exist_ok=True)
        atomic_write_bytes(target, "".join(f"{url}\n" for url in candidates).encode("utf-8"))
    result: dict[str, Any] = {
        "schema_version": "crawl_enrich.v1",
        "join": joined,
        "orphan_detection": {
            "state": "partial" if partial else "complete",
            "reason": reason,
            "urls": candidates,
        },
        "out_urls": out_urls,
        "analytics_findings": analytics_findings(
            document.get("pages") or [],
            joined,
            partial=partial,
            visits_column=visits_column,
            bounce_column=bounce_column,
        ),
    }
    if diagnostics:
        result["input_diagnostics"] = diagnostics
    return result


def segment_diff(
    audit: Any = None, source: str | None = None, target: str | None = None
) -> dict[str, Any]:
    """Cross-segment counterpart diff (#358): which pages of ``source`` have a
    counterpart in ``target``, and which do not.

    This is the one place ``seohead.crawl`` and ``seohead.sf`` meet for this
    feature: the audit's own recorded ``scope.segments`` / ``scope.segments_only``
    build a ``Scope`` here, and only its ``segment_for``/``rejection`` methods
    are handed to the pure analyzer in ``seohead.sf.core.segment_diff``, which
    never imports the crawl module itself.
    """
    if not source or not target:
        raise ValueError("source and target segment names required")
    from urllib.parse import urlsplit

    from seohead.crawl.spider import Scope
    from seohead.sf.core.segment_diff import diff_segments

    doc = _load_audit(audit, "audit")
    run = doc.get("run") or {}
    crawl_config = run.get("crawl_config") or {}
    segments_cfg = crawl_config.get("scope.segments") or []
    segments_only_cfg = crawl_config.get("scope.segments_only") or []
    scope = Scope.from_config({"segments": segments_cfg, "segments_only": segments_only_cfg})
    start_host = urlsplit(str(run.get("source") or "")).hostname or ""

    return diff_segments(
        doc.get("pages") or [],
        source=source,
        target=target,
        segments=segments_cfg,
        segment_for=scope.segment_for,
        rejection=(lambda u: scope.rejection(u, start_host)) if start_host else None,
        segments_only=set(segments_only_cfg),
        crawl_partial=bool(run.get("crawl_partial")),
    )


def render_check(
    url: str | None = None,
    viewport: str = "desktop",
    wait: str = "load",
    user_agent: str | None = None,
    transport_config: dict[str, str] | None = None,
) -> dict[str, Any]:
    if not url:
        raise ValueError("url required")
    from seohead.checks import render as render_core

    kwargs = {"transport_config": transport_config} if transport_config is not None else {}
    return render_core.render_check(
        url, viewport=viewport, wait=wait, user_agent=user_agent, **kwargs
    )


def backlinks_profile(
    domain: str,
    history_only: bool = False,
    imports: list[dict[str, Any]] | None = None,
    evidence: list[dict[str, Any]] | None = None,
    sources: list[dict[str, Any]] | None = None,
    db: str | None = None,
    project: str | None = None,
    observed_at: str | None = None,
    state: str | None = None,
    follow: str | None = None,
    target_prefix: str | None = None,
    min_spam_score: float | None = None,
    max_spam_score: float | None = None,
    first_seen_from: str | None = None,
    first_seen_to: str | None = None,
    one_per_domain: bool = False,
    target_status: dict[str, Any] | None = None,
    limit: int = 1000,
    offset: int = 0,
) -> dict[str, Any]:
    """Merge explicit backlink evidence and optionally retain a private history snapshot.

    Each sources entry selects one bounded provider operation with its own request/gates.
    No sources means offline analysis only; absence never proves a link was lost.
    """
    from seohead.recon.backlink_profile import build_backlink_profile, fetch_backlink_source
    from seohead.recon.net import normalize_domain

    normalized_domain = normalize_domain(domain)
    if not normalized_domain:
        raise ValueError("domain must be a valid hostname or HTTP(S) URL")
    if history_only:
        from seohead.data_sources.backlink_history import snapshot_history

        return snapshot_history(
            db=db, project=project, domain=normalized_domain, limit=limit, offset=offset
        )
    if db and project:
        raise ValueError("pass db or project, not both")
    for item in sources or []:
        if (
            not isinstance(item, dict)
            or item.get("source")
            not in {"dataforseo", "yandex_webmaster", "bing_webmaster", "ahrefs", "majestic", "moz"}
            or not isinstance(item.get("request", {}), dict)
        ):
            raise ValueError("sources entries require a supported source and an object request")
    collected = list(evidence or [])
    for item in sources or []:
        if not isinstance(item, dict) or not isinstance(item.get("request", {}), dict):
            raise ValueError("sources entries require source and an object request")
        source = item.get("source")
        request = dict(item.get("request") or {})
        if source == "dataforseo":
            from seohead.data_sources.dataforseo import backlink_call

            operation = item.get("operation", "backlinks")
            result = backlink_call(operation, normalized_domain, **request)
            collected.append(
                {
                    "source": source,
                    "result": result,
                    "complete": False,
                    "scope": "bounded index response",
                    "operation": operation,
                }
            )
        elif source in {"yandex_webmaster", "bing_webmaster"}:
            collected.append(fetch_backlink_source(source, domain, request))
        elif source in {"ahrefs", "majestic", "moz"}:
            from seohead.data_sources.backlink_indexes import collect

            collected.append(
                {
                    "source": source,
                    "result": collect(source, normalized_domain, **request),
                    "complete": False,
                }
            )
        else:
            raise ValueError("unsupported backlink source")
    return build_backlink_profile(
        domain,
        imports=imports,
        evidence=collected,
        db=db,
        project=project,
        observed_at=observed_at,
        state=state,
        follow=follow,
        target_prefix=target_prefix,
        min_spam_score=min_spam_score,
        max_spam_score=max_spam_score,
        first_seen_from=first_seen_from,
        first_seen_to=first_seen_to,
        one_per_domain=one_per_domain,
        target_status=target_status,
        limit=limit,
        offset=offset,
    )


def backlinks_disavow(
    domains: list[str] | None = None,
    urls: list[str] | None = None,
    output: str | None = None,
) -> dict[str, Any]:
    """Export only the specialist's explicit selection; never submit to search engines."""
    from seohead.recon.backlink_profile import export_disavow

    return export_disavow(domains=domains, urls=urls, output=output)


def backlinks_check(
    target: str | None = None, donors: list[str] | None = None, concurrency: int = 3
) -> dict[str, Any]:
    if not target:
        raise ValueError("target required")
    if not donors:
        raise ValueError("donors[] required")
    from seohead.recon import backlinks as backlinks_core

    return backlinks_core.check_backlinks(target, donors, concurrency=concurrency)


def duplicate_check(
    items: list[dict] | None = None,
    threshold: float = 0.92,
    with_fingerprints: bool = False,
    only_indexable: bool = True,
    scan: str | None = None,
) -> dict[str, Any]:
    if items is not None and scan is not None:
        raise ValueError("items[] and scan are mutually exclusive")
    from seohead.checks import duplicate as dup_core

    if scan is not None:
        from seohead.checks.duplicate import DuplicateBudgetExceeded
        from seohead.storage.corpus_inputs import (
            MAX_DUPLICATE_CANDIDATE_COMPARISONS,
            MAX_DUPLICATE_SHINGLES,
            corpus_public,
            scan_corpus,
        )

        corpus = scan_corpus(scan, kind="duplicate")
        if corpus["coverage"]["state"] == "unavailable":
            return {"ok": False, **corpus_public(corpus)}
        try:
            result = dup_core.find_duplicates(
                corpus["items"],
                threshold=threshold,
                with_fingerprints=with_fingerprints,
                only_indexable=only_indexable,
                max_shingles=MAX_DUPLICATE_SHINGLES,
                max_candidate_comparisons=MAX_DUPLICATE_CANDIDATE_COMPARISONS,
            )
        except DuplicateBudgetExceeded as exc:
            return {"ok": False, "reason": str(exc), **corpus_public(corpus, unavailable=str(exc))}
        return {**result, **corpus_public(corpus, analyzed=result["count"])}
    if not items:
        raise ValueError("items[] required (list of {id, text})")
    return dup_core.find_duplicates(
        items,
        threshold=threshold,
        with_fingerprints=with_fingerprints,
        only_indexable=only_indexable,
    )


def mirror_check(url: str | None = None, timeout: float = 12.0) -> dict[str, Any]:
    """Verify canonical host consolidation across scheme, ``www``, index-file, case, and slash variants."""
    if not url:
        raise ValueError("url required")
    from seohead.recon import mirrors as mirrors_core

    return mirrors_core.check_mirrors(url, timeout=timeout)


def ai_bots_check(url: str | None = None, robots_text: str | None = None) -> dict[str, Any]:
    """Evaluate AI-crawler access from supplied robots.txt content or a site URL."""
    if not url and not robots_text:
        raise ValueError("url or robots_text required")
    from seohead.recon import ai_bots as ai_bots_core

    if robots_text is None:
        from seohead.recon.net import http_client, normalize_url

        target = normalize_url(url or "")
        if not target:
            return {"ok": False, "error": f"not a recognizable URL: {url!r}"}
        from urllib.parse import urlsplit, urlunsplit

        parts = urlsplit(target)
        robots_url = urlunsplit((parts.scheme, parts.netloc, "/robots.txt", "", ""))
        try:
            client, _ = http_client(20.0)
            with client:
                resp = client.get(robots_url)
        except Exception as exc:  # Tool boundary: network failures are result data, not crashes.
            return {"ok": False, "url": robots_url, "error": str(exc)}
        code = resp.status_code
        if code >= 500:
            # A server error is "we could not read the rules", not "there are no
            # rules" — #135 established the same distinction for the native
            # crawler's own robots fetch. Reporting every AI bot allowed here
            # would be a false permission grant on evidence that never loaded.
            return {
                "ok": False,
                "url": url,
                "robots_url": robots_url,
                "status_code": code,
                "error": f"robots.txt returned {code}; rules could not be read",
            }
        # A 4xx robots.txt means "no restrictions" per RFC 9309, same as
        # tools.robots.check_robots; the response body (an error page, not
        # rules) is discarded rather than handed to the parser.
        robots_text = resp.text if code < 400 else ""
        fetched = {"robots_url": robots_url, "status_code": code}
    else:
        fetched = {}
    result = ai_bots_core.check_ai_access(robots_text)
    return {**result, "url": url, **fetched}


def llms_txt_check(url: str | None = None, brand: str | None = None) -> dict[str, Any]:
    if not url:
        raise ValueError("url required")
    from seohead.checks import llms_txt as llms_core

    return llms_core.check_llms_txt(url, brand=brand)


def _require_fetched_html(url: str) -> dict[str, Any]:
    """Fetch ``url`` and tighten ``fetch_html``'s transport-only ``ok`` to a 2xx status.

    Matches the success contract every other URL-fetching handler here already
    uses (``parse_url``'s ``ok``): a transport failure or a non-2xx response
    both come back as ``ok: False``, so a caller can check one flag either way.
    """
    fetched = parser.fetch_html(url)
    if fetched["ok"]:
        fetched["ok"] = 200 <= fetched["status_code"] < 300
    return fetched


def citability_check(
    url: str | None = None, text: str | None = None, content_area: dict[str, Any] | None = None
) -> dict[str, Any]:
    """Score whether content is self-contained and evidence-rich enough to support a cited AI answer.

    Scored over the resolved content area when fetched from a ``url``: nav and
    footer boilerplate would otherwise dilute statistical density, and — more
    importantly — the parser's whole-document ``text`` field has no paragraph
    or heading breaks at all (it is a single collapsed line), which silently
    zeroes the Answer-Blocks and Structure-Quality dimensions for every live
    page. ``markdown_extract``'s content-area Markdown keeps both the
    boilerplate exclusion and the structure the scorer depends on. Passing
    ``text`` directly is unaffected: the caller chose exactly what to score.
    """
    if not url and not text:
        raise ValueError("url or text required")
    from seohead.checks import citability as cit_core

    if text is not None:
        return cit_core.score_citability(text)
    fetched = _require_fetched_html(url)
    if not fetched["ok"]:
        return {"ok": False, "url": url, "error": fetched.get("error", "fetch failed")}
    from seohead.checks import markdown_extract as md_core

    content_markdown = md_core.extract_markdown(fetched["html"], content_area)["content_markdown"]
    return {"url": url, **cit_core.score_citability(content_markdown)}


def markdown_extract(
    url: str | None = None, html: str | None = None, content_area: dict[str, Any] | None = None
) -> dict[str, Any]:
    """Render a page as Markdown in two scopes: content-area only, and full document.

    Pass ``html`` to render offline, or ``url`` to fetch it first. The
    content-area rendering is what is worth diffing between crawls, scoring,
    or handing to a model; the full-document rendering (header and footer
    included) is not what ``boilerplate_report`` takes: that check needs the
    original HTML (or a precomputed hash) to see whether boilerplate is
    consistent across a crawl.
    """
    if not url and not html:
        raise ValueError("url or html required")
    from seohead.checks import markdown_extract as md_core

    if html is not None:
        return {"ok": True, **md_core.extract_markdown(html, content_area)}
    fetched = _require_fetched_html(url)
    if not fetched["ok"]:
        return {"ok": False, "url": url, "error": fetched.get("error", "fetch failed")}
    return {
        "ok": True,
        "url": url,
        "final_url": fetched["final_url"],
        "status_code": fetched["status_code"],
        **md_core.extract_markdown(fetched["html"], content_area),
    }


def log_scan(
    run: str | None = None, images_dir: str | None = None, max_per_rule: int = 20
) -> dict[str, Any]:
    """Report claims a finished run makes that cannot all be true at once.

    ``run`` is a retained native scan path, or a directory holding ``audit.json``, ``pages.jsonl`` and/or
    ``decisions.jsonl`` — whatever ``crawl-site --out-dir`` or ``sf run --out`` wrote.
    ``decisions.jsonl`` (issue #134) is the per-URL exclusion log a native crawl writes
    beside ``pages.jsonl``; it lets a rule catch a contradiction that never survives into
    ``audit.json`` at all, not only one visible in the finished output. ``images_dir`` is an
    ``images-download`` output directory, whose manifest lets a recorded size be compared
    against the file itself.

    This is not a second audit. It reports only pairs of facts from the same run that
    contradict each other, each with both values and where they came from, so a surprising
    number can be traced instead of trusted. See ``seohead.checks.logscan`` for the rules and
    for the defects each one was written from.
    """
    from seohead.checks import logscan

    if not run:
        raise ValueError("log_scan needs a run directory or native scan path")
    with logscan.load_run(run, images_dir) as artifacts:
        if artifacts.audit is None and not artifacts.pages:
            return {
                "ok": False,
                "error": f"no audit.json or pages.jsonl or retained native evidence in {run}",
                "anomalies": [],
                "anomaly_count": 0,
            }
        return logscan.scan(artifacts, max_per_rule=max_per_rule)


def crawl_diagnose(
    scan: str | None = None,
    run: str | None = None,
    max_decisions: int = 20,
) -> dict[str, Any]:
    """Explain a native crawl from retained evidence without fetching the site."""
    from seohead.crawl.diagnostics import diagnose

    return diagnose(scan=scan, run=run, max_decisions=max_decisions)


def crawl_diagnose_export(
    scan: str | None = None,
    run: str | None = None,
    export: str | None = None,
    max_decisions: int = 20,
) -> dict[str, Any]:
    """Write a new redacted diagnostic file only when its path was explicit."""
    if not export:
        raise ValueError("export path is required")
    from seohead.crawl.diagnostics import diagnose

    return diagnose(scan=scan, run=run, max_decisions=max_decisions, export=export)


def boilerplate_report(pages: list[dict] | None = None, scan: str | None = None) -> dict[str, Any]:
    """Group a crawled corpus by header/nav/footer hash and report minority template groups.

    Each page is ``{"url": str, "html": str}`` or, when the hash was already
    computed upstream (``boilerplate_report.boilerplate_hash``), ``{"url": str,
    "hash": str}``.
    """
    if pages is not None and scan is not None:
        raise ValueError("pages[] and scan are mutually exclusive")
    if scan is not None:
        from seohead.storage.corpus_inputs import corpus_public, scan_corpus

        corpus = scan_corpus(scan, kind="boilerplate")
        if corpus["coverage"]["state"] == "unavailable":
            return {"ok": False, **corpus_public(corpus)}
        from seohead.checks import boilerplate_report as bp_core

        result = bp_core.boilerplate_consistency_report(corpus["items"])
        return {**result, **corpus_public(corpus, analyzed=result["count"])}
    if not pages:
        raise ValueError("pages[] required (list of {url, html} or {url, hash})")
    from seohead.checks import boilerplate_report as bp_core

    return bp_core.boilerplate_consistency_report(pages)


def semantic_inputs(
    items: list[dict] | None = None,
    scan: str | None = None,
    content_area: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Build the reproducible normalized-input manifest for semantic analysis.

    Each document's entry names the retained body hash, the exact decoded
    input hash, the normalized output hash, the content-area strategy, and the
    language evidence — the normalized text itself stays inside the corpus
    boundary for the analyzer that consumes it (issue #801).
    """
    if items is not None and scan is not None:
        raise ValueError("items[] and scan are mutually exclusive")
    from seohead.checks import text_normalize as norm_core

    if scan is not None:
        if content_area is not None:
            raise ValueError("scan input uses the crawl's recorded content_area config")
        from seohead.storage.corpus_inputs import corpus_public, scan_corpus

        corpus = scan_corpus(scan, kind="semantic")
        if corpus["coverage"]["state"] == "unavailable":
            return {"ok": False, **corpus_public(corpus)}
        documents = [norm_core.manifest_entry(item) for item in corpus["items"]]
        return {
            "ok": True,
            "count": len(documents),
            "documents": documents,
            **corpus_public(corpus, analyzed=len(documents)),
        }
    if not items:
        raise ValueError("items[] required (list of {url, html})")
    prepared = norm_core.prepare_items(items, content_area=content_area)
    documents = [norm_core.manifest_entry(item) for item in prepared]
    unavailable = sum(1 for document in documents if document["state"] == "unavailable")
    return {
        "ok": True,
        "count": len(documents),
        "documents": documents,
        "normalization": norm_core.normalization_policy(content_area),
        "coverage": {
            "state": "partial" if unavailable else "complete",
            "reason": "items without a usable html body are unavailable" if unavailable else "",
            "eligible_documents": len(items),
            "prepared_documents": len(documents) - unavailable,
            "analyzed_documents": len(documents) - unavailable,
            "measured_empty_documents": sum(
                1 for document in documents if document["state"] == "empty"
            ),
            "omitted_documents": unavailable,
            "omission_reasons": (
                {"item supplies no html body": unavailable} if unavailable else {}
            ),
        },
    }


def semantic_similarity(
    items: list[dict] | None = None,
    scan: str | None = None,
    embeddings: list[dict] | None = None,
    adapter: dict[str, Any] | None = None,
    cache_path: str | None = None,
    threshold: float = 0.82,
    max_candidate_comparisons: int = 250_000,
) -> dict[str, Any]:
    """Group supplied embedding evidence; this handler never loads or calls a model."""
    if (items is None) == (scan is None):
        raise ValueError("provide exactly one of items[] or scan")
    if not isinstance(adapter, dict):
        raise ValueError("adapter declaration is required")
    if not cache_path:
        raise ValueError("cache_path is required for local semantic embedding reuse")
    if not isinstance(embeddings, list):
        raise ValueError("embeddings[] is required; SEOHEAD does not call a model by default")
    from seohead.checks import semantic_similarity as core
    from seohead.checks import text_normalize as norm_core

    if scan is not None:
        from seohead.storage.corpus_inputs import corpus_public, scan_corpus

        corpus = scan_corpus(scan, kind="semantic")
        if corpus["coverage"]["state"] == "unavailable":
            return {"ok": False, "groups": [], **corpus_public(corpus)}
        documents = corpus["items"]
        public = corpus_public(corpus)
    else:
        assert items is not None
        documents = norm_core.prepare_items(items)
        public = {
            "normalization": norm_core.normalization_policy(),
            "coverage": {
                "state": "complete",
                "eligible_documents": len(items),
                "prepared_documents": len(documents),
                "analyzed_documents": 0,
                "omitted_documents": 0,
                "omission_reasons": {},
            },
        }
    vectors = {
        entry.get("url"): entry.get("vector")
        for entry in embeddings
        if isinstance(entry, dict) and isinstance(entry.get("url"), str)
    }
    for document in documents:
        if document.get("url") in vectors:
            document["embedding"] = vectors[document["url"]]
    result = core.analyze_semantic_documents(
        documents,
        core.DeclaredEmbeddingAdapter(adapter),
        core.EmbeddingCache(cache_path),
        threshold=threshold,
        max_candidate_comparisons=max_candidate_comparisons,
    )
    if "coverage" in public:
        result["source_coverage"] = public["coverage"]
    if "source" in public:
        result["source"] = public["source"]
    if "normalization" in public:
        result["normalization"] = public["normalization"]
    return result


def meta_description_drafts(
    items: list[dict] | None = None,
    scan: str | None = None,
    context: dict[str, Any] | None = None,
    drafts: list[dict] | None = None,
    executor: dict[str, Any] | None = None,
    checkpoint_path: str | None = None,
    batch_size: int = 20,
    json_path: str | None = None,
    csv_path: str | None = None,
) -> dict[str, Any]:
    """Prepare or validate resumable, page-grounded supplied drafts without model calls."""
    if (items is None) == (scan is None):
        raise ValueError("provide exactly one of items[] or scan")
    from seohead.checks import meta_description_drafts as core

    if scan is not None:
        from seohead.storage.corpus_inputs import corpus_public, scan_corpus

        corpus = scan_corpus(scan, kind="semantic")
        if corpus["coverage"]["state"] == "unavailable":
            return {"ok": False, **corpus_public(corpus)}
        plan = core.prepare_draft_plan_from_normalized(
            corpus["items"], context, batch_size=batch_size
        )
        public = corpus_public(corpus)
    else:
        assert items is not None
        plan = core.prepare_draft_plan(items, context, batch_size=batch_size)
        public = {}
    if drafts is None:
        return {"ok": True, "plan": plan, **public}
    if not isinstance(executor, dict) or not checkpoint_path:
        raise ValueError("draft execution requires executor declaration and checkpoint_path")
    if (json_path is None) != (csv_path is None):
        raise ValueError("json_path and csv_path must be supplied together")
    result = core.run_draft_plan(
        plan,
        core.DeclaredDraftExecutor(executor, drafts),
        core.DraftCheckpoint(checkpoint_path),
    )
    if json_path and csv_path:
        core.export_draft_review(result, json_path, csv_path)
    return {"ok": True, "plan_coverage": plan["coverage"], "result": result, **public}


def ai_column(
    items: list[dict] | None = None,
    scan: str | None = None,
    prompt: str = "",
    urls: list[str] | None = None,
    column: str = "ai_column",
    max_pages: int = 100,
    rows: list[dict] | None = None,
    csv_path: str | None = None,
) -> dict[str, Any]:
    """Plan a per-URL AI column over retained page evidence, then validate caller-supplied values."""
    if (items is None) == (scan is None):
        raise ValueError("provide exactly one of items[] or scan")
    from seohead.checks import ai_column as core

    if scan is not None:
        from seohead.storage.corpus_inputs import corpus_public, scan_corpus

        corpus = scan_corpus(scan, kind="semantic")
        if corpus["coverage"]["state"] == "unavailable":
            return {"ok": False, **corpus_public(corpus)}
        source_items = corpus["items"]
        public = corpus_public(corpus)
    else:
        assert items is not None
        source_items = items
        public = {}
    plan = core.prepare_ai_column_plan(
        source_items, prompt, urls=urls, column=column, max_pages=max_pages
    )
    if rows is None:
        return {"ok": True, "plan": plan, **public}
    result = core.apply_ai_column_results(plan, rows)
    if csv_path:
        core.export_ai_column_csv(result, csv_path)
    return {"ok": True, "plan_coverage": plan["coverage"], "result": result, **public}


def social_meta_check(
    url: str | None = None, og: dict[str, str] | None = None, twitter: dict[str, str] | None = None
) -> dict[str, Any]:
    """Identify missing Open Graph and Twitter Card fields required for a stable link preview."""
    if not url and og is None and twitter is None:
        raise ValueError("url or og/twitter required")
    from seohead.checks import social_meta as sm_core

    if og is None and twitter is None:
        from seohead.checks import parser as _parser

        page = _parser.parse_url(
            url,
            {
                "meta": False,
                "canonical": False,
                "og": True,
                "headings": False,
                "jsonld": False,
                "links": False,
                "text": False,
            },
        )
        if not page.get("ok"):
            return {"ok": False, "url": url, "error": page.get("error", "parse failed")}
        og, twitter = page.get("og") or {}, page.get("twitter") or {}
        fetched = {"url": url}
    else:
        fetched = {}
    return {**sm_core.check_social_meta(og=og, twitter=twitter), **fetched}


def soft404_check(url: str | None = None) -> dict[str, Any]:
    """Probe two deterministic nonexistent URLs to distinguish honest 404s from soft-404 responses."""
    if not url:
        raise ValueError("url required")
    from seohead.checks import soft404 as s4_core

    return s4_core.check_soft404(url)


# Registry consumed by the CLI and MCP server: one source of truth for public behavior.

# --- External data providers: demand, search results, and spend -------------------------


def keywords_expand(
    phrase: str | None = None, limit: int = 300, regions: list[str] | None = None
) -> dict[str, Any]:
    """Expand a seed phrase with Yandex Wordstat refinements and related queries.

    Returned frequency is **base frequency**, not exact frequency: the API does not expose
    ``!``, ``+``, or ``[]`` operators, and base counts are typically about nine times higher than
    exact counts. They are suitable for initial filtering; use Arsenkin for exact ``!W`` values.
    A multi-region request sums demand across its regions, so request regions separately when
    regional comparison matters. This method is paid and subject to Wordstat's hourly quota.
    """
    if not phrase:
        raise ValueError("phrase required")
    from seohead.data_sources.credentials import MissingCredential
    from seohead.data_sources.yandex_cloud import Wordstat

    try:
        pool, meta = Wordstat().expand(phrase, limit=limit, regions=tuple(regions or ["225"]))
    except MissingCredential as exc:
        return {"ok": False, "error": str(exc)}
    except RuntimeError as exc:
        return {"ok": False, "error": str(exc)}
    ranked = sorted(pool.items(), key=lambda kv: -kv[1])
    return {
        "ok": True,
        "phrase": phrase,
        "found": len(ranked),
        "total_count": meta.get("totalCount"),
        "from_results": meta.get("results"),
        "from_associations": meta.get("associations"),
        "keywords": [
            {"phrase": p, "base_frequency": c, "origin": meta["origin"].get(p)} for p, c in ranked
        ],
    }


def keywords_seasonality(
    phrase: str | None = None,
    from_date: str | None = None,
    to_date: str | None = None,
    period: str = "PERIOD_MONTHLY",
    regions: list[str] | None = None,
) -> dict[str, Any]:
    """Return Yandex Wordstat demand dynamics; dates use RFC3339, e.g. ``2026-01-01T00:00:00Z``."""
    if not phrase or not from_date or not to_date:
        raise ValueError("phrase, from_date and to_date required")
    from seohead.data_sources.credentials import MissingCredential
    from seohead.data_sources.yandex_cloud import Wordstat

    try:
        body = Wordstat().dynamics(
            phrase, from_date, to_date, period=period, regions=tuple(regions or ["225"])
        )
    except MissingCredential as exc:
        return {"ok": False, "error": str(exc)}
    except RuntimeError as exc:
        return {"ok": False, "error": str(exc)}
    return {"ok": True, "phrase": phrase, "period": period, "dynamics": body}


def serp_fetch(
    query: str | None = None, queries: list[str] | None = None, region: str = "225", top: int = 10
) -> dict[str, Any]:
    """Fetch Yandex results for one query or a batch through the asynchronous API only.

    The synchronous endpoint is deliberately excluded because it costs roughly sixteen times more.
    Batch operations are launched together and polled as a group, so N queries take approximately
    one batch duration rather than N sequential request durations. An exact duplicate query is
    billed once, since a second submission of identical text could never surface a second result.
    Submitted operations are billed even if polling times out; their operation records remain in
    the local spend journal. A submission the provider rejected outright is never billed and
    reaches the caller as an explicit error, distinct from a billed operation that timed out.
    """
    targets = [q for q in ([query] if query else []) + list(queries or []) if q]
    if not targets:
        raise ValueError("query or queries required")
    unique_targets = list(dict.fromkeys(targets))
    from seohead.data_sources.credentials import MissingCredential
    from seohead.data_sources.yandex_cloud import WebSearch

    try:
        client = WebSearch()
        raw = client.search_batch(unique_targets, region=region, groups=top)
    except MissingCredential as exc:
        return {"ok": False, "error": str(exc)}
    except RuntimeError as exc:
        return {"ok": False, "error": str(exc)}
    results = {}
    for q, v in raw.items():
        entry = {"docs": v.get("docs", []), "error": v.get("error"), "status": v.get("status")}
        if v.get("operation_id") is not None:
            entry["operation_id"] = v["operation_id"]
        if v.get("http_status") is not None:
            entry["http_status"] = v["http_status"]
        results[q] = entry
    # A query is only ever absent here because its operation was billed and never finished
    # before the timeout: search_batch already gives every rejected or lost submission an
    # explicit entry above, so this is never a stand-in for "the provider said no".
    missing = [q for q in unique_targets if q not in results]
    return {
        "ok": True,
        "region": region,
        "requested": len(unique_targets),
        "returned": len(results),
        "results": results,
        "not_returned": missing,
        "note": (
            "queries not returned before timeout were already billed; their operations "
            "are recorded in the spend journal"
            if missing
            else None
        ),
    }


def keywords_exact(
    keywords: list[str] | None = None, region: int = 225, wait: bool = True
) -> dict[str, Any]:
    """Fetch exact ``!W`` frequency through Arsenkin, which Wordstat's API does not expose.

    This operation is paid and consumes account limits. The charge and ``task_id`` are journaled as
    soon as the task is created, allowing a result whose polling or parsing failed to be retrieved
    later without paying for a duplicate task.

    The provider tool is ``wordstat`` with ``type=1``; there is no ``keywords_frequency`` tool, and
    asking for one answers ``404 WRONG_TOOL`` without billing. ``frequencies`` holds the flattened
    ``{phrase: {"base": N, "overal": N}}`` mapping, where ``overal`` is the provider's field for
    the exact ``!W`` figure (``!WS``); ``quoted`` is the ``"WS"`` phrase operator and ``exact`` is
    the ``[!WS]`` strict-order operator, so neither is reported as ``!W``. ``result`` keeps the
    raw provider payload, ``warnings`` lists phrases with no data for the requested region or rows
    the parser skipped, and ``cleaned`` lists phrases whose punctuation had to be stripped before
    sending, because the provider rejects the whole batch otherwise.
    """
    if not keywords:
        raise ValueError("keywords required")
    from seohead.data_sources.arsenkin import (
        WORDSTAT_TOOL,
        ArsenkinClient,
        ArsenkinError,
        parse_wordstat,
        sanitize_wordstat_query,
        wordstat_payload,
    )
    from seohead.data_sources.credentials import MissingCredential

    try:
        try:
            region_id = int(region)
        except (TypeError, ValueError):
            return {"ok": False, "error": f"invalid region {region!r}", "code": "INVALID_REGION"}
        # Report every phrase the provider would have rejected, so a caller comparing
        # frequencies against its own list can see which ones were measured differently.
        cleaned = {
            original: sanitize_wordstat_query(original)
            for original in keywords
            if sanitize_wordstat_query(original) != str(original)
        }
        try:
            payload = wordstat_payload(list(keywords), region_id)
        except ValueError as exc:
            return {"ok": False, "error": str(exc), "code": "EMPTY_QUERIES", "cleaned": cleaned}
        client = ArsenkinClient()
        task = client.set_task(WORDSTAT_TOOL, payload)
        if not wait:
            return {
                "ok": True,
                "task_id": task["task_id"],
                "cost": task["cost"],
                "region": region_id,
                "cleaned": cleaned,
                "note": "task created and billed; retrieve the result later by task_id",
            }
        result = client.wait(task["task_id"])
        payload_result = result.get("result", result)
        parsed = parse_wordstat(payload_result, region_id)
        response: dict[str, Any] = {
            "ok": True,
            "task_id": task["task_id"],
            "cost": task["cost"],
            "region": region_id,
            "cleaned": cleaned,
            "frequencies": parsed["frequencies"],
            "result": payload_result,
        }
        if parsed["warnings"]:
            response["warnings"] = parsed["warnings"]
        return response
    except MissingCredential as exc:
        return {"ok": False, "error": str(exc)}
    except ArsenkinError as exc:
        error: dict[str, Any] = {"ok": False, "error": str(exc), "code": exc.code}
        # `task` exists only once `set_task` has already succeeded (and billed) --
        # that identifier must survive into a subsequent `wait()` failure, since
        # parsing it back out of the free-text error string is exactly the recovery
        # route this handler's own docstring documents. When `set_task` itself is
        # what raised, nothing was billed, so no task_id/cost should be fabricated.
        if "task" in locals():
            error["task_id"] = task["task_id"]
            error["cost"] = task["cost"]
        return error


def dataforseo_estimate(
    operation: str,
    query_count: int = 1,
    row_limit: int = 0,
    cache_hits: int = 0,
    prices: dict[str, Any] | None = None,
    env: str | None = None,
    refresh_prices: bool = False,
    query: str | None = None,
) -> dict[str, Any]:
    """Quote supplied prices offline; refresh_prices explicitly reads free account prices."""
    from seohead.data_sources.dataforseo import estimate

    if refresh_prices:
        from seohead.data_sources.dataforseo import (
            DataForSEOClient,
            DataForSEOError,
            MissingCredential,
        )

        try:
            prices = DataForSEOClient(env=env).balance().get("prices")
        except (MissingCredential, DataForSEOError, OSError, ValueError) as exc:
            return {
                "ok": False,
                "state": "unavailable",
                "reason": "free account price lookup failed",
                "error": str(exc),
            }

    quote = estimate(
        operation,
        query_count=query_count,
        row_limit=row_limit,
        cache_hits=cache_hits,
        prices=prices,
        env=env,
        query=query,
    )
    return {"ok": quote["state"] == "complete", **quote}


def google_keywords(
    keywords: list[str] | None = None,
    seed: str | None = None,
    location_code: int = 2840,
    language: str = "en",
    country: str | None = None,
    limit: int = 100,
    difficulty: bool = False,
    confirm_paid: bool = False,
    env: str | None = None,
    spend_ceiling_usd: float | None = None,
    prices: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Query Google demand through DataForSEO by keyword list or seed phrase.

    ``keywords`` returns search volume and competition for an existing list. ``seed`` expands one
    phrase into keyword ideas, analogous to Wordstat refinements but for Google. Set
    ``difficulty=true`` to return keyword difficulty instead of search volume.

    DataForSEO does not support locations in Russia or Belarus. The geographic
    guard blocks such requests before they reach the paid provider and directs callers to Wordstat
    or Arsenkin. The default ``sandbox`` environment returns realistic response shapes with fake
    data and incurs no charge; production requires explicit provider configuration.
    """
    from seohead.data_sources import dataforseo as core

    if seed and difficulty and spend_ceiling_usd is not None:
        quotes = [
            core.estimate(operation, row_limit=limit, prices=prices, env=env)
            for operation in ("keyword_ideas", "keyword_difficulty")
        ]
        if any(quote["cost_usd"] is None for quote in quotes):
            return {
                "ok": False,
                "state": "unavailable",
                "reason": "both operation prices are required to enforce the combined ceiling",
                "estimate": quotes,
            }
        if sum(quote["cost_usd"] for quote in quotes) > spend_ceiling_usd:
            return {
                "ok": False,
                "state": "spend_limit_exceeded",
                "reason": "combined expansion and difficulty estimate exceeds ceiling before request",
                "estimate": quotes,
            }
    if seed:
        ideas = core.keyword_ideas(
            seed,
            location_code=location_code,
            language=language,
            limit=limit,
            country=country,
            confirm_paid=confirm_paid,
            env=env,
            spend_ceiling_usd=spend_ceiling_usd,
            prices=prices,
        )
        if not difficulty or not ideas.get("ok"):
            return ideas
        # `difficulty=True` is a documented, independent option -- it must not be
        # silently dropped just because `seed` also routed through the ideas path.
        expanded = [k.get("phrase") for k in ideas.get("keywords") or [] if k.get("phrase")]
        if not expanded:
            return ideas
        remaining_ceiling = spend_ceiling_usd
        if spend_ceiling_usd is not None:
            actual = ideas.get("cost_usd")
            if actual is None:
                return {
                    **ideas,
                    "ok": False,
                    "state": "partial",
                    "reason": "expansion cost is unknown; cannot enforce remaining ceiling for difficulty",
                }
            remaining_ceiling = max(0.0, spend_ceiling_usd - actual)
        scored = core.keyword_difficulty(
            expanded,
            location_code=location_code,
            language=language,
            country=country,
            confirm_paid=confirm_paid,
            env=env,
            spend_ceiling_usd=remaining_ceiling,
            prices=prices,
        )
        ideas["difficulty"] = scored
        if scored.get("ok") is False:
            ideas["ok"] = False
        return ideas
    if not keywords:
        raise ValueError("keywords or seed required")
    if difficulty:
        return core.keyword_difficulty(
            keywords,
            location_code=location_code,
            language=language,
            country=country,
            confirm_paid=confirm_paid,
            env=env,
            spend_ceiling_usd=spend_ceiling_usd,
            prices=prices,
        )
    return core.search_volume(
        keywords,
        location_code=location_code,
        language=language,
        country=country,
        confirm_paid=confirm_paid,
        env=env,
        spend_ceiling_usd=spend_ceiling_usd,
        prices=prices,
    )


def google_serp(
    query: str | None = None,
    location_code: int = 2840,
    language: str = "en",
    depth: int = 10,
    country: str | None = None,
    confirm_paid: bool = False,
    env: str | None = None,
    spend_ceiling_usd: float | None = None,
    prices: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Return the Google organic results that actually rank for a query in the selected market."""
    if not query:
        raise ValueError("query required")
    from seohead.data_sources import dataforseo as core

    return core.serp(
        query,
        location_code=location_code,
        language=language,
        depth=depth,
        country=country,
        confirm_paid=confirm_paid,
        env=env,
        spend_ceiling_usd=spend_ceiling_usd,
        prices=prices,
    )


def topvisor_read(
    operation: str = "projects", params: dict[str, Any] | None = None
) -> dict[str, Any]:
    """Read one bounded page of existing Topvisor data without launching paid checks."""
    from seohead.data_sources.credentials import MissingCredential
    from seohead.data_sources.topvisor import TopvisorError, fetch

    try:
        return fetch(operation, params)
    except (MissingCredential, TopvisorError, ValueError) as exc:
        return {"ok": False, "error": str(exc)}


def metrika_counters() -> dict[str, Any]:
    """List Metrika counters visible to the token and expose the ``counter_id`` required by reports."""
    from seohead.data_sources.credentials import MissingCredential
    from seohead.data_sources.metrika import MetrikaClient, MetrikaError

    try:
        counters = MetrikaClient().counters()
    except MissingCredential as exc:
        return {"ok": False, "error": str(exc)}
    except MetrikaError as exc:
        return {"ok": False, "error": exc.message, "status": exc.status}
    return {
        "ok": True,
        "count": len(counters),
        "counters": [
            {
                "id": c.get("id"),
                "name": c.get("name"),
                "site": c.get("site"),
                "status": c.get("status"),
            }
            for c in counters
        ],
    }


def metrika_setup(counter_id: str | None = None) -> dict[str, Any]:
    """Inspect a Metrika counter's goals, filters, and data-processing operations.

    Run this before drawing conclusions from traffic. Counter operations can silently reshape
    reports—for example by removing URL parameters. If no goals are configured, the dataset cannot
    contain conversions; reporting a "zero conversion rate" would describe instrumentation, not
    observed user behavior.
    """
    if not counter_id:
        raise ValueError("counter_id required")
    from seohead.data_sources.credentials import MissingCredential
    from seohead.data_sources.metrika import MetrikaClient, MetrikaError

    try:
        client = MetrikaClient()
        goals = client.goals(counter_id)
        filters = client.filters(counter_id)
        operations = client.operations(counter_id)
    except MissingCredential as exc:
        return {"ok": False, "error": str(exc)}
    except MetrikaError as exc:
        return {"ok": False, "error": exc.message, "status": exc.status}
    return {
        "ok": True,
        "counter_id": counter_id,
        "goals": [{"id": g.get("id"), "name": g.get("name"), "type": g.get("type")} for g in goals],
        "goals_count": len(goals),
        "filters": (filters or {}).get("filters", []),
        "operations": (operations or {}).get("operations", []),
        "note": (
            "no goals are configured, so conversions cannot appear in the data; "
            "a zero conversion rate would reflect instrumentation, not observed behavior"
        )
        if not goals
        else None,
    }


def metrika_report(
    counter_id: str | None = None,
    metrics: str | None = None,
    dimensions: str | None = None,
    date1: str = "30daysAgo",
    date2: str = "today",
    filters: str | None = None,
    sort: str | None = None,
    limit: int = 100,
    paginate: bool = False,
) -> dict[str, Any]:
    """Return a Metrika report as flat JSON-serializable records.

    ``metrics`` and ``dimensions`` are comma-separated API identifiers such as ``ym:s:visits`` and
    ``ym:s:startURL``. Dates also accept relative forms such as ``30daysAgo``. With
    ``paginate=true`` the client collects successive pages but stops at 100,000 rows and marks the
    result as capped rather than implying that the dataset is complete. A ``Query is too
    complicated`` refusal is retried month by month and, if needed, at a sampled accuracy; the
    answer then carries ``split`` and ``accuracy`` saying what was actually used. Only count
    metrics that are additive over disjoint periods (``ym:s:visits``, ``ym:s:pageviews``)
    merge this way — a query for unique-visitor, ratio, or average metrics fails rather
    than return a summed value that would be wrong. ``sampled`` stays ``None`` when a
    slice did not report its sampling state — unknown is not ``false``.
    """
    if not counter_id or not metrics:
        raise ValueError("counter_id and metrics required")
    from seohead.data_sources.credentials import MissingCredential
    from seohead.data_sources.metrika import MetrikaClient, MetrikaError, rows_to_records

    params = {"ids": counter_id, "metrics": metrics, "date1": date1, "date2": date2}
    if dimensions:
        params["dimensions"] = dimensions
    if filters:
        params["filters"] = filters
    if sort:
        params["sort"] = sort
    try:
        body = MetrikaClient().report(params, paginate=paginate, limit=limit)
    except MissingCredential as exc:
        return {"ok": False, "error": str(exc)}
    except MetrikaError as exc:
        return {"ok": False, "error": exc.message, "status": exc.status}
    return {
        "ok": True,
        "counter_id": counter_id,
        "period": f"{date1}..{date2}",
        "total_rows": body.get("total_rows"),
        "returned": len(body.get("data") or []),
        "capped": body.get("capped", False),
        "incomplete": body.get("incomplete", False),
        # ``sampled`` reports what the API did, not what was requested: a query degraded
        # to accuracy=0.1 may still come back unsampled, and ``accuracy`` says what was
        # actually used. A body that does not say stays ``None`` — unknown is not false.
        "sampled": body.get("sampled"),
        "sample_share": body.get("sample_share"),
        "accuracy": body.get("accuracy_used") or (body.get("query") or {}).get("accuracy"),
        "split": body.get("split"),
        "totals": body.get("totals"),
        "rows": rows_to_records(body),
    }


TRAFFIC_REPORT_BASENAME = "metrika-traffic"


def _write_text_atomic(path: Path, text: str) -> None:
    atomic_write_bytes(path, text.encode("utf-8"))


def metrika_traffic_pdf(
    counter_id: Any = None,
    date1: str | None = None,
    date2: str | None = None,
    out_dir: str | None = None,
    document: dict[str, Any] | str | None = None,
    attribution: str = "last_significant",
    traffic: str = "organic",
    filters: str | None = None,
    lang: str | None = None,
    top: int = 15,
    site_label: str | None = None,
    brand: dict[str, Any] | str | None = None,
    gsc_rows: list[dict[str, Any]] | None = None,
    gsc_site_url: str | None = None,
    render: bool = True,
    pdf: bool = True,
    overwrite: bool = False,
    timeout: float = 120.0,
) -> dict[str, Any]:
    """Collect a Metrika traffic document and render it as a dashboard-style HTML/PDF report.

    Two stages that can run separately. Collection (``counter_id``, ``date1``, ``date2``) reads
    the Reporting API and writes ``metrika-traffic.json``; rendering formats a document (just
    collected, or an existing one passed as ``document``) into ``metrika-traffic.html`` and, when
    a Chrome/Edge/Chromium executable is found, ``metrika-traffic.pdf``. Rendering alone makes no
    network request. ``out_dir`` is required and is the only place written; existing files are
    refused unless ``overwrite`` is true. A missing browser leaves the HTML and reports the PDF as
    ``skipped`` with the reason.
    """
    import json

    from seohead.data_sources import metrika_traffic as core
    from seohead.reports import traffic_dashboard

    if not out_dir:
        raise ValueError("out_dir required: the directory that receives the report files")
    if document is not None and counter_id is not None:
        raise ValueError("pass either document (render only) or counter_id (collect), not both")
    if gsc_rows is not None and gsc_site_url:
        raise ValueError("pass either gsc_rows or gsc_site_url, not both")
    if lang is not None and lang not in core.LANGUAGES:
        raise ValueError(f"lang must be one of {list(core.LANGUAGES)}")
    if not isinstance(timeout, int | float) or isinstance(timeout, bool) or timeout <= 0:
        raise ValueError("timeout must be a positive number of seconds")
    brand_obj = traffic_dashboard.load_brand(brand)

    if document is not None:
        if isinstance(document, str):
            source = Path(document)
            if not source.is_file():
                raise ValueError(f"document file not found: {document}")
            try:
                document = json.loads(source.read_text(encoding="utf-8"))
            except ValueError as exc:
                raise ValueError(f"document is not valid JSON: {exc}") from None
        traffic_dashboard.validate_document(document)
    else:
        core.validate_request(
            counter_id,
            date1,
            date2,
            attribution=attribution,
            traffic=traffic,
            lang=lang or "en",
            top=top,
            filters=filters,
        )
        if gsc_rows is not None:
            gsc_rows = core.normalize_gsc_rows(gsc_rows)

    directory = Path(out_dir)
    targets = {
        "document": directory / f"{TRAFFIC_REPORT_BASENAME}.json" if document is None else None,
        "html": directory / f"{TRAFFIC_REPORT_BASENAME}.html" if render else None,
        "pdf": directory / f"{TRAFFIC_REPORT_BASENAME}.pdf" if render and pdf else None,
    }
    existing = sorted(str(p) for p in targets.values() if p is not None and p.exists())
    if existing and not overwrite:
        return {
            "ok": False,
            "error": "report files already exist; pass overwrite=true to replace them",
            "existing": existing,
        }

    collected = document is None
    if collected:
        from seohead.data_sources.credentials import MissingCredential
        from seohead.data_sources.metrika import MetrikaClient

        try:
            client = MetrikaClient()
        except MissingCredential as exc:
            return {"ok": False, "error": str(exc)}
        gsc_fetch = None
        if gsc_site_url:
            from seohead.data_sources import gsc

            def gsc_fetch(start: str, end: str) -> dict[str, Any]:
                return gsc.search_analytics_pages(
                    gsc_site_url, start_date=start, end_date=end, dimensions=["query"]
                )

        document = core.build_traffic_document(
            counter_id,
            date1,
            date2,
            client=client,
            attribution=attribution,
            traffic=traffic,
            filters=filters,
            lang=lang or "en",
            top=top,
            site_label=site_label,
            gsc_rows=gsc_rows,
            gsc_fetch=gsc_fetch,
        )
        if not document.get("ok"):
            return {
                "ok": False,
                "error": document.get("error") or "traffic summary unavailable",
                "requests": document.get("requests")
                or (document.get("methodology") or {}).get("requests"),
            }
    elif site_label:
        document = dict(document, site_label=site_label)

    directory.mkdir(parents=True, exist_ok=True)
    files: dict[str, str | None] = {"document": None, "html": None, "pdf": None}
    if targets["document"] is not None:
        _write_text_atomic(
            targets["document"], json.dumps(document, ensure_ascii=False, indent=2) + "\n"
        )
        files["document"] = str(targets["document"])
    pdf_result: dict[str, Any] = {"status": "skipped", "reason": "rendering was not requested"}
    if targets["html"] is not None:
        html = traffic_dashboard.render_html(document, brand=brand_obj, lang=lang)
        _write_text_atomic(targets["html"], html)
        files["html"] = str(targets["html"])
        if targets["pdf"] is None:
            pdf_result = {"status": "skipped", "reason": "pdf=false was requested"}
        else:
            from seohead.reports.chromium_pdf import print_to_pdf

            pdf_result = print_to_pdf(targets["html"], targets["pdf"], timeout=timeout)
            if pdf_result.get("status") == "ok":
                files["pdf"] = str(targets["pdf"])
    blocks = document.get("blocks") or {}
    return {
        "ok": True,
        "schema": document.get("schema"),
        "mode": "collect_and_render"
        if collected and render
        else ("collect" if collected else "render"),
        "period": document.get("period"),
        "attribution": document.get("attribution"),
        "traffic": document.get("traffic"),
        "files": files,
        "pdf": pdf_result,
        "blocks": {
            name: {"status": block.get("status"), "reason": block.get("reason")}
            for name, block in blocks.items()
            if isinstance(block, dict)
        },
        "warnings": document.get("warnings") or [],
        "requests": (document.get("methodology") or {}).get("requests") if collected else 0,
    }


def wayback_history(
    url: str | None = None,
    limit: int | None = None,
    from_date: str | None = None,
    to_date: str | None = None,
) -> dict[str, Any]:
    """Every recorded Wayback Machine snapshot of a URL: when it changed, and what it looked like.

    Free and keyless. Answers what a crawl cannot: *when* a page started returning its current
    status, and what content preceded it — the difference between a bug report and a restoration
    plan. A URL the archive never captured is not an error; it returns an empty list.
    """
    if not url:
        raise ValueError("url required")
    from seohead.data_sources import wayback as core

    return core.history(url, limit=limit, from_date=from_date, to_date=to_date)


def cloudflare_traffic(
    zone: str | None = None,
    since: str | None = None,
    until: str | None = None,
    project: str | None = None,
    include_configuration: bool = False,
) -> dict[str, Any]:
    """Read Cloudflare edge analytics by zone name or a selected project binding."""
    if bool(zone) == bool(project):
        raise ValueError("pass exactly one of zone or project")
    from seohead.data_sources import cloudflare as core

    return core.traffic(
        zone,
        project=project,
        since=since,
        until=until,
        include_configuration=include_configuration,
    )


def cloudflare_history(
    project: str | None = None,
    zone_id: str | None = None,
    snapshot_id: int | None = None,
    limit: int = 20,
    offset: int = 0,
    include_facts: bool = False,
    facts_offset: int = 0,
    facts_limit: int = 100,
) -> dict[str, Any]:
    """Read retained aggregate Cloudflare snapshots from a local project."""
    if not project:
        raise ValueError("project required")
    from seohead.data_sources.cloudflare_history import list_snapshots

    return list_snapshots(
        project=project,
        zone_id=zone_id,
        snapshot_id=snapshot_id,
        limit=limit,
        offset=offset,
        include_facts=include_facts,
        facts_offset=facts_offset,
        facts_limit=facts_limit,
    )


def cloudflare_logpush_import(
    input_path: str,
    project: str,
    max_events: int = 10_000,
) -> dict[str, Any]:
    """Import a supplied Logpush JSON Lines file into the project's local logstore."""
    from seohead.data_sources.cloudflare_logs import import_logpush_file

    return import_logpush_file(input_path, project, max_events=max_events)


def cloudflare_instant_logs(
    project: str,
    confirm_live_capture: bool = False,
    max_seconds: int = 10,
    max_events: int = 100,
    max_bytes: int = 1_048_576,
    sample: int = 10,
    cancel_event: Any = None,
) -> dict[str, Any]:
    """Explicitly create and bound a transient Cloudflare Instant Logs capture."""
    from seohead.data_sources.cloudflare_logs import instant_logs_capture

    return instant_logs_capture(
        project,
        confirm_live_capture=confirm_live_capture,
        max_seconds=max_seconds,
        max_events=max_events,
        max_bytes=max_bytes,
        sample=sample,
        cancel_event=cancel_event,
    )


def crtsh_subdomains(domain: str | None = None) -> dict[str, Any]:
    """Subdomains discovered from public Certificate Transparency logs (crt.sh).

    Free and keyless. Every TLS certificate ever issued for a domain is public record, so this
    finds hosts that no page ever links to — the gap `mirror-check` and `regions-check` both
    currently rely on being told about by hand.
    """
    if not domain:
        raise ValueError("domain required")
    from seohead.data_sources import crtsh as core

    return core.subdomains(domain)


def gsc_query(
    site_url: str | None = None,
    mode: str = "search_analytics",
    start_date: str = "28daysAgo",
    end_date: str = "today",
    dimensions: list[str] | None = None,
    row_limit: int = 1000,
    inspection_url: str | None = None,
) -> dict[str, Any]:
    """Google Search Console: search performance (``mode=search_analytics``) or Google's own
    indexing verdict for one URL (``mode=inspect_url``).

    Requires an own, verified property and an OAuth2 bearer token — see
    ``seohead sources-doctor`` and docs/SETUP.md. A missing token returns an explicit failure
    naming what to configure; it never fabricates a result.
    """
    if not site_url:
        raise ValueError("site_url required")
    from seohead.data_sources import gsc as core

    if mode == "inspect_url":
        if not inspection_url:
            raise ValueError("inspection_url required for mode=inspect_url")
        return core.inspect_url(site_url, inspection_url)
    if mode != "search_analytics":
        raise ValueError("mode must be search_analytics or inspect_url")
    return core.search_analytics(
        site_url,
        start_date=start_date,
        end_date=end_date,
        dimensions=dimensions,
        row_limit=row_limit,
    )


def webmaster_url_queries(
    host_id: str | None = None,
    url: str | None = None,
    url_contains: str | None = None,
    max_urls: int = 100,
    max_queries_per_url: int = 500,
    start_date: str | None = None,
    end_date: str | None = None,
) -> dict[str, Any]:
    """Read bounded URL-to-query evidence from an own Yandex Webmaster host."""
    if not host_id:
        raise ValueError("host_id required")
    from seohead.data_sources import yandex_webmaster as core

    return core.url_queries(
        host_id,
        url=url,
        url_contains=url_contains,
        max_urls=max_urls,
        max_queries_per_url=max_queries_per_url,
        start_date=start_date,
        end_date=end_date,
    )


def miratext_analyze(**request: Any) -> dict[str, Any]:
    """Start or resume a bounded Miratext analysis; paid modes need explicit confirmation."""
    from seohead.data_sources import miratext

    return miratext.analyze(**request)


def gsc_archive(
    database: str | None = None,
    action: str = "status",
    site_url: str | None = None,
    start_date: str | None = None,
    end_date: str | None = None,
    max_requests: int = 1,
    pause: float = 1.0,
    backup_path: str | None = None,
) -> dict[str, Any]:
    """Manage an explicitly selected local GSC SQLite archive.

    Status is offline and does not create an absent archive. Prepare is the only action
    that creates a database; run performs bounded API work and resumes saved checkpoints.
    Backup snapshots an existing archive into a new local file. No credentials are inputs.
    """
    import math
    import sqlite3

    from seohead.data_sources.gsc import _validate_date_range
    from seohead.data_sources.gsc_archive import Archive

    if action not in ("status", "prepare", "run", "backup"):
        raise ValueError("action must be status, prepare, run, or backup")
    if type(max_requests) is not int or not 1 <= max_requests <= 1000:
        raise ValueError("max_requests must be an integer in 1..1000")
    if type(pause) not in (int, float) or not math.isfinite(pause) or not 0 <= pause <= 60:
        raise ValueError("pause must be a finite number in 0..60 seconds")

    def local_path(value: str | None, name: str) -> Path:
        if (
            not isinstance(value, str)
            or not value.strip()
            or "\x00" in value
            or "://" in value
            or value.startswith(("file:", ":memory:"))
        ):
            raise ValueError(f"{name} must be an explicit local file path")
        path = Path(value).expanduser().resolve()
        if path.exists() and not path.is_file():
            raise ValueError(f"{name} must point to a file")
        return path

    path = local_path(database, "database")
    if action == "prepare":
        if not isinstance(site_url, str) or not site_url.strip():
            raise ValueError("site_url is required for prepare")
        if _validate_date_range(start_date, end_date):
            raise ValueError("prepare requires an ordered YYYY-MM-DD start_date and end_date")
    elif any(v is not None for v in (site_url, start_date, end_date)):
        raise ValueError("site_url, start_date, and end_date apply only to prepare")
    destination = None
    if action == "backup":
        destination = local_path(backup_path, "backup_path")
        if destination == path or destination.exists():
            raise ValueError("backup_path must be a new file different from database")
    elif backup_path is not None:
        raise ValueError("backup_path applies only to backup")
    if action != "prepare" and not path.is_file():
        return {
            "ok": False,
            "state": "not_found",
            "database": str(path),
            "error": "Archive does not exist; prepare it first",
        }

    archive = None
    try:
        archive = Archive(
            path, create=action == "prepare", read_only=action in ("status", "backup")
        )
        if action == "prepare":
            return archive.prepare(site_url, start_date, end_date)
        if action == "run":
            return archive.run_batch(max_requests=max_requests, pause=pause)
        if action == "backup":
            return {"ok": True, "database": str(path), "backup": archive.backup(destination)}
        return archive.status()
    except (OSError, sqlite3.Error, RuntimeError):
        return {
            "ok": False,
            "state": "failed",
            "database": str(path),
            "error": "Archive operation failed; check file access, writer lock, schema, and GSC credentials",
        }
    finally:
        if archive is not None:
            archive.close()


def crux_report(
    url: str | None = None,
    origin: str | None = None,
    urls: list[str] | None = None,
    form_factor: str | None = None,
    metrics: list[str] | None = None,
    max_samples: int = 25,
    cache_dir: str | None = None,
    cache_max_age_hours: float = 24,
) -> dict[str, Any]:
    """Field Core Web Vitals (LCP, INP, CLS) as real Chrome users experienced them, at the 75th
    percentile — the honest counterpart to a synthesized lab score (see `render-check` and
    issue #59). Pass exactly one of ``url``/``origin``. Requires a Chrome UX Report API key.
    """
    from seohead.data_sources import crux as core

    if urls is not None:
        if url or origin or metrics:
            raise ValueError("urls cannot be combined with url, origin, or metrics")
        return core.sample_urls(
            urls,
            form_factor=form_factor,
            max_samples=max_samples,
            cache_dir=cache_dir,
            cache_max_age_hours=cache_max_age_hours,
        )
    if cache_dir or max_samples != 25 or cache_max_age_hours != 24:
        raise ValueError("sample budget and cache options require urls")
    return core.query(url=url, origin=origin, form_factor=form_factor, metrics=metrics)


def indexnow_submit(
    urls: list[str] | None = None,
    host: str | None = None,
    key_location: str | None = None,
) -> dict[str, Any]:
    """Push changed URLs to Bing, Yandex, Naver, and Seznam in one call.

    Google has not joined IndexNow as of 2026 — a submission here does not affect Google's crawl
    schedule. Requires a self-generated key, published at ``https://<host>/<key>.txt`` before the
    first call; see docs/SETUP.md.
    """
    if not urls:
        raise ValueError("urls required")
    if not host:
        raise ValueError("host required")
    from seohead.data_sources import indexnow as core

    return core.submit(urls, host=host, key_location=key_location)


def regions_tree(save_to: str | None = None) -> dict[str, Any]:
    """Fetch the authoritative Yandex region tree used by the ``regions[]`` parameter.

    This is Wordstat's only free method. Static mappings in ``data_sources/regions.py`` are faster,
    but some entries are documentation-derived rather than verified against the current API; use
    this live tree to validate an unfamiliar region before issuing a paid request.
    """
    from seohead.data_sources import yandex_regions as regions_core

    return regions_core.fetch_tree(save_to=save_to)


def spend_report(since: str | None = None, csv_path: str | None = None) -> dict[str, Any]:
    """Summarize recorded provider charges by source, operation, and day, or with csv_path
    write one CSV row per journal entry instead and return where it went."""
    from seohead.data_sources import spend as spend_core

    if csv_path:
        count = spend_core.write_csv(csv_path, since=since)
        return {"ok": True, "csv": str(Path(csv_path).expanduser()), "rows": count, "since": since}
    return spend_core.report(since=since)


def sources_doctor() -> dict[str, Any]:
    """Report redacted credential references and readiness without verifying provider access."""
    from seohead.data_sources import credentials as creds

    checks = {
        "arsenkin": ("arsenkin/token", "ARSENKIN_TOKEN"),
        "yandex_cloud_api_key": ("yandex-wordstat/api_key", "YANDEX_CLOUD_API_KEY"),
        "yandex_cloud_folder": ("yandex-wordstat/folder_id", "YANDEX_CLOUD_FOLDER_ID"),
        "yandex_metrika": ("yandex-metrika/token", "YANDEX_METRIKA_TOKEN"),
        "dataforseo": ("dataforseo/login", "DATAFORSEO_LOGIN"),
        "gsc": ("gsc/access_token", "GSC_ACCESS_TOKEN"),
        "crux": ("crux/api_key", "CRUX_API_KEY"),
        "indexnow": ("indexnow/key", "INDEXNOW_KEY"),
        "miratext": ("miratext/api_key", "MIRATEXT_API_KEY"),
    }
    sources = {
        name: {
            "ready": creds.available(path, env),
            "file": str(creds.CONFIG_ROOT / path),
            "env": env,
        }
        for name, (path, env) in checks.items()
    }
    dataforseo_ready, dataforseo_components = creds.dataforseo_ready()
    sources["dataforseo"]["ready"] = dataforseo_ready
    sources["dataforseo"]["components"] = dataforseo_components
    from seohead.data_sources import spend as spend_core
    from seohead.data_sources.providers import sources_doctor as provider_doctor

    provider_status = provider_doctor()["providers"]
    gsc_provider = provider_status["gsc"]
    gsc_components = gsc_provider["credential_components"]
    sources["gsc"]["ready"] = any(gsc_components.values())
    sources["gsc"]["components"] = gsc_components
    sources["gsc"]["service_account_status"] = gsc_provider["service_account_status"]
    return {
        "ok": True,
        "sources": sources,
        "provider_status": provider_status,
        "spend_log": str(spend_core.log_path()),
    }


def sources_sync(
    source: str | None = None,
    resource: str | None = None,
    start_date: str | None = None,
    end_date: str | None = None,
    db: str | None = None,
    project: str | None = None,
    force: bool = False,
) -> dict[str, Any]:
    """Fetch missing or explicitly forced days into one local provider database.

    ``source`` is gsc, ga4, metrika, webmaster, or webmaster_history; ``resource`` is the
    property, counter, or Webmaster host ID. Lagged days are recorded, not fetched.
    """
    from seohead.data_sources import sources_db as core

    if not source:
        raise ValueError(f"source required: one of {sorted(core.SOURCES)}")
    return core.sync(
        core.db_path(db, project),
        source,
        resource or "",
        start_date=start_date,
        end_date=end_date,
        force=force,
    )


def sources_status(db: str | None = None, project: str | None = None) -> dict[str, Any]:
    """Summarize requested, complete, empty, partial, failed and lagged days offline."""
    from seohead.data_sources import sources_db as core

    return core.status(core.db_path(db, project))


def sources_export(
    source: str | None = None,
    resource: str | None = None,
    start_date: str | None = None,
    end_date: str | None = None,
    match: str | None = None,
    limit: int = 1000,
    out: str | None = None,
    db: str | None = None,
    project: str | None = None,
) -> dict[str, Any]:
    """Read at most limit ordered provider rows or write them to a new private CSV."""
    from seohead.data_sources import sources_db as core

    if not source:
        raise ValueError(f"source required: one of {sorted(core.SOURCES)}")
    return core.query(
        core.db_path(db, project),
        source,
        resource=resource,
        start_date=start_date,
        end_date=end_date,
        match=match,
        limit=limit,
        out=out,
    )


def scan_reanalyze(
    input_path: str,
    out: str,
    producer_build: str | None = None,
    step: str = "all",
    max_documents: int | None = None,
) -> dict[str, Any]:
    """Derive a fresh SQLite scan by parsing retained evidence without network access."""
    from seohead.mcp.reanalysis_handlers import reanalyze_scan

    return reanalyze_scan(
        input_path=input_path,
        out=out,
        producer_build=producer_build,
        step=step,
        max_documents=max_documents,
    )


def scan_sitemap_generate(
    input_path: str,
    out_dir: str,
    sitemap_base_url: str,
    include_images: bool = False,
    exclude: list[str] | None = None,
) -> dict[str, Any]:
    """Write XML sitemaps from retained canonical/indexable 200 responses; no network."""
    from seohead.mcp.scan_sitemaps import generate_scan_sitemaps

    return generate_scan_sitemaps(
        input_path=input_path,
        out_dir=out_dir,
        sitemap_base_url=sitemap_base_url,
        include_images=include_images,
        exclude=exclude,
    )


def scan_graph(
    input_path: str,
    positions: list[str] | str | None = None,
    section: str | None = None,
    around: str | None = None,
    depth: int = 2,
    aggregate: str = "none",
    representation: str = "all",
    node_offset: int = 0,
    edge_offset: int = 0,
    node_limit: int = 1000,
    edge_limit: int = 2000,
    max_nodes: int = 5000,
    max_edges: int = 20000,
    compare_to: str | None = None,
    timeout_seconds: float = 1.0,
    max_bytes: int = 8388608,
) -> dict[str, Any]:
    """Read a bounded retained internal graph, its sections and scan-to-scan changes."""
    from seohead.mcp.history_handlers import scan_graph as core

    return core(
        input_path=input_path,
        positions=positions,
        section=section,
        around=around,
        depth=depth,
        aggregate=aggregate,
        representation=representation,
        node_offset=node_offset,
        edge_offset=edge_offset,
        node_limit=node_limit,
        edge_limit=edge_limit,
        max_nodes=max_nodes,
        max_edges=max_edges,
        compare_to=compare_to,
        timeout_seconds=timeout_seconds,
        max_bytes=max_bytes,
    )


def scan_list(
    directory: str | None = None, offset: int = 0, limit: int = 100, project: str | None = None
) -> dict[str, Any]:
    from seohead.mcp.history_handlers import scan_list as core

    if project is not None:
        from seohead.projects.workspace import open_project

        opened = open_project(project)
        directory = directory or str(Path(opened["path"]) / "scans")
    if directory is None:
        raise ValueError("directory or project is required")
    return core(directory, offset=offset, limit=limit)


def scan_inspect(
    input_path: str,
    table: str = "pages",
    offset: int = 0,
    limit: int = 100,
    max_bytes: int = 1_048_576,
    columns: list[str] | None = None,
    total: bool = False,
) -> dict[str, Any]:
    from seohead.mcp.history_handlers import scan_inspect as core

    return core(
        input_path,
        table=table,
        offset=offset,
        limit=limit,
        max_bytes=max_bytes,
        columns=columns,
        total=total,
    )


def scan_url_counts(
    input_path: str,
    filter_sets: list[list[dict[str, Any]]],
    count_timeout_seconds: float = 30.0,
) -> dict[str, Any]:
    """Exact filtered counts of many filter sets over one saved scan in one call (#1553)."""
    from seohead.storage.url_query import scan_url_counts as core

    return core(input_path, filter_sets, count_timeout_seconds=count_timeout_seconds)


def scan_url_query(
    input_path: str,
    filters: list[dict[str, Any]] | None = None,
    sort: str | None = None,
    direction: str = "asc",
    columns: list[str] | None = None,
    offset: int = 0,
    limit: int = 200,
    count_timeout_seconds: float = 1.0,
    max_bytes: int = 1_048_576,
    facets: list[str] | str | None = None,
    preset: str | None = None,
    export: str | None = None,
    export_format: str = "csv",
    export_max_rows: int | None = None,
    issue_check: str | list[str] | None = None,
    issue_severity: str | None = None,
    include_check_counts: bool = False,
) -> dict[str, Any]:
    """Filter, sort and paginate the whole page table of one saved scan, read-only.

    With ``export`` set, writes every matching row to that new file instead of returning a page.
    """
    from seohead.storage.url_query import EXPORT_MAX_ROWS, export_scan_query
    from seohead.storage.url_query import scan_url_query as core

    if export is not None:
        return export_scan_query(
            input_path,
            export,
            fmt=export_format,
            preset=preset,
            filters=filters,
            columns=columns,
            max_rows=EXPORT_MAX_ROWS if export_max_rows is None else export_max_rows,
        )
    return core(
        input_path,
        filters=filters,
        sort=sort,
        direction=direction,
        columns=columns,
        offset=offset,
        limit=limit,
        count_timeout_seconds=count_timeout_seconds,
        issue_check=issue_check,
        issue_severity=issue_severity,
        include_check_counts=include_check_counts,
        max_bytes=max_bytes,
        facets=facets,
        preset=preset,
    )


def scan_directory_tree(
    input_path: str,
    max_depth: int = 3,
    max_children: int = 50,
    max_nodes: int = 10_000,
    max_bytes: int = 1_048_576,
    timeout_seconds: float = 15.0,
) -> dict[str, Any]:
    """Read a bounded URL-path directory tree from retained page URLs only."""
    from seohead.storage.directory_tree import scan_directory_tree as core

    try:
        return {
            "ok": True,
            **core(
                input_path,
                max_depth=max_depth,
                max_children=max_children,
                max_nodes=max_nodes,
                max_bytes=max_bytes,
                timeout_seconds=timeout_seconds,
            ),
        }
    except (ValueError, OSError) as exc:
        return {
            "ok": False,
            "state": "invalid",
            "reason_code": "invalid_directory_tree",
            "error": str(exc),
        }


def headers_analyze(
    input_path: str,
    url: str | None = None,
    offset: int = 0,
    limit: int = 100,
    max_bytes: int = 1_048_576,
    timeout_seconds: float = 15.0,
) -> dict[str, Any]:
    """Analyze headers and phase timing retained in a scan; never fetch the URL."""
    import sqlite3

    from seohead.storage.headers_analyze import analyze

    try:
        return analyze(
            input_path,
            url=url,
            offset=offset,
            limit=limit,
            max_bytes=max_bytes,
            timeout_seconds=timeout_seconds,
        )
    except (ValueError, OSError, sqlite3.Error) as exc:
        return {
            "ok": False,
            "state": "invalid",
            "reason_code": "invalid_headers_analysis",
            "error": str(exc),
        }


def scan_url_history(project: str, url: str, limit: int = 50) -> dict[str, Any]:
    """State of one exact URL in each retained scan of a project, newest first, read-only."""
    from seohead.projects.workspace import _load
    from seohead.storage.url_history import url_history as core

    root, _document = _load(project)
    return core(root / "scans", url, limit=limit)


def scan_url_detail(
    input_path: str,
    url: str,
    response_offset: int = 0,
    response_limit: int = 10,
    form_offset: int = 0,
    form_limit: int = 20,
    max_bytes: int = 1_048_576,
    body_offset: int = 0,
    body_length: int = 4096,
    max_body_bytes: int = 8 * 1024 * 1024,
) -> dict[str, Any]:
    """Read one exact native URL's bounded retained transport metadata offline."""
    from seohead.mcp.history_handlers import scan_url_detail as core

    return core(
        input_path,
        url,
        response_offset=response_offset,
        response_limit=response_limit,
        form_offset=form_offset,
        form_limit=form_limit,
        max_bytes=max_bytes,
        body_offset=body_offset,
        body_length=body_length,
        max_body_bytes=max_body_bytes,
    )


def scan_body_read(
    input_path: str,
    url: str,
    representation: str = "static",
    variant_key: str | None = None,
    offset: int = 0,
    length: int = 4096,
    max_decoded_bytes: int = 8 * 1024 * 1024,
) -> dict[str, Any]:
    from seohead.mcp.history_handlers import scan_body_read as core

    return core(
        input_path,
        url,
        representation=representation,
        variant_key=variant_key,
        offset=offset,
        length=length,
        max_decoded_bytes=max_decoded_bytes,
    )


def scan_findings_page(
    project: str,
    scan_uuid: str | None = None,
    offset: int = 0,
    limit: int = 100,
    query: str = "",
    url: str | None = None,
    sort: str = "severity",
    descending: bool = False,
) -> dict[str, Any]:
    from seohead.projects.observer import findings_page

    return findings_page(
        project,
        scan_uuid=scan_uuid,
        offset=offset,
        limit=limit,
        query=query,
        url=url,
        sort=sort,
        descending=descending,
    )


def scan_link_inspect(
    input_path: str,
    view: str = "path",
    seed: str | None = None,
    target: str | None = None,
    representation: str = "all",
    cursor: str | None = None,
    link_id: int | None = None,
    document_id: int | None = None,
    offset: int = 0,
    limit: int = 100,
    max_bytes: int = 1_048_576,
    max_body_bytes: int = 5 * 1024 * 1024,
    max_nodes: int = 10_000,
    max_edges: int = 200_000,
    max_depth: int = 20,
    timeout_seconds: float = 15.0,
    url: str | None = None,
    direction: str = "out",
    link_type: str = "all",
    follow: str = "all",
    status_class: str = "all",
    contains: str | None = None,
    sort: str = "order",
    preset: str | None = None,
) -> dict[str, Any]:
    from seohead.mcp.history_handlers import scan_link_inspect as core

    return core(
        input_path,
        view=view,
        seed=seed,
        target=target,
        representation=representation,
        cursor=cursor,
        link_id=link_id,
        document_id=document_id,
        offset=offset,
        limit=limit,
        max_bytes=max_bytes,
        max_body_bytes=max_body_bytes,
        max_nodes=max_nodes,
        max_edges=max_edges,
        max_depth=max_depth,
        timeout_seconds=timeout_seconds,
        url=url,
        direction=direction,
        link_type=link_type,
        follow=follow,
        status_class=status_class,
        contains=contains,
        sort=sort,
        preset=preset,
    )


def scan_status(input_path: str, full_validation: bool = False) -> dict[str, Any]:
    from seohead.mcp.history_handlers import scan_status as core

    return core(input_path, full_validation=full_validation)


def scan_rendered_routes(input_path: str) -> dict[str, Any]:
    from seohead.mcp.history_handlers import scan_rendered_routes as core

    return core(input_path)


def scan_snapshot(input_path: str, out: str) -> dict[str, Any]:
    from seohead.mcp.history_handlers import scan_snapshot as core

    return core(input_path, out)


def scan_export(
    input_path: str,
    out: str,
    format: str = "json",
    records: Any = None,
    fields: Any = None,
) -> dict[str, Any]:
    """Export retained scan data under the versioned ``scan_export.v1`` contract."""
    from seohead.storage.scan_export import export_scan_data

    return export_scan_data(input_path, out, fmt=format, records=records, fields=fields)


def scan_pin(input_path: str, pinned: bool = True) -> dict[str, Any]:
    from seohead.mcp.history_handlers import scan_pin as core

    return core(input_path, pinned=pinned)


def scan_prune(
    directory: str | None = None,
    older_than_days: int = 30,
    keep_newest: int = 5,
    plan: dict[str, Any] | str | None = None,
    apply: bool = False,
    project: str | None = None,
) -> dict[str, Any]:
    from seohead.mcp.history_handlers import scan_prune as core

    if project is not None:
        from seohead.projects.workspace import open_project

        opened = open_project(project)
        directory = directory or str(Path(opened["path"]) / "scans")
    if directory is None:
        raise ValueError("directory or project is required")
    return core(
        directory,
        older_than_days=older_than_days,
        keep_newest=keep_newest,
        plan=plan,
        apply=apply,
    )


def scan_body_diff(
    left: str,
    right: str,
    url: str,
    variant_key: str | None = None,
    representation: str = "static",
    text: bool = False,
    max_bytes: int = 5 * 1024 * 1024,
    max_lines: int = 10_000,
    left_representation: str | None = None,
    right_representation: str | None = None,
) -> dict[str, Any]:
    from seohead.mcp.history_handlers import scan_body_diff as core

    return core(
        left,
        right,
        url,
        variant_key=variant_key,
        representation=representation,
        text=text,
        max_bytes=max_bytes,
        max_lines=max_lines,
        left_representation=left_representation,
        right_representation=right_representation,
    )


def project_new(
    directory: str,
    target: str,
    label: str | None = None,
    facts: list[dict[str, Any]] | None = None,
    template_references: list[str] | None = None,
    profile_references: list[str] | None = None,
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_new as core

    return core(
        directory,
        target,
        label=label,
        facts=facts,
        template_references=template_references,
        profile_references=profile_references,
    )


def project_open(directory: str, expected_site: str | None = None) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_open as core

    return core(directory, expected_site=expected_site)


def project_archive(
    project: str,
    out: str,
    dry_run: bool = False,
    exclude_bodies: bool = False,
    exclude_bodies_older_than_days: int | None = None,
    keep_newest_scans: int | None = None,
) -> dict[str, Any]:
    from seohead.projects.archive import archive_project

    return archive_project(
        project,
        out,
        dry_run=dry_run,
        exclude_bodies=exclude_bodies,
        exclude_bodies_older_than_days=exclude_bodies_older_than_days,
        keep_newest_scans=keep_newest_scans,
    )


def project_restore(archive: str, to: str) -> dict[str, Any]:
    from seohead.projects.archive import restore_project

    return restore_project(archive, to)


def project_update(directory: str, label: str, expected_revision: int) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_update as core

    return core(directory, label=label, expected_revision=expected_revision)


def project_delete(directory: str) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_delete as core

    return core(directory)


def project_settings(directory: str) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_settings as core

    return core(directory)


def project_settings_propose(
    directory: str, changes: dict[str, Any], expected_revision: int
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_settings_propose as core

    return core(directory, changes=changes, expected_revision=expected_revision)


def project_settings_proposals(directory: str, offset: int = 0, limit: int = 50) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_settings_proposals as core

    return core(directory, offset=offset, limit=limit)


def project_settings_review(
    directory: str, proposal_id: str, decision: str, expected_revision: int
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_settings_review as core

    return core(
        directory,
        proposal_id=proposal_id,
        decision=decision,
        expected_revision=expected_revision,
    )


def project_action_log(
    directory: str,
    offset: int = 0,
    limit: int = 50,
    client: str | None = None,
    outcome: str | None = None,
    query: str = "",
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_action_log as core

    return core(
        directory,
        offset=offset,
        limit=limit,
        client=client,
        outcome=outcome,
        query=query,
    )


def project_agent_heartbeat(directory: str) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_agent_heartbeat as core

    return core(directory)


def project_status(directory: str, consumer: str | None = None) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_basic_status

    return project_basic_status(directory, consumer=consumer)


def doctor(
    projects_directory: str | None = None,
    config_path: str | None = None,
    sf_cli: str | None = None,
) -> dict[str, Any]:
    from seohead.core.doctor import doctor as core_doctor

    return core_doctor(
        projects_directory=projects_directory,
        config_path=config_path,
        sf_cli=sf_cli,
    )


def update_check(consent: bool, automatic: bool = False, max_releases: int = 5) -> dict[str, Any]:
    from seohead.core.update import check_updates

    return check_updates(consent=consent, automatic=automatic, max_releases=max_releases)


def update_download(
    consent: bool,
    tag: str,
    asset_name: str,
    out: str,
    max_bytes: int = 250 * 1024 * 1024,
    progress=None,
    cancelled=None,
) -> dict[str, Any]:
    from seohead.core.update import download_update

    return download_update(
        consent=consent,
        tag=tag,
        asset_name=asset_name,
        out=out,
        max_bytes=max_bytes,
        progress=progress,
        cancelled=cancelled,
    )


def project_sources_link(
    directory: str, service: str, resource: str, label: str | None = None
) -> dict[str, Any]:
    from seohead.projects.bindings import link

    return link(directory, service, resource, label=label)


def project_sources_unlink(directory: str, service: str, resource: str) -> dict[str, Any]:
    from seohead.projects.bindings import unlink

    return unlink(directory, service, resource)


def project_sources_list(directory: str) -> dict[str, Any]:
    from seohead.projects.bindings import list_bindings

    return list_bindings(directory)


def project_progress(
    directory: str, limit: int = 20, offset: int = 0, consumer: str | None = None
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_progress as core

    return core(directory, limit=limit, offset=offset, consumer=consumer)


def remediation_summary(ledger: str) -> dict[str, Any]:
    """Read explicit remediation and recheck denominators from a local ledger."""
    from seohead.storage.ledger import remediation_summary as core

    return core(ledger)


def remediation_create(path: str, project_dir: str, producer_build: str) -> dict[str, Any]:
    """Create one new empty ledger bound to a project; existing files are refused."""
    from seohead.storage.ledger import create_ledger, ledger_summary

    out = create_ledger(path, project_dir=project_dir, producer_build=producer_build)
    return {"ledger": str(out), "summary": ledger_summary(out)}


def remediation_ingest(ledger: str, scan: str) -> dict[str, Any]:
    """Ingest one saved audit into a ledger as baseline/history; re-ingest is idempotent."""
    from seohead.storage.ledger import ingest_scan

    return ingest_scan(ledger, scan)


def workflow_start(
    directory: str,
    scenario_id: str,
    steps: list[str],
    expected_revision: int = 0,
    context: dict[str, Any] | None = None,
) -> dict[str, Any]:
    from seohead.projects.execution import start

    return start(
        directory,
        scenario_id=scenario_id,
        steps=steps,
        expected_revision=expected_revision,
        context=context,
    )


def workflow_checkpoint(
    directory: str,
    run_id: str,
    step_id: str,
    state: str,
    evidence: list[dict] | None = None,
    expected_revision: int = 0,
    review: dict[str, Any] | None = None,
    phase: str | None = None,
) -> dict[str, Any]:
    from seohead.projects.execution import checkpoint

    return checkpoint(
        directory,
        run_id=run_id,
        step_id=step_id,
        state=state,
        evidence=evidence,
        expected_revision=expected_revision,
        review=review,
        phase=phase,
    )


def workflow_status(directory: str) -> dict[str, Any]:
    from seohead.projects.execution import status

    return status(directory)


def workflow_execute(
    directory: str,
    scenario_id: str,
    steps: list[str],
    outcomes: list[dict],
    context: dict[str, Any] | None = None,
) -> dict[str, Any]:
    from seohead.projects.execution import execute

    return execute(
        directory, scenario_id=scenario_id, steps=steps, outcomes=outcomes, context=context
    )


def workflow_resume(directory: str, run_id: str, expected_revision: int) -> dict[str, Any]:
    from seohead.projects.execution import resume

    return resume(directory, run_id=run_id, expected_revision=expected_revision)


def monitor_configure(directory: str, policy: dict, expected_revision: int = 0) -> dict[str, Any]:
    from seohead.projects.monitoring import configure

    return configure(directory, policy, expected_revision)


def monitor_run(
    directory: str, scan_id: str, observations: list[dict], expected_revision: int
) -> dict[str, Any]:
    from seohead.projects.monitoring import run

    return run(directory, scan_id, observations, expected_revision)


def monitor_collect(directory: str, expected_revision: int, apply: bool = False) -> dict[str, Any]:
    """Preview or explicitly collect one existing bounded monitoring claim."""
    from seohead.mcp.monitor_handlers import monitor_collect as core

    return core(directory=directory, expected_revision=expected_revision, apply=apply)


def monitor_local_deliver(directory: str, scan_id: str, expected_revision: int) -> dict[str, Any]:
    """Record a local monitoring receipt without an external transport."""
    from seohead.mcp.monitor_handlers import monitor_local_deliver as core

    return core(directory=directory, scan_id=scan_id, expected_revision=expected_revision)


def monitor_status(directory: str) -> dict[str, Any]:
    from seohead.projects.monitoring import status

    return status(directory)


def schedule_show(directory: str) -> dict[str, Any]:
    """Read the project's scheduled-scan settings, last run and next due time."""
    from seohead.projects.schedule import show

    return show(directory)


def schedule_set(
    directory: str,
    expected_revision: int,
    settings: dict | None = None,
    **fields: Any,
) -> dict[str, Any]:
    """Merge a partial update into the project's schedule settings (revisioned)."""
    from seohead.projects.schedule import configure

    return configure(directory, expected_revision=expected_revision, settings=settings, **fields)


def schedule_run_due(directory: str) -> dict[str, Any]:
    """Run this project's scheduled scans if they are due; refuses overlap and out-of-window runs."""
    from seohead.projects.schedule import run_due

    return run_due(directory, scan=crawl_site)


def schedule_install(
    directory: str, kind: str, dest: str | None = None, python: str | None = None
) -> dict[str, Any]:
    """Write a launchd plist, cron line or Task Scheduler command for run-due. Explicit only."""
    from seohead.projects.schedule_install import install

    return install(directory, kind=kind, dest=dest, python=python)


def schedule_uninstall(directory: str, kind: str, dest: str | None = None) -> dict[str, Any]:
    from seohead.projects.schedule_install import uninstall

    return uninstall(directory, kind=kind, dest=dest)


def monitor_schedule(directory: str, action: str, expected_revision: int) -> dict[str, Any]:
    """Record a local monitor runner claim without starting a background service."""
    from seohead.projects.monitoring import schedule

    return schedule(directory, action=action, expected_revision=expected_revision)


def remediation_cases(
    ledger: str,
    check: str | None = None,
    url: str | None = None,
    finding_key: str | None = None,
    limit: int | None = 100,
    offset: int = 0,
    source_scan_id: int | None = None,
    group_ref: str | None = None,
    max_bytes: int | None = None,
) -> dict[str, Any]:
    """Read paginated ledger cases and immutable observation/decision history."""
    from seohead.storage.ledger import LedgerError, read_cases, read_group_members

    if source_scan_id is not None or group_ref is not None:
        if check is not None or url is not None or finding_key is not None:
            raise LedgerError("group member selection cannot be combined with finding selectors")
        return read_group_members(
            ledger,
            source_scan_id=source_scan_id,
            group_ref=group_ref,
            offset=offset,
            limit=limit,
            max_bytes=1024 * 1024 if max_bytes is None else max_bytes,
        )
    if max_bytes is not None:
        raise LedgerError("max_bytes requires source_scan_id and group_ref")

    return read_cases(
        ledger, check=check, url=url, finding_key=finding_key, limit=limit, offset=offset
    )


def remediation_transition(
    ledger: str,
    occurrence_key: str,
    state: str,
    actor: str,
    reason: str,
    expected_revision: int,
    observation_id: int | None = None,
    decided_at: str | None = None,
) -> dict[str, Any]:
    """Append one evidence-bound lifecycle decision to a local ledger."""
    from seohead.storage.ledger import transition_occurrence

    return transition_occurrence(
        ledger,
        occurrence_key=occurrence_key,
        state=state,
        actor=actor,
        reason=reason,
        expected_revision=expected_revision,
        observation_id=observation_id,
        decided_at=decided_at,
    )


def remediation_record_verification(
    ledger: str,
    verification_path: str,
    actor: str,
    expected_revision: int,
    occurrence_keys: list[str] | None = None,
    task_id: str = "unassigned",
) -> dict[str, Any]:
    """Persist a retained bounded recheck artifact against pending ledger cases."""
    from seohead.storage.ledger import record_verification

    return record_verification(
        ledger,
        verification_path,
        actor=actor,
        expected_revision=expected_revision,
        occurrence_keys=occurrence_keys,
        task_id=task_id,
    )


def remediation_recheck(
    ledger: str,
    baseline: Any,
    occurrence_keys: list[str],
    actor: str,
    expected_revision: int,
    out_dir: str,
    task_id: str = "unassigned",
    after: Any = None,
    config: str | None = None,
) -> dict[str, Any]:
    """Run the guarded bounded verifier for exact pending ledger cases.

    The supplied baseline must match the retained raw or canonical audit digest
    and original results policy for every selected target occurrence.  This keeps a finding-key recheck
    from silently selecting another revision or widening into a whole-site
    crawl.  The verifier writes its immutable evidence first; only then does
    the ledger atomically bind typed outcomes to that evidence.
    """
    from seohead.core.verification import source_identity
    from seohead.storage.inputs import load_audit_source
    from seohead.storage.ledger import (
        LedgerError,
        canonical_url,
        occurrence_matches_baseline,
        open_ledger,
        record_verification,
    )

    if not isinstance(occurrence_keys, list) or not occurrence_keys:
        raise ValueError("occurrence_keys must be a nonempty list of exact ledger case keys")
    if len(occurrence_keys) > 5_000 or len(set(occurrence_keys)) != len(occurrence_keys):
        raise ValueError("occurrence_keys must be a distinct bounded case selection")
    baseline_source = load_audit_source(baseline, "baseline")
    try:
        baseline_identity = source_identity(baseline_source)
        baseline_sha = baseline_identity["audit_sha256"]
        baseline_policy = baseline_identity["results_policy_fingerprint"]
        with contextlib.closing(open_ledger(ledger)) as con:
            revision = int(
                con.execute("SELECT ledger_revision FROM ledger WHERE singleton=1").fetchone()[0]
            )
            if revision != expected_revision:
                raise LedgerError("ledger revision changed; reread cases before starting a recheck")
            bindings: dict[str, tuple[str, str, str]] = {}
            for key in occurrence_keys:
                rows = con.execute(
                    "SELECT o.occurrence_id,o.occurrence_key,o.current_state,o.subject_value,"
                    "c.check_key,ob.issue_ordinal "
                    "FROM occurrence o JOIN check_def c ON c.check_id=o.check_id "
                    "JOIN observation ob ON ob.occurrence_id=o.occurrence_id "
                    "JOIN source_scan s ON s.source_scan_id=ob.source_scan_id "
                    "WHERE o.occurrence_key=? AND o.subject_type='url' AND ob.role='target' "
                    "AND (s.audit_sha256=? OR s.canonical_audit_sha256=?) "
                    "AND s.config_fingerprint=?",
                    (key, baseline_sha, baseline_sha, baseline_policy),
                ).fetchall()
                if len(rows) != 1 or not occurrence_matches_baseline(
                    con,
                    rows[0]["occurrence_id"],
                    audit_sha256=baseline_sha,
                    results_policy_fingerprint=baseline_policy,
                ):
                    raise LedgerError(
                        "selected case does not map to exactly one retained target occurrence "
                        "with this baseline digest and results policy"
                    )
                case = rows[0]
                if case["current_state"] != "recheck_pending":
                    raise LedgerError("selected case must be recheck_pending before execution")
                if str(case["issue_ordinal"]) in bindings:
                    raise LedgerError("selected cases map to the same baseline finding ordinal")
                bindings[str(case["issue_ordinal"])] = (
                    case["occurrence_key"],
                    case["check_key"],
                    case["subject_value"],
                )
        verified = verify_fixes(
            baseline=baseline_source,
            finding_ids=list(bindings),
            after=after,
            config=config,
            out_dir=out_dir,
        )
    finally:
        if hasattr(baseline_source, "close"):
            baseline_source.close()
    keys_in_result = []
    for finding in verified["findings"]:
        binding = bindings.get(str(finding.get("finding_id")))
        if (
            binding is None
            or finding.get("check") != binding[1]
            or not isinstance(finding.get("url"), str)
            or canonical_url(finding["url"]) != binding[2]
        ):
            raise LedgerError(
                "guarded verifier returned a finding outside the selected ledger cases"
            )
        keys_in_result.append(binding[0])
    recorded = record_verification(
        ledger,
        verified["verification"],
        actor=actor,
        expected_revision=expected_revision,
        occurrence_keys=keys_in_result,
        task_id=task_id,
    )
    return {"ok": True, "verification": verified, "recording": recorded}


def remediation_report(
    ledger: str, out_dir: str | None = None, limit: int = 100, offset: int = 0
) -> dict[str, Any]:
    """Render retained remediation evidence, optionally to a new local directory."""
    from seohead.storage.ledger import remediation_report as build
    from seohead.storage.ledger import write_remediation_report

    return (
        write_remediation_report(ledger, out_dir, limit=limit, offset=offset)
        if out_dir
        else build(ledger, limit=limit, offset=offset)
    )


def _record_project_action(
    directory: str,
    *,
    action: str,
    outcome: str,
    detail: str = "",
    proposal_id: str | None = None,
) -> None:
    from seohead.projects.action_log import record_interface_action

    try:
        record_interface_action(
            directory,
            action=action,
            outcome=outcome,
            detail=detail,
            proposal_id=proposal_id,
        )
    except (OSError, ValueError):
        return None


def project_inbox_submit(
    directory: str,
    text: str,
    kind: str = "note",
    references: list[str] | None = None,
    author_role: str = "specialist",
    expected_revision: int | None = None,
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_inbox_submit as core

    result = core(
        directory,
        text=text,
        kind=kind,
        references=references,
        author_role=author_role,
        expected_revision=expected_revision,
    )
    _record_project_action(
        directory,
        action="project.inbox.submit",
        outcome="pending" if kind == "proposed_goal" else "applied",
        detail=f"Recorded project inbox {kind}",
    )
    return result


def project_inbox_list(
    directory: str,
    consumer: str,
    offset: int = 0,
    limit: int = 20,
    include_acknowledged: bool = True,
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_inbox_list as core

    return core(
        directory,
        consumer=consumer,
        offset=offset,
        limit=limit,
        include_acknowledged=include_acknowledged,
    )


def project_inbox_read(
    directory: str, consumer: str, entry_ids: list[str], expected_revision: int | None = None
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_inbox_read as core

    return core(
        directory, consumer=consumer, entry_ids=entry_ids, expected_revision=expected_revision
    )


def project_inbox_acknowledge(
    directory: str, consumer: str, entry_ids: list[str], expected_revision: int | None = None
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_inbox_acknowledge as core

    return core(
        directory, consumer=consumer, entry_ids=entry_ids, expected_revision=expected_revision
    )


def project_inbox_goal(
    directory: str, entry_id: str, state: str, expected_revision: int | None = None
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_inbox_goal as core

    result = core(directory, entry_id=entry_id, state=state, expected_revision=expected_revision)
    _record_project_action(
        directory,
        action="project.goal." + state,
        outcome="applied",
        detail=f"Updated owner goal state to {state}",
    )
    return result


def project_inbox_triage(
    directory: str,
    entry_id: str,
    outcome: dict[str, Any],
    actor: str,
    expected_revision: int | None = None,
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_inbox_triage as core

    return core(
        directory,
        entry_id=entry_id,
        outcome=outcome,
        actor=actor,
        expected_revision=expected_revision,
    )


def project_inbox_unread(directory: str, consumer: str, limit: int = 10) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_inbox_unread as core

    return core(directory, consumer=consumer, limit=limit)


def project_event_append(directory: str, source: str, actor: str, text: str) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_event_append as core

    return core(directory, source=source, actor=actor, text=text)


def project_event_page(
    directory: str,
    offset: int = 0,
    limit: int = 50,
    source: str | None = None,
    query: str = "",
    since_revision: int | None = None,
    follow: bool = False,
    wait_seconds: int = 0,
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_event_page as core

    return core(
        directory,
        offset=offset,
        limit=limit,
        source=source,
        query=query,
        since_revision=since_revision,
        follow=follow,
        wait_seconds=wait_seconds,
    )


def project_observe(
    directory: str,
    consumer: str | None = None,
    scan_limit: int = 20,
    run_offset: int = 0,
    run_limit: int = 20,
    since_revision: str | None = None,
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_observe as core

    return core(
        directory,
        consumer=consumer,
        scan_limit=scan_limit,
        run_offset=run_offset,
        run_limit=run_limit,
        since_revision=since_revision,
    )


def project_facts(
    directory: str,
    facts: list[dict[str, Any]] | None = None,
    detect: bool = False,
    apply: bool = False,
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_facts as core

    # Detection reuses the shared single-page tools rather than a second HTTP path,
    # so it inherits their pinning transport and their failure reporting.
    return core(directory, facts=facts, detect=detect, apply=apply, tools=HANDLERS)


def project_checklist_init(
    directory: str,
    template: dict | None = None,
    expected_revision: int | None = None,
    plan: dict | None = None,
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_checklist_init as core

    return core(directory, template=template, expected_revision=expected_revision, plan=plan)


def project_checklist_update(directory: str, item: dict, expected_revision: int) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_checklist_update as core

    result = core(directory, item=item, expected_revision=expected_revision)
    _record_project_action(
        directory,
        action="project.checklist.update",
        outcome="applied",
        detail="Updated checklist item definition",
    )
    return result


def project_checklist_record(
    directory: str, item_id: str, record: dict, expected_revision: int
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_checklist_record as core

    result = core(directory, item_id=item_id, record=record, expected_revision=expected_revision)
    action = (
        "project.checklist.signoff"
        if record.get("signoff") is True
        else "project.checklist.review"
        if record.get("review") == "approved"
        else "project.checklist.record"
    )
    _record_project_action(
        directory,
        action=action,
        outcome="applied",
        detail=f"Recorded checklist item {item_id} as {record.get('status', 'unknown')}",
    )
    return result


def project_view_list(directory: str) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_view_list as core

    return core(directory)


def project_view_show(directory: str, name: str) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_view_show as core

    return core(directory, name)


def project_view_save(directory: str, view: dict, expected_revision: int) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_view_save as core

    return core(directory, view, expected_revision)


def project_view_delete(directory: str, name: str, expected_revision: int) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_view_delete as core

    return core(directory, name, expected_revision)


def project_view_rename(
    directory: str, name: str, new_name: str, expected_revision: int
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_view_rename as core

    return core(directory, name, new_name, expected_revision)


def findings_view(directory: str, name: str, audit: Any, offset: int = 0) -> dict[str, Any]:
    from seohead.projects.finding_views import apply_view_to_audit
    from seohead.storage.inputs import resolve_audit_input

    document, diagnostics = resolve_audit_input(audit)
    result = apply_view_to_audit(directory, name, document, offset=offset)
    result["input_diagnostics"] = diagnostics
    return result


def project_priorities(
    directory: str,
    policy: dict | None = None,
    apply: bool = False,
    expected_revision: int | None = None,
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_priorities as core

    return core(directory, policy=policy, apply=apply, expected_revision=expected_revision)


def project_policy(
    directory: str,
    policy: dict | None = None,
    apply: bool = False,
    expected_revision: int | None = None,
) -> dict[str, Any]:
    from seohead.projects.runtime import project_policy as core
    from seohead.projects.runtime import project_write_lock
    from seohead.projects.workspace import _load

    root, _project = _load(directory)
    with project_write_lock(root):
        if not apply:
            return core(directory, policy=policy, apply=False, expected_revision=expected_revision)
        import uuid

        from seohead.projects.action_log import record as record_action
        from seohead.projects.action_log import resolve_pending

        operation_id = uuid.uuid4().hex
        pending = record_action(
            root,
            client="cli",
            action="project.policy.apply",
            outcome="pending",
            detail="Crawl policy update pending; terminal result will replace this row.",
            operation_id=operation_id,
        )["action"]
        try:
            result = core(
                directory,
                policy=policy,
                apply=True,
                expected_revision=expected_revision,
                operation_id=operation_id,
            )
        except (OSError, ValueError) as exc:
            resolve_pending(
                root,
                pending["sequence"],
                outcome="stale" if "revision" in str(exc).lower() else "refused",
                detail="Crawl policy update did not apply.",
                operation_id=operation_id,
            )
            raise
        try:
            resolve_pending(
                root,
                pending["sequence"],
                outcome="applied",
                detail=f"Applied crawl policy revision {result['revision']}.",
                operation_id=operation_id,
            )
        except (OSError, ValueError):
            result["audit_warning"] = "Policy was saved; its action row remains pending."
        return result


def project_prepare(
    directory: str,
    template: dict | None = None,
    competitors: list | None = None,
    approve_large_crawl: bool = False,
    producer_build: str | None = None,
    max_urls: int | None = None,
) -> dict[str, Any]:
    from seohead.projects.runtime import prepare_project

    return prepare_project(
        directory,
        tools=HANDLERS,
        template=template,
        competitors=competitors,
        approve_large_crawl=approve_large_crawl,
        producer_build=producer_build,
        max_urls=max_urls,
    )


def project_competitors_add(directory: str, competitors: list) -> dict[str, Any]:
    from seohead.projects.runtime import add_competitors

    return add_competitors(directory, competitors)


def project_competitors_list(directory: str) -> dict[str, Any]:
    from seohead.projects.runtime import list_competitors

    return list_competitors(directory)


def project_competitors_remove(directory: str, url: str) -> dict[str, Any]:
    from seohead.projects.runtime import remove_competitor

    return remove_competitor(directory, url)


def project_competitors_scan(
    directory: str,
    task_id: str,
    expected_revision: int,
    confirm: bool,
    concurrency: int = 3,
    approve_large_crawl: bool = False,
    producer_build: str | None = None,
    run_id: str | None = None,
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_competitors_scan as core

    return core(
        directory=directory,
        task_id=task_id,
        expected_revision=expected_revision,
        confirm=confirm,
        concurrency=concurrency,
        approve_large_crawl=approve_large_crawl,
        producer_build=producer_build,
        run_id=run_id,
    )


def project_competitors_scan_status(directory: str, run_id: str) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_competitors_scan_status as core

    return core(directory=directory, run_id=run_id)


def project_competitors_scan_cancel(directory: str, run_id: str) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_competitors_scan_cancel as core

    return core(directory=directory, run_id=run_id)


def project_export(
    directory: str,
    dataset: str,
    fmt: str,
    output: str,
    compare_manifest: str | None = None,
    job_id: str | None = None,
    wait: bool = True,
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_export_start as core

    return core(
        directory=directory,
        dataset=dataset,
        fmt=fmt,
        output=output,
        compare_manifest=compare_manifest,
        job_id=job_id,
        wait=wait,
    )


def project_export_status(directory: str, job_id: str) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_export_status as core

    return core(directory=directory, job_id=job_id)


def project_export_cancel(directory: str, job_id: str) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_export_cancel as core

    return core(directory=directory, job_id=job_id)


def project_exports_page(directory: str, offset: int = 0, limit: int = 50) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_exports_page as core

    return core(directory=directory, offset=offset, limit=limit)


def project_start(
    directory: str,
    target: str,
    facts: list[dict[str, Any]] | None = None,
    template: dict | None = None,
    competitors: list | None = None,
    approve_large_crawl: bool = False,
    producer_build: str | None = None,
) -> dict[str, Any]:
    from seohead.projects.workspace import create_project

    created = create_project(directory, target, facts=facts)
    try:
        return project_prepare(
            directory,
            template=template,
            competitors=competitors,
            approve_large_crawl=approve_large_crawl,
            producer_build=producer_build,
        )
    except (ValueError, OSError) as exc:
        return {
            "ok": False,
            "error": str(exc),
            "project": created,
            "next": "Use project-prepare to continue the inspectable project",
        }


def skill_list() -> dict[str, Any]:
    from seohead.projects.runtime import playbook_list

    return playbook_list("skill")


def skill_show(name: str) -> dict[str, Any]:
    from seohead.projects.runtime import playbook_show

    return playbook_show(name, "skill")


def scenario_list() -> dict[str, Any]:
    from seohead.projects.runtime import playbook_list

    return playbook_list("scenario")


def scenario_show(name: str) -> dict[str, Any]:
    from seohead.projects.runtime import playbook_show

    return playbook_show(name, "scenario")


def provider_replay(
    input_path: str,
    evidence_file: str,
    out_dir: str,
    url_column: str = "url",
    review_external_only: bool = False,
) -> dict[str, Any]:
    from seohead.data_sources.providers import provider_replay as core

    return core(
        input_path,
        evidence_file,
        out_dir,
        url_column=url_column,
        review_external_only=review_external_only,
    )


def provider_auth(
    provider: str,
    action: str = "status",
    grant_file: str | None = None,
    confirm: bool = False,
    state: str | None = None,
    code: str | None = None,
) -> dict[str, Any]:
    from seohead.data_sources.oauth import manage_grant

    return manage_grant(provider, action, grant_file, confirm, state=state, code=code)


def provider_registry() -> dict[str, Any]:
    from seohead.mcp.provider_handlers import provider_registry as core

    return core()


def provider_readiness(provider: str | None = None, operation: str | None = None) -> dict[str, Any]:
    from seohead.mcp.provider_handlers import provider_readiness as core

    return core(provider=provider, operation=operation)


def provider_verify(provider: str, request: dict[str, Any] | None = None) -> dict[str, Any]:
    from seohead.mcp.provider_handlers import provider_verify as core

    return core(provider, request)


def provider_credential(
    provider: str,
    component: str,
    action: str = "status",
    confirm: bool = False,
    value: str | None = None,
    replace: bool = False,
) -> dict[str, Any]:
    from seohead.mcp.provider_handlers import provider_credential as core

    return core(provider, component, action, confirm, value, replace)


def provider_discover(provider: str, domain: str | None = None) -> dict[str, Any]:
    from seohead.mcp.provider_handlers import provider_discover as core

    return core(provider, domain)


def provider_collect(
    provider: str, operation: str, request: dict[str, Any], artifact_dir: str | None = None
) -> dict[str, Any]:
    from seohead.mcp.provider_handlers import provider_collect as core

    return core(provider, operation, request, artifact_dir=artifact_dir)


def provider_artifact_read(
    artifact_ref: str,
    artifact_dir: str,
    offset: int = 0,
    limit: int = 25,
) -> dict[str, Any]:
    """Read a bounded, allowlisted PageSpeed summary from a caller-owned local artifact dir."""
    from seohead.data_sources.provider_artifacts import read_pagespeed_artifact

    return read_pagespeed_artifact(artifact_ref, artifact_dir, offset=offset, limit=limit)


def provider_join(
    crawl_pages: list[dict[str, Any]],
    evidence_rows: list[dict[str, Any]],
    review_external_only: bool = False,
    adjustments: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    from seohead.mcp.provider_handlers import provider_join as core

    return core(
        crawl_pages,
        evidence_rows,
        review_external_only=review_external_only,
        adjustments=adjustments,
    )


def _json_or_path(value: Any, label: str) -> Any:
    """Resolve one argument that accepts inline JSON text or a bounded JSON file path."""
    import json
    from pathlib import Path

    if value is None or isinstance(value, (dict, list)):
        return value
    if not isinstance(value, str):
        raise ValueError(f"{label} must be JSON data or a file path")
    text = value.strip()
    if text[:1] in {"{", "["}:
        try:
            return json.loads(text)
        except json.JSONDecodeError as exc:
            raise ValueError(f"{label} is not valid JSON: {exc}") from exc
    path = Path(text)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 4 * 1024 * 1024:
        raise ValueError(f"{label} must be inline JSON or a bounded readable JSON file")
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise ValueError(f"{label} is not valid JSON: {exc}") from exc


def _evidence_document(
    value: Any, *, mapping: Any = None, sheet: str | None = None, site_origin: str | None = None
) -> dict[str, Any]:
    """Resolve one evidence input into a normalized evidence document."""
    from seohead.data_sources import evidence_import

    if value is None:
        raise ValueError("evidence source required")
    manifest = _json_or_path(mapping, "mapping")
    if isinstance(value, dict):
        if value.get("format") == evidence_import.NORMALIZED_FORMAT:
            return value
        if isinstance(value.get("rows"), list):
            return evidence_import.normalize_inline(
                value["rows"], manifest=manifest or value.get("mapping")
            )
        raise ValueError("evidence must be a normalized document, a rows object, or a file path")
    if isinstance(value, str):
        text = value.strip()
        if text[:1] in {"{", "["}:
            import json

            return _evidence_document(
                json.loads(text), mapping=mapping, sheet=sheet, site_origin=site_origin
            )
        return evidence_import.normalize_file(
            value, manifest=manifest, sheet=sheet, site_origin=site_origin
        )
    raise ValueError("evidence must be a normalized document, a rows object, or a file path")


def evidence_normalize(
    file: str | None = None,
    mapping: Any = None,
    sheet: str | None = None,
    site_origin: str | None = None,
    out_dir: str | None = None,
) -> dict[str, Any]:
    """Normalize one supplied CSV/XLSX/JSON or saved provider envelope, fully offline.

    Restricted sources (saved provider evidence, or a manifest that declares
    ``privacy: restricted``) report summary and redacted provenance only; their
    normalized rows exist solely inside the optional restricted ``out_dir``
    artifact. Supplied files return the full normalized document.
    """
    if not file:
        raise ValueError("file required")
    from seohead.data_sources import evidence_import
    from seohead.data_sources.providers import _save_local_artifact

    manifest = _json_or_path(mapping, "mapping")
    document = evidence_import.normalize_file(
        file, manifest=manifest, sheet=sheet, site_origin=site_origin
    )
    privacy = (document["mapping"].get("source") or {}).get("privacy") or "supplied"
    artifact = _save_local_artifact(out_dir, document) if out_dir else None
    result: dict[str, Any] = {
        "ok": True,
        "format": "seohead.evidence-normalize.v1",
        "privacy": privacy,
        "summary": document["summary"],
        "artifact_reference": artifact,
    }
    if privacy == "restricted":
        result["provenance"] = evidence_import.public_provenance(document["provenance"])
        result["rows_redacted"] = True
    else:
        result["document"] = document
    return result


def evidence_join(
    audit: Any = None,
    scan: str | None = None,
    pages: Any = None,
    evidence: Any = None,
    compare: Any = None,
    mapping: Any = None,
    compare_mapping: Any = None,
    policy: Any = None,
    sheet: str | None = None,
    compare_sheet: str | None = None,
    site_origin: str | None = None,
    compare_site_origin: str | None = None,
    ignore_query: bool = False,
    ignore_scheme: bool = False,
    casefold_path: bool = False,
    out_dir: str | None = None,
) -> dict[str, Any]:
    """Join normalized analytics/search evidence to crawl pages, offline only.

    One of ``pages``, ``scan`` or ``audit`` supplies the crawl side; ``evidence``
    is a file or normalized document normalized through the shared
    ``seohead.evidence-mapping.v1`` path. ``compare`` adds a pure
    compatibility decision against a second source under a declared
    ``policy``. Restricted sources keep every population inside the optional
    private ``out_dir`` artifact and return counts, never row content.
    """
    from seohead.data_sources import evidence_join as join_core
    from seohead.data_sources.providers import _save_local_artifact

    document = _evidence_document(evidence, mapping=mapping, sheet=sheet, site_origin=site_origin)
    compare_document = (
        _evidence_document(
            compare,
            mapping=compare_mapping,
            sheet=compare_sheet,
            site_origin=compare_site_origin,
        )
        if compare is not None
        else None
    )
    if sum(source is not None for source in (pages, scan, audit)) > 1:
        raise ValueError("pages, scan and audit are alternative crawl inputs, not a set")
    page_rows = None
    large_scan = False
    crawl_context: dict[str, Any] = {}
    if pages is not None:
        page_rows = _json_or_path(pages, "pages")
        if (
            not isinstance(page_rows, list)
            or len(page_rows) > 100_000
            or any(not isinstance(page, dict) for page in page_rows)
        ):
            raise ValueError("pages must be a bounded list of page objects")
        crawl_context = {"source": "pages"}
    elif scan:
        from seohead.storage import open_scan

        con = open_scan(scan, require_audit=False)
        try:
            large_scan = (
                con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]
                > LARGE_EVIDENCE_JOIN_SCAN_PAGES
            )
            if not large_scan:
                page_rows = [
                    dict(row)
                    for row in con.execute(
                        "SELECT u.url,p.status_code FROM pages p JOIN urls u USING(url_id)"
                        " ORDER BY p.url_id"
                    )
                ]
            scan_uuid = con.execute("SELECT scan_uuid FROM scan").fetchone()[0]
            partial = None
            audit_row = con.execute("SELECT document_json FROM audit WHERE singleton=1").fetchone()
            if audit_row:
                import json as _json

                try:
                    partial = bool(
                        (_json.loads(audit_row[0]).get("run") or {}).get("crawl_partial")
                    )
                except (TypeError, ValueError):
                    partial = None
        finally:
            con.close()
        crawl_context = {"source": "scan", "scan_uuid": scan_uuid, "partial": partial}
    elif audit is not None:
        diagnostics: list[dict[str, str]] = []
        audit_document = _load_audit(audit, "audit", diagnostics)
        page_rows = audit_document.get("pages") or []
        if len(page_rows) > 100_000:
            raise ValueError("audit pages exceed the bounded join limit")
        crawl_context = {
            "source": "audit",
            "partial": bool((audit_document.get("run") or {}).get("crawl_partial")),
        }
        if diagnostics:
            crawl_context["input_diagnostics"] = diagnostics
    if page_rows is None and not large_scan and compare_document is None:
        raise ValueError(
            "evidence_join needs a crawl input (pages, scan or audit) to join, "
            "or a compare source for a compatibility-only decision"
        )
    url_policy = {
        "ignore_query": bool(ignore_query),
        "ignore_scheme": bool(ignore_scheme),
        "casefold_path": bool(casefold_path),
    }
    join_result = None
    store_result = None
    if large_scan:
        if not out_dir:
            raise ValueError(
                f"a scan over {LARGE_EVIDENCE_JOIN_SCAN_PAGES} pages requires --out-dir "
                "for its durable evidence-join store"
            )
        import hashlib
        import json
        import os

        from seohead.data_sources import evidence_join_store

        root = Path(out_dir)
        if root.is_symlink():
            raise ValueError("private join output directory must not be a symlink")
        root.mkdir(mode=0o700, parents=True, exist_ok=True)
        digest = hashlib.sha256(
            json.dumps(document, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode(
                "utf-8"
            )
        ).hexdigest()
        destination = root / f"evidence-join-{digest[:16]}.sqlite"
        if destination.exists() or destination.is_symlink():
            raise ValueError(
                "durable evidence-join artifact already exists; choose a new output directory"
            )
        store_result = evidence_join_store.write(scan, document, destination, policy=url_policy)
        os.chmod(root, 0o700)
    elif page_rows is not None:
        join_result = join_core.join_evidence(
            page_rows, document, url_policy=url_policy, crawl=crawl_context
        )
    compatibility = (
        join_core.evidence_compatibility(
            document, compare_document, policy=_json_or_path(policy, "policy")
        )
        if compare_document is not None
        else None
    )
    restricted = any(
        ((doc.get("mapping") or {}).get("source") or {}).get("privacy") == "restricted"
        for doc in (document, compare_document)
        if doc is not None
    )
    artifact = (
        f"local-artifact:{store_result['content_sha256']}"
        if store_result is not None
        else _save_local_artifact(out_dir, {"join": join_result, "compatibility": compatibility})
        if out_dir
        else None
    )
    response: dict[str, Any] = {
        "ok": True,
        "format": "seohead.evidence-join-result.v1",
        "privacy": "restricted" if restricted else "supplied",
        "artifact_reference": artifact,
    }
    if compatibility is not None:
        response["compatibility"] = (
            join_core.public_compatibility(compatibility) if restricted else compatibility
        )
    if store_result is not None:
        response["join_store"] = {
            "format": store_result["format"],
            "file_name": Path(store_result["path"]).name,
            "url_policy": store_result["url_policy"],
            "allocation_policy": store_result["allocation_policy"],
            "crawl": store_result["crawl"],
            "summary": store_result["summary"],
            "content_sha256": store_result["content_sha256"],
        }
        response["populations_redacted"] = restricted
    elif restricted:
        response["populations_redacted"] = True
        if join_result is not None:
            response["join"] = {
                "format": join_result["format"],
                "url_policy": join_result["url_policy"],
                "crawl": join_result["crawl"],
                "summary": join_result["summary"],
            }
    elif join_result is not None:
        response["join"] = join_result
    return response


def bi_filter(
    package: str,
    dataset: str,
    out_dir: str,
    where: dict[str, list[str]] | None = None,
    columns: list[str] | None = None,
    max_rows_per_file: int = 250_000,
    max_bytes_per_file: int = 8_388_608,
    max_output_bytes: int = 4_294_967_296,
    xlsx_out: str | None = None,
    xlsx_max_rows_per_sheet: int = 1_048_575,
) -> dict[str, Any]:
    """Filter a verified local BI package into a new typed package and optional XLSX."""
    from seohead.mcp.bi_handlers import bi_filter as core

    return core(
        package=package,
        dataset=dataset,
        out_dir=out_dir,
        where=where,
        columns=columns,
        max_rows_per_file=max_rows_per_file,
        max_bytes_per_file=max_bytes_per_file,
        max_output_bytes=max_output_bytes,
        xlsx_out=xlsx_out,
        xlsx_max_rows_per_sheet=xlsx_max_rows_per_sheet,
    )


def scan_navigation(
    input_path: str, document_id: int | None = None, limit: int = 100, offset: int = 0
) -> dict[str, Any]:
    """Read bounded observed navigation evidence from a retained local scan."""
    from seohead.mcp.navigation_handlers import scan_navigation as core

    return core(input_path=input_path, document_id=document_id, limit=limit, offset=offset)


def project_activity(directory: str, since_revision: str | None = None) -> dict[str, Any]:
    """Read lightweight current activity for a local project and its sites."""
    from seohead.projects.observer import observe_activity as core

    return core(directory=directory, since_revision=since_revision)


def project_checklist_page(
    directory: str,
    offset: int = 0,
    limit: int = 50,
    query: str = "",
    kind: str | None = None,
    state: str | None = None,
    sort: str = "id",
    descending: bool = False,
    states: list[str] | None = None,
) -> dict[str, Any]:
    """Read a bounded searchable page of project checklist evidence."""
    from seohead.projects.observer import checklist_page as core

    return core(
        directory=directory,
        offset=offset,
        limit=limit,
        query=query,
        kind=kind,
        state=state,
        sort=sort,
        descending=descending,
        states=states,
    )


def project_task_detail(directory: str, item_id: str) -> dict[str, Any]:
    """Read one project task definition, evidence and bounded history."""
    from seohead.projects.observer import task_detail as core

    return core(directory=directory, item_id=item_id)


def project_task_comment(
    directory: str,
    item_id: str,
    text: str,
    actor: str,
    expected_revision: int,
    reference: dict[str, str] | None = None,
) -> dict[str, Any]:
    from seohead.mcp.project_handlers import project_task_comment as core

    return core(
        directory=directory,
        item_id=item_id,
        text=text,
        actor=actor,
        expected_revision=expected_revision,
        reference=reference,
    )


def project_scans(directory: str, offset: int = 0, limit: int = 20) -> dict[str, Any]:
    """Read a bounded page of retained project scans with evidence metadata."""
    from seohead.projects.observer import scans_page as core

    return core(directory=directory, offset=offset, limit=limit)


def bi_export(
    scan: str | None = None,
    audit: Any = None,
    provider_joins: list[str] | None = None,
    out_dir: str | None = None,
    max_rows_per_file: int = 25_000,
    max_bytes_per_file: int = 8 * 1024 * 1024,
    max_output_bytes: int = 4 * 1024 * 1024 * 1024,
    max_scan_bytes: int = 8 * 1024 * 1024 * 1024,
    search_metric: str | None = None,
    xlsx_out: str | None = None,
    xlsx_dataset: str | None = None,
    xlsx_max_rows_per_sheet: int = 1_048_575,
) -> dict[str, Any]:
    """Write typed, partitioned BI datasets from saved local crawl evidence."""
    from seohead.reports.bi import export_bi as core
    from seohead.reports.bi_destinations import export_bi_xlsx

    result = core(
        scan=scan,
        audit=audit,
        provider_joins=provider_joins,
        out_dir=out_dir,
        max_rows_per_file=max_rows_per_file,
        max_bytes_per_file=max_bytes_per_file,
        max_output_bytes=max_output_bytes,
        max_scan_bytes=max_scan_bytes,
        search_metric=search_metric,
    )
    if (xlsx_out is None) != (xlsx_dataset is None):
        raise ValueError("xlsx_out and xlsx_dataset must be supplied together")
    xlsx = (
        export_bi_xlsx(
            result["output_directory"],
            dataset=xlsx_dataset,
            out=xlsx_out,
            max_rows_per_sheet=xlsx_max_rows_per_sheet,
        )
        if xlsx_out is not None
        else None
    )
    return {"ok": True, **result, "xlsx": xlsx}


def bi_sheets_plan(package: str, max_cells: int = 10_000_000) -> dict[str, Any]:
    """Preflight a complete BI package for Sheets without credentials or writes."""
    from seohead.reports.bi_destinations import sheets_plan

    return {"ok": True, **sheets_plan(package, max_cells=max_cells)}


def publication_cohorts(
    document: Any = None, file: str | None = None, out_dir: str | None = None
) -> dict[str, Any]:
    """Write an offline publication-cohort package from saved normalized evidence."""
    from seohead.reports.cohorts import publication_cohorts as core

    return {"ok": True, **core(document=document, file=file, out_dir=out_dir)}


def gsc_progress(
    document: Any = None, file: str | None = None, out_dir: str | None = None
) -> dict[str, Any]:
    """Write an offline branded/non-branded GSC progress package."""
    from seohead.reports.cohorts import gsc_progress as core

    return {"ok": True, **core(document=document, file=file, out_dir=out_dir)}


def bi_bigquery_plan(package: str, dataset: str, operation: str = "replace") -> dict[str, Any]:
    """Describe a BigQuery load without a project, credentials, billing, or writes."""
    from seohead.reports.bi_destinations import bigquery_plan

    return {"ok": True, **bigquery_plan(package, dataset=dataset, operation=operation)}


def bi_destination_apply(
    package: str,
    target: str,
    destination: str,
    operation: str,
    apply: bool = False,
    reconcile: bool = False,
    *,
    client: Any = None,
) -> dict[str, Any]:
    """Apply a verified local BI package through an injected authorized destination client."""
    if destination not in {"sheets", "bigquery"}:
        raise ValueError("destination must be 'sheets' or 'bigquery'")
    from seohead.reports.bi_destinations import (
        apply_with_client,
        destination_preview,
        resolve_host_client,
    )

    if reconcile and not apply:
        raise ValueError("reconcile requires apply=true for the reviewed destination target")
    if not apply:
        return {
            "ok": True,
            **destination_preview(
                package, target=target, destination=destination, operation=operation
            ),
        }

    if client is None:
        client = resolve_host_client(destination, target)

    return {
        "ok": True,
        "destination": destination,
        **apply_with_client(
            package,
            target=target,
            operation=operation,
            client=client,
            apply=apply,
            reconcile=reconcile,
        ),
    }


def inspect_url(url: str, checks: list[str] | None = None) -> dict[str, Any]:
    """Run a closed, bounded single-URL investigation using the existing shared tools."""
    chosen = checks if checks is not None else ["metadata", "headers", "robots"]
    operations = {
        "metadata": "parse",
        "headers": "headers_check",
        "robots": "robots_check",
        "redirects": "redirects_check",
        "structured": "schema_check",
        "render": "render_check",
    }
    if (
        not isinstance(chosen, list)
        or not chosen
        or len(chosen) > len(operations)
        or any(type(name) is not str or name not in operations for name in chosen)
    ):
        raise ValueError(
            "checks must be a bounded selection of metadata/headers/robots/redirects/structured/render"
        )
    results = {}
    for name in dict.fromkeys(chosen):
        try:
            results[name] = HANDLERS[operations[name]](url=url)
        except (ValueError, OSError) as exc:
            results[name] = {"ok": False, "reason": str(exc)}
    return {
        "ok": True,
        "url": url,
        "results": results,
        "scope": "one URL; rendering and field/indexing outcomes are not interchangeable",
    }


def audit_workflow(
    directory: str,
    action: str = "status",
    target: str | None = None,
    competitors: list | None = None,
    template: dict | None = None,
    audit: Any = None,
    fmt: str = "md",
    out: str | None = None,
    approve_large_crawl: bool = False,
    max_urls: int | None = None,
) -> dict[str, Any]:
    """Expose a closed project workflow rather than an unrestricted action dispatcher."""
    if action == "status":
        return project_status(directory)
    if action == "start":
        if not target:
            raise ValueError("start requires a target URL")
        return project_start(
            directory,
            target,
            template=template,
            competitors=competitors,
            approve_large_crawl=approve_large_crawl,
        )
    if action == "prepare":
        return project_prepare(
            directory,
            template=template,
            competitors=competitors,
            approve_large_crawl=approve_large_crawl,
            max_urls=max_urls,
        )
    if action == "report":
        from seohead.projects.runtime import latest_project_scan

        source = audit if audit is not None else str(latest_project_scan(directory))
        return report_build(audit=source, fmt=fmt, out=out, project=directory)
    raise ValueError("action must be status, start, prepare, or report")


def tool_catalog(
    query: str = "", limit: int = 10, offset: int = 0, include_arguments: bool = False
) -> dict[str, Any]:
    """Discover source-derived tool metadata without advertising every schema up front."""
    from dataclasses import asdict

    from seohead.mcp.tool_reference import load_seo_tools, load_sf_tools

    if (
        not isinstance(query, str)
        or len(query) > 500
        or type(limit) is not int
        or not 1 <= limit <= 50
        or type(offset) is not int
        or offset < 0
    ):
        raise ValueError(
            "query must be bounded text, limit must be 1..50, and offset must be non-negative"
        )
    words = query.casefold().split()
    matches = []
    from seohead.mcp.tool_reference import load_semantics_tools

    for tool in [*load_seo_tools(), *load_sf_tools(), *load_semantics_tools()]:
        text = (tool.name + " " + tool.summary + " " + tool.notes).casefold()
        if not all(word in text for word in words):
            continue
        row = asdict(tool)
        if not include_arguments:
            row.pop("arguments", None)
            row.pop("notes", None)
        matches.append(row)
    return {
        "ok": True,
        "total": len(matches),
        "items": matches[offset : offset + limit],
        "offset": offset,
        "limit": limit,
        "has_more": offset + limit < len(matches),
        "access": "Use the matching startup profile or full profile for direct low-level calls; high-level workflows invoke their bounded steps internally.",
    }


def url_check_batch(
    urls: list[str] | None = None,
    checks: list[str] | None = None,
    options: dict[str, Any] | None = None,
    scan: str | None = None,
    offset: int = 0,
    max_urls: int = 50,
) -> dict[str, Any]:
    """Run a bounded set of single-URL checks over an explicit list or saved scan page."""
    if bool(urls) == bool(scan):
        raise ValueError("provide exactly one of urls[] or scan")
    page = None
    if scan:
        if (
            type(offset) is not int
            or offset < 0
            or type(max_urls) is not int
            or not 1 <= max_urls <= 50
        ):
            raise ValueError("scan offset must be non-negative and max_urls must be 1..50")
        page = scan_url_query(
            scan, columns=["url"], offset=offset, limit=max_urls, max_bytes=1_048_576
        )
        if not page.get("ok"):
            return {"ok": False, "state": "unavailable", "reason": page.get("error"), "results": []}
        column_index = page["columns"].index("url")
        urls = [row[column_index] for row in page.get("rows", [])]
        if not urls:
            return {
                "ok": False,
                "state": "empty",
                "reason": "the retained scan page contains no URLs",
                "source": "retained_scan",
                "offset": offset,
                "has_more": bool(page.get("has_more")),
                "results": [],
            }
    if not isinstance(urls, list) or not 1 <= len(urls) <= 50:
        raise ValueError("urls must contain 1..50 URLs")
    if any(not isinstance(url, str) or not url.strip() or len(url) > 2048 for url in urls):
        raise ValueError("each URL must be non-empty text no longer than 2048 characters")
    if not isinstance(checks, list) or not 1 <= len(checks) <= 5:
        raise ValueError("checks must contain 1..5 check names")
    supported = {
        "parse": lambda url, opts: parse(url=url, options=opts),
        "robots-check": lambda url, opts: robots_check(
            url=url, user_agent=opts.get("user_agent", "*"), paths=opts.get("paths")
        ),
        "headers-check": lambda url, opts: headers_check(url=url, method=opts.get("method", "GET")),
        "redirects-check": lambda url, opts: redirects_check(url=url, options=opts),
        "schema-check": lambda url, _opts: schema_check(url=url),
        "links-check": lambda url, opts: links_check(
            url=url,
            internal_only=opts.get("internal_only", False),
            limit=min(int(opts.get("limit", 200)), 500),
        ),
        "hreflang-check": lambda url, _opts: hreflang_check(url=url),
        "security-check": lambda url, opts: security_check(url=url, probe_paths=False),
        "ai-bots-check": lambda url, _opts: ai_bots_check(url=url),
        "llms-txt-check": lambda url, opts: llms_txt_check(url=url, brand=opts.get("brand")),
        "citability-check": lambda url, opts: citability_check(
            url=url, content_area=opts.get("content_area")
        ),
        "social-meta-check": lambda url, _opts: social_meta_check(url=url),
        "soft404-check": lambda url, _opts: soft404_check(url=url),
    }
    if len(set(checks)) != len(checks) or any(name not in supported for name in checks):
        raise ValueError("checks must be unique supported single-URL check names")
    if options is not None and not isinstance(options, dict):
        raise ValueError("batch options must be an object")
    normalized = options or {}
    if checks and {"headers-check", "redirects-check"}.intersection(checks):
        method = str(normalized.get("method") or "GET").upper()
        if method not in {"GET", "HEAD"}:
            raise ValueError("batched read-only checks allow only GET or HEAD methods")
    if len(urls) * len(checks) > 50:
        raise ValueError("a batch may contain at most 50 URL/check combinations")
    results = []
    for url in urls:
        for name in checks:
            try:
                result = supported[name](url, normalized)
                items = result.get("results") if isinstance(result, dict) else None
                failed = isinstance(result, dict) and result.get("ok") is False
                if name == "parse" and isinstance(items, list) and items:
                    failed = all(
                        isinstance(item, dict) and item.get("ok") is False for item in items
                    )
                results.append({"url": url, "check": name, "ok": not failed, "result": result})
            except Exception as exc:
                results.append({"url": url, "check": name, "ok": False, "error": str(exc)})
    all_failed = bool(results) and all(row["ok"] is False for row in results)
    return {
        "ok": not all_failed,
        "partial": any(row["ok"] is False for row in results) and not all_failed,
        "source": "retained_scan" if scan else "explicit_url_list",
        "offset": offset if scan else None,
        "has_more": bool(page and page.get("has_more")),
        "scan_state": page.get("state") if page else None,
        "scan_reason": page.get("reason") if page else None,
        "count": len(results),
        "results": results,
    }


def scan_evidence(
    input_path: str, section: str = "capabilities", limit: int = 1000, offset: int = 0
) -> dict[str, Any]:
    from seohead.mcp.evidence_handlers import scan_evidence as core

    return core(input_path, section=section, limit=limit, offset=offset)


def scan_content_search(
    input_path: str,
    query: str,
    out_dir: str,
    scope: str = "raw_html",
    mode: str = "contains",
    representation: str = "static",
    selector: str | None = None,
    case_sensitive: bool = False,
    include_snippets: bool = False,
    *,
    progress: Callable[[int], None] | None = None,
    kind: str = "literal",
    url: str | None = None,
) -> dict[str, Any]:
    """Search one finished retained scan and write a bounded local NDJSON package.

    The scan is read offline and never re-fetched. ``out_dir`` must be a new
    local directory; each serialized record is at most 64 KiB and the package
    is capped at 512 MiB. Records and their fixed-width offset index are staged
    then atomically published. The response contains counts, bytes and paths, while
    ``scan_content_search_page`` reads at most 100 derived records at a time.
    A partial source, unavailable body, or package byte cap publishes explicit
    partial/incomplete coverage, never an all-clear absence claim.
    """
    import hashlib
    import json
    import os
    import shutil
    import tempfile

    from seohead.storage.content_search import MAX_SERIALIZED_RECORD_BYTES, search_scan

    if not isinstance(out_dir, str) or not out_dir:
        raise ValueError("out_dir required: a new local content-search package directory")
    destination = Path(out_dir)
    parent = destination.parent
    if destination.is_symlink() or os.path.lexists(destination):
        raise ValueError("content-search out_dir must be a new non-symlink path")
    if parent.is_symlink() or not parent.is_dir():
        raise ValueError("content-search output parent must be an existing non-symlink directory")
    if not isinstance(progress, Callable | type(None)):
        raise ValueError("content-search progress must be an internal callback or null")
    stage = Path(tempfile.mkdtemp(prefix=f".{destination.name}-", dir=parent))
    count = 0
    record_bytes = 0
    digest = hashlib.sha256()
    package_limit_hit = False
    package_bytes = 0
    try:
        records_path = stage / "records.ndjson"
        index_path = stage / "records.idx"
        with records_path.open("wb") as records, index_path.open("wb") as index:

            def emit(record: dict[str, Any]) -> bool | None:
                nonlocal count, record_bytes, package_limit_hit
                encoded = (
                    json.dumps(record, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
                    + b"\n"
                )
                if len(encoded) > MAX_SERIALIZED_RECORD_BYTES:
                    raise ValueError(
                        "content-search record exceeds the 64 KiB serialized record limit"
                    )
                projected = record_bytes + len(encoded) + (count + 1) * 40
                if projected + MAX_CONTENT_SEARCH_MANIFEST_BYTES > MAX_CONTENT_SEARCH_PACKAGE_BYTES:
                    package_limit_hit = True
                    return False
                index.write(record_bytes.to_bytes(8, "big"))
                index.write(hashlib.sha256(encoded).digest())
                records.write(encoded)
                digest.update(encoded)
                record_bytes += len(encoded)
                count += 1
                if progress is not None:
                    progress(count)
                return None

            summary = search_scan(
                input_path,
                query=query,
                scope=scope,
                mode=mode,
                representations=(representation,),
                selector=selector,
                case_sensitive=case_sensitive,
                include_snippets=include_snippets,
                on_record=emit,
                kind=kind,
                url=url,
            )
        if package_limit_hit:
            summary["search_completed"] = False
            summary["ok"] = False
            summary["absence_confirmed"] = False
            summary["coverage"]["package_output_truncated"] = True
            summary["coverage"]["package_output_limit_bytes"] = MAX_CONTENT_SEARCH_PACKAGE_BYTES
            summary["coverage"]["stream_error"] = "derived package byte limit reached"
            summary["coverage"]["partial_reasons"] = [
                reason
                for reason in summary["coverage"]["partial_reasons"]
                if reason != "record consumer stopped before all selected documents were emitted"
            ]
            summary["coverage"]["partial_reasons"].append(
                "derived package byte limit reached before all selected documents were emitted"
            )
        manifest = {
            "format": "seohead.retained-content-search.v1",
            "search_completed": summary["search_completed"],
            "source": summary["source"],
            "scope": summary["scope"],
            "mode": summary["mode"],
            "kind": summary["kind"],
            "representations": summary["representations"],
            "coverage": summary["coverage"],
            "url_filter": url,
            "absence_confirmed": summary["absence_confirmed"],
            "records": {
                "file": records_path.name,
                "index": index_path.name,
                "count": count,
                "bytes": record_bytes,
                "sha256": digest.hexdigest(),
                "index_entry_bytes": 40,
                "record_byte_limit": MAX_SERIALIZED_RECORD_BYTES,
            },
            "package_byte_limit": MAX_CONTENT_SEARCH_PACKAGE_BYTES,
            "snippets": "included" if include_snippets else "omitted",
            "network": False,
        }
        manifest_bytes = (
            json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n"
        ).encode("utf-8")
        if len(manifest_bytes) > MAX_CONTENT_SEARCH_MANIFEST_BYTES:
            raise ValueError("content-search package manifest exceeds 1 MiB")
        package_bytes = record_bytes + count * 40 + len(manifest_bytes)
        if package_bytes > MAX_CONTENT_SEARCH_PACKAGE_BYTES:
            raise ValueError("content-search package exceeds its byte limit")
        (stage / "manifest.json").write_bytes(manifest_bytes)
        if os.path.lexists(destination):
            raise ValueError("content-search out_dir appeared while the package was being written")
        os.replace(stage, destination)
    except BaseException:
        shutil.rmtree(stage, ignore_errors=True)
        raise
    coverage = summary["coverage"]["state"]
    status = (
        "complete"
        if summary["search_completed"] and coverage == "complete"
        else ("partial" if summary["search_completed"] else "incomplete")
    )
    return {
        "ok": True,
        "status": status,
        "format": "seohead.retained-content-search.v1",
        "out_dir": str(destination),
        "manifest": str(destination / "manifest.json"),
        "records": count,
        "package_bytes": package_bytes,
        "source": summary["source"],
        "coverage": summary["coverage"],
        "absence_confirmed": summary["absence_confirmed"],
        "search_completed": summary["search_completed"],
        "capabilities": {"retained_content_search": True},
    }


def custom_extract_sample(
    html: str, extractors: list[dict[str, Any]], url: str = ""
) -> dict[str, Any]:
    """Run declared extractors against supplied HTML; never fetches the URL label."""
    from seohead.checks.custom_extract import check_sample

    return check_sample(html, extractors, url=url)


def scan_content_search_page(
    package: str,
    offset: int = 0,
    limit: int = 100,
    status: str | None = None,
    status_code: int | None = None,
) -> dict[str, Any]:
    """Read no more than 100 indexed derived content-search records without rescanning evidence.

    With ``status`` or ``status_code`` the page is taken from the filtered stream: ``offset`` and
    ``next_offset`` count matching records, and the scan is linear (every record is re-verified).
    """
    import hashlib
    import json
    import os

    from seohead.storage.content_search import MAX_SERIALIZED_RECORD_BYTES

    if type(offset) is not int or offset < 0 or type(limit) is not int or not 1 <= limit <= 100:
        raise ValueError("offset must be nonnegative and limit must be 1..100")
    if status is not None and status not in {"matched", "not_matched", "unavailable"}:
        raise ValueError("status must be matched, not_matched, unavailable, or omitted")
    if status_code is not None and (type(status_code) is not int or not 100 <= status_code <= 599):
        raise ValueError("status_code must be an integer 100..599 or omitted")
    filtered = status is not None or status_code is not None
    root = Path(package)
    manifest_path = root / "manifest.json"
    if (
        root.is_symlink()
        or not root.is_dir()
        or manifest_path.is_symlink()
        or not manifest_path.is_file()
    ):
        raise ValueError("content-search package must be a regular local package directory")
    if manifest_path.stat().st_size > MAX_CONTENT_SEARCH_MANIFEST_BYTES:
        raise ValueError("content-search package manifest exceeds 1 MiB")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValueError("content-search package manifest is invalid") from exc
    records = manifest.get("records") if isinstance(manifest, dict) else None
    if (
        manifest.get("format") != "seohead.retained-content-search.v1"
        or not isinstance(records, dict)
        or type(records.get("count")) is not int
        or records["count"] < 0
        or type(records.get("bytes")) is not int
        or records["bytes"] < 0
        or not isinstance(records.get("file"), str)
        or not isinstance(records.get("index"), str)
        or records.get("index_entry_bytes") != 40
        or records.get("record_byte_limit", MAX_SERIALIZED_RECORD_BYTES)
        != MAX_SERIALIZED_RECORD_BYTES
        or Path(records["file"]).name != records["file"]
        or Path(records["index"]).name != records["index"]
    ):
        raise ValueError("content-search package manifest is invalid")
    records_path = root / records["file"]
    index_path = root / records["index"]
    if any(path.is_symlink() or not path.is_file() for path in (records_path, index_path)):
        raise ValueError("content-search package records are unavailable")
    if os.path.getsize(index_path) != records["count"] * records[
        "index_entry_bytes"
    ] or os.path.getsize(records_path) != records.get("bytes"):
        raise ValueError("content-search package index length is invalid")
    package_bytes = (
        manifest_path.stat().st_size + os.path.getsize(index_path) + os.path.getsize(records_path)
    )
    if package_bytes > MAX_CONTENT_SEARCH_PACKAGE_BYTES:
        raise ValueError("content-search package exceeds its byte limit")
    source = manifest.get("source")
    if not isinstance(source, dict):
        raise ValueError("content-search package source identity is invalid")
    if records["count"] and (
        type(source.get("scan_uuid")) is not str or type(source.get("evidence_revision")) is not int
    ):
        raise ValueError("content-search package source identity is invalid")

    def checked_row(line: bytes, digest: bytes) -> dict[str, Any]:
        if len(line) > MAX_SERIALIZED_RECORD_BYTES or not line.endswith(b"\n"):
            raise ValueError("content-search package record is truncated or exceeds 64 KiB")
        if hashlib.sha256(line).digest() != digest:
            raise ValueError("content-search package record integrity is invalid")
        try:
            row = json.loads(line)
        except json.JSONDecodeError as exc:
            raise ValueError("content-search package record is invalid") from exc
        if not isinstance(row, dict):
            raise ValueError("content-search package record is invalid")
        if row.get("scan_uuid") != source.get("scan_uuid") or row.get(
            "evidence_revision"
        ) != source.get("evidence_revision"):
            raise ValueError("content-search package record source identity disagrees")
        return row

    if filtered:
        return _content_search_filtered_page(
            manifest=manifest,
            records=records,
            index_path=index_path,
            records_path=records_path,
            offset=offset,
            limit=limit,
            status=status,
            status_code=status_code,
            checked_row=checked_row,
        )
    if offset >= records["count"]:
        return {
            "ok": True,
            "format": manifest["format"],
            "source": source,
            "offset": offset,
            "records": [],
            "has_more": False,
            "next_offset": offset,
        }
    rows: list[dict[str, Any]] = []
    with index_path.open("rb") as index, records_path.open("rb") as stream:
        index.seek(offset * records["index_entry_bytes"])
        marker = index.read(records["index_entry_bytes"])
        if len(marker) != records["index_entry_bytes"]:
            raise ValueError("content-search package index is truncated")
        stream.seek(int.from_bytes(marker[:8], "big"))
        for _ in range(limit):
            line = stream.readline(MAX_SERIALIZED_RECORD_BYTES + 1)
            if not line:
                raise ValueError("content-search package records are truncated")
            entry = offset + len(rows)
            index.seek(entry * records["index_entry_bytes"] + 8)
            expected = index.read(32)
            if len(expected) != 32:
                raise ValueError("content-search package index is truncated")
            rows.append(checked_row(line, expected))
            if offset + len(rows) >= records["count"]:
                break
    next_offset = offset + len(rows)
    return {
        "ok": True,
        "format": manifest["format"],
        "source": source,
        "offset": offset,
        "records": rows,
        "has_more": next_offset < records["count"],
        "next_offset": next_offset,
    }


def _content_search_filtered_page(
    *,
    manifest: dict[str, Any],
    records: dict[str, Any],
    index_path: Path,
    records_path: Path,
    offset: int,
    limit: int,
    status: str | None,
    status_code: int | None,
    checked_row: Any,
) -> dict[str, Any]:
    from seohead.storage.content_search import MAX_SERIALIZED_RECORD_BYTES

    # ponytail: linear pass over every record, no matched index | ceiling: ~1M records per page read | upgrade: matched.idx offsets (issue #939 slice 2)
    def matches(row: dict[str, Any]) -> bool:
        return (status is None or row.get("status") == status) and (
            status_code is None or row.get("status_code") == status_code
        )

    rows: list[dict[str, Any]] = []
    seen = 0
    has_more = False
    entry_bytes = records["index_entry_bytes"]
    with index_path.open("rb") as index, records_path.open("rb") as stream:
        for _ in range(records["count"]):
            marker = index.read(entry_bytes)
            if len(marker) != entry_bytes:
                raise ValueError("content-search package index is truncated")
            stream.seek(int.from_bytes(marker[:8], "big"))
            line = stream.readline(MAX_SERIALIZED_RECORD_BYTES + 1)
            if not line:
                raise ValueError("content-search package records are truncated")
            row = checked_row(line, marker[8:40])
            if not matches(row):
                continue
            if seen < offset:
                seen += 1
                continue
            if len(rows) == limit:
                has_more = True
                break
            rows.append(row)
            seen += 1
    return {
        "ok": True,
        "format": manifest["format"],
        "source": manifest["source"],
        "filtered": True,
        "filter": {"status": status, "status_code": status_code},
        "offset": offset,
        "records": rows,
        "has_more": has_more,
        "next_offset": offset + len(rows),
    }


def scan_extract(
    input_path: str,
    rules: list[dict[str, Any]],
    url: str | None = None,
    representation: str = "static",
    limit: int = 100,
) -> dict[str, Any]:
    from seohead.mcp.evidence_handlers import scan_extract as core

    return core(input_path, rules, url=url, representation=representation, limit=limit)


def scan_structured_blocks(
    input_path: str,
    url: str,
    representation: str = "static",
    variant_key: str | None = None,
) -> dict[str, Any]:
    from seohead.mcp.evidence_handlers import scan_structured_blocks as core

    return core(input_path, url, representation=representation, variant_key=variant_key)


def marketing_inventory(
    documents: list[dict[str, Any]],
    cta_selector: str | None = None,
    form_selector: str | None = None,
    id_attributes: list[str] | None = None,
    id_parameters: list[str] | None = None,
    out_dir: str | None = None,
) -> dict[str, Any]:
    """Correlate CTA/form fields per supplied HTML element without network access."""
    from seohead.checks.marketing_inventory import inventory

    return inventory(
        documents,
        cta_selector=cta_selector,
        form_selector=form_selector,
        id_attributes=id_attributes,
        id_parameters=id_parameters,
        out_dir=out_dir,
    )


def scan_fragment_links(
    input_path: str,
    offset: int = 0,
    limit: int = 100,
    state: str | None = None,
    representation: str | None = None,
) -> dict[str, Any]:
    from seohead.mcp.evidence_handlers import scan_fragment_links as core

    return core(input_path, offset=offset, limit=limit, state=state, representation=representation)


def scan_requeue(
    input_path: str, where: str, backup_path: str, from_scan: str | None = None
) -> dict[str, Any]:
    from seohead.mcp.history_handlers import scan_requeue as core

    return core(input_path, where=where, backup_path=backup_path, from_scan=from_scan)


def scan_import_urls(input_path: str, urls_file: str, backup_path: str) -> dict[str, Any]:
    from seohead.mcp.history_handlers import scan_import_urls as core

    return core(input_path, urls_file=urls_file, backup_path=backup_path)


_RAW_HANDLERS = {
    "doctor": doctor,
    "update_check": update_check,
    "update_download": update_download,
    "parse": parse,
    "redirects_generate": redirects_generate,
    "redirects_check": redirects_check,
    "sitemap_crawl": sitemap_crawl,
    "crawl_site": crawl_site,
    "crawl_control": crawl_control,
    "verify_fixes": verify_fixes,
    "crawl_describe_settings": crawl_describe_settings,
    "images_download": images_download,
    "images_optimize": images_optimize,
    "keywords_cluster": keywords_cluster,
    "robots_check": robots_check,
    "headers_check": headers_check,
    "asset_weight_check": asset_weight_check,
    "links_check": links_check,
    "hreflang_check": hreflang_check,
    "domain_profile": domain_profile,
    "cdn_check": cdn_check,
    "tech_detect": tech_detect,
    "security_check": security_check,
    "backlinks_check": backlinks_check,
    "backlinks_profile": backlinks_profile,
    "backlinks_disavow": backlinks_disavow,
    "schema_check": schema_check,
    "schema_build": schema_build,
    "duplicate_check": duplicate_check,
    "ai_bots_check": ai_bots_check,
    "mirror_check": mirror_check,
    "llms_txt_check": llms_txt_check,
    "citability_check": citability_check,
    "markdown_extract": markdown_extract,
    "boilerplate_report": boilerplate_report,
    "semantic_inputs": semantic_inputs,
    "semantic_similarity": semantic_similarity,
    "meta_description_drafts": meta_description_drafts,
    "ai_column": ai_column,
    "log_scan": log_scan,
    "crawl_diagnose": crawl_diagnose,
    "crawl_diagnose_export": crawl_diagnose_export,
    "social_meta_check": social_meta_check,
    "soft404_check": soft404_check,
    "log_analyze": log_analyze,
    "logs_import": logs_import,
    "logs_summary": logs_summary,
    "logs_join": logs_join,
    "logs_bots": logs_bots,
    "regions_check": regions_check,
    "render_check": render_check,
    "site_audit": site_audit,
    "report_build": report_build,
    "facts_export": facts_export,
    "compare_crawls": compare_crawls,
    "compare_read": compare_read,
    "compare_status": compare_status,
    "compare_report": compare_report,
    "compare_url": compare_url,
    "crawl_enrich": crawl_enrich,
    "crawl_import": crawl_import,
    "segment_diff": segment_diff,
    "keywords_expand": keywords_expand,
    "keywords_seasonality": keywords_seasonality,
    "keywords_exact": keywords_exact,
    "serp_fetch": serp_fetch,
    "spend_report": spend_report,
    "sources_doctor": sources_doctor,
    "sources_sync": sources_sync,
    "sources_status": sources_status,
    "sources_export": sources_export,
    "regions_tree": regions_tree,
    "topvisor_read": topvisor_read,
    "metrika_counters": metrika_counters,
    "metrika_setup": metrika_setup,
    "metrika_report": metrika_report,
    "metrika_traffic_pdf": metrika_traffic_pdf,
    "google_keywords": google_keywords,
    "dataforseo_estimate": dataforseo_estimate,
    "google_serp": google_serp,
    "wayback_history": wayback_history,
    "crtsh_subdomains": crtsh_subdomains,
    "cloudflare_traffic": cloudflare_traffic,
    "cloudflare_history": cloudflare_history,
    "cloudflare_logpush_import": cloudflare_logpush_import,
    "cloudflare_instant_logs": cloudflare_instant_logs,
    "gsc_query": gsc_query,
    "webmaster_url_queries": webmaster_url_queries,
    "miratext_analyze": miratext_analyze,
    "gsc_archive": gsc_archive,
    "crux_report": crux_report,
    "indexnow_submit": indexnow_submit,
    "scan_reanalyze": scan_reanalyze,
    "scan_graph": scan_graph,
    "scan_sitemap_generate": scan_sitemap_generate,
    "scan_list": scan_list,
    "scan_inspect": scan_inspect,
    "scan_url_detail": scan_url_detail,
    "scan_body_read": scan_body_read,
    "scan_url_query": scan_url_query,
    "scan_url_counts": scan_url_counts,
    "scan_directory_tree": scan_directory_tree,
    "headers_analyze": headers_analyze,
    "scan_url_history": scan_url_history,
    "scan_link_inspect": scan_link_inspect,
    "scan_status": scan_status,
    "scan_rendered_routes": scan_rendered_routes,
    "scan_evidence": scan_evidence,
    "scan_content_search": scan_content_search,
    "scan_content_search_page": scan_content_search_page,
    "custom_extract_sample": custom_extract_sample,
    "scan_extract": scan_extract,
    "scan_structured_blocks": scan_structured_blocks,
    "marketing_inventory": marketing_inventory,
    "scan_fragment_links": scan_fragment_links,
    "scan_requeue": scan_requeue,
    "scan_import_urls": scan_import_urls,
    "scan_snapshot": scan_snapshot,
    "scan_export": scan_export,
    "scan_pin": scan_pin,
    "scan_prune": scan_prune,
    "scan_body_diff": scan_body_diff,
    "scan_findings_page": scan_findings_page,
    "project_new": project_new,
    "project_open": project_open,
    "project_archive": project_archive,
    "project_restore": project_restore,
    "project_update": project_update,
    "project_delete": project_delete,
    "project_settings": project_settings,
    "project_settings_propose": project_settings_propose,
    "project_settings_proposals": project_settings_proposals,
    "project_settings_review": project_settings_review,
    "project_action_log": project_action_log,
    "project_agent_heartbeat": project_agent_heartbeat,
    "project_status": project_status,
    "project_sources_link": project_sources_link,
    "project_sources_unlink": project_sources_unlink,
    "project_sources_list": project_sources_list,
    "project_progress": project_progress,
    "remediation_summary": remediation_summary,
    "remediation_create": remediation_create,
    "remediation_ingest": remediation_ingest,
    "workflow_start": workflow_start,
    "workflow_checkpoint": workflow_checkpoint,
    "workflow_status": workflow_status,
    "workflow_execute": workflow_execute,
    "workflow_resume": workflow_resume,
    "monitor_configure": monitor_configure,
    "monitor_run": monitor_run,
    "monitor_collect": monitor_collect,
    "monitor_local_deliver": monitor_local_deliver,
    "monitor_status": monitor_status,
    "monitor_schedule": monitor_schedule,
    "schedule_show": schedule_show,
    "schedule_set": schedule_set,
    "schedule_run_due": schedule_run_due,
    "schedule_install": schedule_install,
    "schedule_uninstall": schedule_uninstall,
    "remediation_cases": remediation_cases,
    "remediation_transition": remediation_transition,
    "remediation_record_verification": remediation_record_verification,
    "remediation_recheck": remediation_recheck,
    "remediation_report": remediation_report,
    "project_inbox_submit": project_inbox_submit,
    "project_inbox_list": project_inbox_list,
    "project_inbox_read": project_inbox_read,
    "project_inbox_acknowledge": project_inbox_acknowledge,
    "project_inbox_goal": project_inbox_goal,
    "project_inbox_triage": project_inbox_triage,
    "project_inbox_unread": project_inbox_unread,
    "project_event_append": project_event_append,
    "project_event_page": project_event_page,
    "project_observe": project_observe,
    "project_facts": project_facts,
    "project_checklist_init": project_checklist_init,
    "project_checklist_update": project_checklist_update,
    "project_checklist_record": project_checklist_record,
    "project_priorities": project_priorities,
    "project_view_list": project_view_list,
    "project_view_show": project_view_show,
    "project_view_save": project_view_save,
    "project_view_delete": project_view_delete,
    "project_view_rename": project_view_rename,
    "findings_view": findings_view,
    "inspect_url": inspect_url,
    "audit_workflow": audit_workflow,
    "tool_catalog": tool_catalog,
    "url_check_batch": url_check_batch,
    "project_policy": project_policy,
    "project_prepare": project_prepare,
    "project_competitors_add": project_competitors_add,
    "project_competitors_list": project_competitors_list,
    "project_competitors_remove": project_competitors_remove,
    "project_competitors_scan": project_competitors_scan,
    "project_competitors_scan_status": project_competitors_scan_status,
    "project_competitors_scan_cancel": project_competitors_scan_cancel,
    "project_export": project_export,
    "project_export_status": project_export_status,
    "project_export_cancel": project_export_cancel,
    "project_exports_page": project_exports_page,
    "project_start": project_start,
    "skill_list": skill_list,
    "skill_show": skill_show,
    "scenario_list": scenario_list,
    "scenario_show": scenario_show,
    "provider_replay": provider_replay,
    "provider_auth": provider_auth,
    "provider_registry": provider_registry,
    "provider_readiness": provider_readiness,
    "provider_verify": provider_verify,
    "provider_credential": provider_credential,
    "provider_discover": provider_discover,
    "provider_collect": provider_collect,
    "provider_artifact_read": provider_artifact_read,
    "provider_join": provider_join,
    "evidence_normalize": evidence_normalize,
    "evidence_join": evidence_join,
    "bi_export": bi_export,
    "bi_filter": bi_filter,
    "scan_navigation": scan_navigation,
    "project_activity": project_activity,
    "project_checklist_page": project_checklist_page,
    "project_task_detail": project_task_detail,
    "project_task_comment": project_task_comment,
    "project_scans": project_scans,
    "publication_cohorts": publication_cohorts,
    "gsc_progress": gsc_progress,
    "bi_sheets_plan": bi_sheets_plan,
    "bi_bigquery_plan": bi_bigquery_plan,
    "bi_destination_apply": bi_destination_apply,
}

# Sensitive values that flow only through these local workflows are omitted from journal
# arguments and fingerprints; the generic journal sanitizer remains name-based.
_SENSITIVE_HANDLER_ARGUMENTS = {
    "provider_auth": frozenset({"state", "code"}),
    "provider_credential": frozenset({"value"}),
}

# Journaling sits here rather than in each interface: the CLI and the MCP server
# both dispatch through this mapping, so one wrapper records every call exactly
# once and no future tool can be added without being recorded.
HANDLERS = {
    name: runlog.journaled(
        name, fn, sensitive_arguments=_SENSITIVE_HANDLER_ARGUMENTS.get(name, frozenset())
    )
    for name, fn in _RAW_HANDLERS.items()
}
