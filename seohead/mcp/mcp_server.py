"""Local stdio MCP server exposing the complete SEOHEAD Tools capability set.

One connector provides live URL and content tools, domain and infrastructure reconnaissance,
external demand and traffic providers, report generation, and Screaming Frog crawl-export audits.
The transport is local stdio only: it does not open a network port or expose a hosted endpoint.

Run ``python -m seohead.mcp.mcp_server`` or ``seohead mcp``.
Requires the optional ``mcp`` dependency: ``pip install "seohead-seotools[mcp]"``.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Any, Literal

from pydantic import StrictInt

from seohead.core import runlog
from seohead.core.models import ParseManyResult, RobotsCheckResult
from seohead.mcp import handlers


def _all_parse_results_failed(result: Any) -> bool:
    """``handlers.parse`` returns a ``ParseManyResult`` (``{"count", "results": [...]}``) with no
    top-level ``ok`` key — only each item in ``results`` carries its own. ``handlers.handler_failed``
    only ever looks at the top level, so it never catches this shape. Total failure (every
    requested URL failed) must not read as success; a partial failure (some ok, some not) stays a
    normal result so the per-item errors remain visible to the caller.
    """
    if not isinstance(result, dict):
        return False
    items = result.get("results")
    if not isinstance(items, list) or not items:
        return False
    return all(isinstance(r, dict) and r.get("ok") is False for r in items)


def _checked(result: Any) -> Any:
    """Raise so FastMCP marks the call ``isError`` instead of returning a handler's own-reported
    failure (``ok: False``, see ``handlers.handler_failed``) as a normal success — the same
    distinction the CLI makes with a non-zero exit (docs/USAGE.md). Every ``return handlers.*``
    call below passes through here rather than each ``@mcp.tool`` decorator wrapping its own
    function, because ``tool_reference.py`` reads that decorator's literal shape with `ast` to
    generate docs/TOOL_REFERENCE.md and must keep finding it unchanged.
    """
    from mcp.server.fastmcp.exceptions import ToolError

    if handlers.handler_failed(result) or _all_parse_results_failed(result):
        raise ToolError(json.dumps(result, ensure_ascii=False, default=str))
    return result


def build_server(profile: str = "full", progress_notifications: bool = False):  # -> FastMCP
    runlog.set_interface("mcp")
    from mcp.server.fastmcp import FastMCP
    from mcp.server.fastmcp.exceptions import ToolError
    from mcp.types import ToolAnnotations

    from seohead.mcp import mcp_control
    from seohead.mcp.mcp_profiles import profile_tools

    class ControlledMCP(FastMCP):
        async def call_tool(self, name, arguments):
            try:
                mcp_control.require_enabled()
                allowed = profile_tools(mcp_control.read_state()["profile"])
                if allowed is not None and name not in allowed:
                    raise ValueError("tool is outside the shared MCP profile")
            except (ValueError, OSError) as exc:
                raise ToolError(
                    str(exc) if isinstance(exc, ValueError) else "MCP state unavailable"
                ) from None
            return await super().call_tool(name, arguments)

        async def list_tools(self):
            mcp_control.require_enabled()
            allowed = profile_tools(mcp_control.read_state()["profile"])
            tools = await super().list_tools()
            return tools if allowed is None else [t for t in tools if t.name in allowed]

    mcp = ControlledMCP("SEOHEAD Tools")

    pure = ToolAnnotations(
        readOnlyHint=True, destructiveHint=False, idempotentHint=True, openWorldHint=False
    )
    fetch = ToolAnnotations(
        readOnlyHint=True, destructiveHint=False, idempotentHint=True, openWorldHint=True
    )
    read_files = ToolAnnotations(
        readOnlyHint=True, destructiveHint=False, idempotentHint=True, openWorldHint=False
    )
    create_files = ToolAnnotations(
        readOnlyHint=False, destructiveHint=False, idempotentHint=False, openWorldHint=False
    )
    create_files_from_web = ToolAnnotations(
        readOnlyHint=False, destructiveHint=False, idempotentHint=False, openWorldHint=True
    )
    rewrite_files = ToolAnnotations(
        readOnlyHint=False, destructiveHint=True, idempotentHint=False, openWorldHint=False
    )
    rewrite_files_from_web = ToolAnnotations(
        readOnlyHint=False, destructiveHint=True, idempotentHint=False, openWorldHint=True
    )
    paid = ToolAnnotations(
        readOnlyHint=False, destructiveHint=False, idempotentHint=False, openWorldHint=True
    )
    submit = ToolAnnotations(
        readOnlyHint=False, destructiveHint=False, idempotentHint=True, openWorldHint=True
    )

    host_consumer = os.environ.get("SEOHEAD_MCP_CONSUMER_ID")
    host_projects = {
        str(Path(item).resolve())
        for item in os.environ.get("SEOHEAD_MCP_PROJECT_ALLOWLIST", "").split(os.pathsep)
        if item
    }

    def bound_consumer(directory: str, consumer: str | None) -> str | None:
        """Use only an explicit caller or this stdio process's scoped owner.

        A process binding is opt-in and applies only to the configured project
        allowlist.  It never guesses a peer agent's identity.
        """
        if consumer is not None:
            return consumer
        if not host_consumer or not host_projects:
            return None
        return host_consumer if str(Path(directory).resolve()) in host_projects else None

    def with_project_notice(
        result: dict[str, Any], directory: str, consumer: str | None
    ) -> dict[str, Any]:
        """Attach a scoped notice without letting an unrelated project leak in."""
        consumer = bound_consumer(directory, consumer)
        if consumer is None:
            return result
        result = dict(result)
        result["inbox_unread"] = handlers.project_inbox_unread(directory, consumer)
        return result

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_parse(
        url: str = "", urls: list[str] | None = None, options: dict[str, Any] | None = None
    ) -> ParseManyResult:
        """Parse SEO data (title, meta description, canonical, OG/Twitter, H1-H6,
        JSON-LD, links, visible text, word count) from one URL or a list of URLs."""
        return _checked(handlers.parse(url=url or None, urls=urls, options=options))

    @mcp.tool(annotations=pure, structured_output=True)
    def seo_redirects_generate(
        redirects: list[dict],
        fmt: str = "apache-rewrite-rule",
        default_url: str = "/",
        custom_template: str = "",
    ) -> dict[str, Any]:
        """Generate redirect rules (Apache mod_rewrite/Redirect, Nginx, or a custom
        template) from a list of {old_url, new_url} pairs."""
        return _checked(
            handlers.redirects_generate(
                redirects=redirects,
                fmt=fmt,
                default_url=default_url,
                custom_template=custom_template,
            )
        )

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_redirects_check(url: str, options: dict[str, Any] | None = None) -> dict[str, Any]:
        """Follow a live redirect chain for a URL and report each hop (status, location)."""
        return _checked(handlers.redirects_check(url=url, options=options))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_scan_reanalyze(
        input_path: str,
        out: str,
        producer_build: str | None = None,
        step: Literal["all", "links.position"] = "all",
        max_documents: int | None = None,
    ) -> dict[str, Any]:
        """Reparse retained HTML/DOM and rerun existing checks without network.

        Reads a native or derived SQLite scan and creates a new artifact with
        parent UUID and current analyzer provenance. The source is unchanged;
        an existing output is refused. Missing required bodies produce named
        unavailable errors. Missing live-only context remains skipped. No HTTP,
        DNS, browser, provider, or cache-miss fallback is permitted.
        """
        kwargs = {"input_path": input_path, "out": out, "producer_build": producer_build}
        if step != "all":
            kwargs["step"] = step
        if max_documents is not None:
            kwargs["max_documents"] = max_documents
        return _checked(handlers.scan_reanalyze(**kwargs))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_scan_sitemap_generate(
        input_path: str,
        out_dir: str,
        sitemap_base_url: str,
        include_images: bool = False,
        exclude: list[str] | None = None,
    ) -> dict[str, Any]:
        """Write local XML sitemap files from retained indexable, canonical 200 responses.

        Requires a fresh output directory and a same-origin deployment base URL.
        Splits at 50,000 URLs and 50 MiB uncompressed, writes a sitemap index, and
        optionally includes images parsed from complete retained bodies. lastmod
        uses observed modification evidence, never the capture or generation time.
        Exclusion patterns and skipped evidence are reported. No network or deployment.
        """
        return _checked(
            handlers.scan_sitemap_generate(
                input_path=input_path,
                out_dir=out_dir,
                sitemap_base_url=sitemap_base_url,
                include_images=include_images,
                exclude=exclude,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_graph(
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
        """Read versioned chunks of a retained internal-link graph without network.

        Filter by position or URL section, expand around a URL by depth, aggregate
        sections, or compare two scans. At most 5,000 nodes and 20,000 edges are
        admitted; totals, clipping, coverage and unavailable reasons are explicit.
        Orphan and component counts describe the admitted graph scope.
        """
        return _checked(
            handlers.scan_graph(
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
        )

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_crawl_site(
        url: str = "",
        urls: list[str] | None = None,
        urls_file: str | None = None,
        sitemap: str | None = None,
        sitemap_only: bool = False,
        config: str | None = None,
        max_urls: int | None = None,
        max_depth: int | None = None,
        min_delay: float | None = None,
        robots: str | None = None,
        concurrency: int | None = None,
        out_dir: str | None = None,
        scan_out: str | None = None,
        producer_build: str | None = None,
        overrides: dict[str, Any] | None = None,
        resume: str | None = None,
        project: str | None = None,
        approve_large_crawl: bool = False,
        user_agent: str | None = None,
        observer_run_id: str | None = None,
    ) -> dict[str, Any]:
        """Crawl a site from a start URL by following links, or fetch an explicit
        ``urls`` list instead of following links at all, then audit the result
        through the same checks used for Screaming Frog exports. One of ``url``
        or ``urls``/``urls_file`` is required. Same host only when following links, politeness
        adapts to the origin. Checks whose evidence a native crawl cannot produce
        are reported as skipped, never as clean.

        Pass ``urls`` or a local ``urls_file`` instead of ``url`` for list mode: fetch exactly that set,
        depth 0, no link discovery -- the migration-audit shape (a redirect map,
        a Search Console export). ``urls_file`` scans TXT, CSV, XLSX, or XML for
        absolute HTTP(S) URLs in source order. ``max_depth`` and ``concurrency``
        have nothing to discover in that mode and are ignored.

        ``robots`` is "respect" (obey), "report_only" (fetch robots.txt, crawl
        anyway, and report what a compliant crawler would have missed) or
        "ignore" (do not fetch it at all) -- applied in list mode too, and named
        in the result's ``discovery.directive_policy``, not only enforced
        silently. ``concurrency`` is a per-origin ceiling the adaptive throttle
        grows into, not a fixed thread count. ``sitemap`` seeds the crawl from a
        sitemap's declared URLs in addition to following links from ``url``, and
        reconciles the two sources (declared vs. observed). ``config`` is a path
        to a crawler config file (JSON) on this machine, the same file
        ``crawl-site --config`` reads. ``max_urls``, ``max_depth``, ``min_delay``,
        ``robots`` and ``concurrency`` are left unset by default: an omitted
        override preserves whatever ``config`` (or its own defaults) already
        says, exactly like the CLI's flags -- pass one explicitly only to
        change that one setting. ``seo_crawl_describe_settings`` lists the
        defaults each of them falls back to. Explicit ``max_urls`` admits up to
        1,000,000 URLs; larger values fail before I/O. Request, render, time and
        disk budgets remain independent: use ``limits.max_requests`` in overrides
        for explicit overhead headroom, at most 2,000,000 HTTP attempts.

        ``http.proxy`` in ``config`` or ``overrides`` selects an explicit HTTP
        forward proxy for the entire native crawl, including sitemap, resource
        and pinned browser requests. Credentials require an ``env:VARIABLE``
        URL reference; ``http.proxy_allow_private`` authorizes only a private
        proxy endpoint, not private targets. Ambient proxy variables are ignored.
        Proxied runs require cache off and a fresh artifact; failures never
        fall back to direct egress.

        A URL crawl with neither ``scan_out`` nor ``out_dir`` writes a collision-safe
        SQLite scan below the caller's ``scans/`` directory. ``scan_out`` overrides
        that destination; ``out_dir`` selects the explicit legacy directory route.
        Explicit URL lists can select ``scan_out`` for disk-backed, resumable
        capture in exact input order without link discovery, including multiple hosts.
        Large legacy-directory lists use the same native spool, while directory
        audit materialization remains separately bounded. List capture is static-only;
        rendering and resource fetching are refused. ``cache.mode`` live/replay
        requires the bounded eager ``out_dir`` route. Native scans retain bounded HTML entities and separate
        DOM; their policy records disabled, sensitive, no-store and budget omissions.
        In the config file, ``resources.fetch=true`` opts into direct same-origin
        script/stylesheet capture (20,000 HTTP attempts and 5 MiB per response by
        default). Resource requests share total crawl-time and storage budgets but
        do not consume the page URL limit. CSS imports, JavaScript modules and browser
        network recording are excluded. Offline reanalysis is a separate
        ``seo_scan_reanalyze`` operation over retained evidence; it never contacts
        the network and names unavailable live-only inputs. Audit
        creation has a finite population/output limit and may return unavailable
        while preserving the scan. Supply ``producer_build`` when source provenance
        cannot be determined from a clean checkout.

        ``resume`` names an interrupted ``scan_out`` artifact to continue from its
        stored frontier and throttle state. It is the whole input: the start URL
        and every crawler setting are read back from the file, since they are what
        that frontier was built under, so no other crawl argument may accompany it.
        A scan written by a different producing build, for a different start URL,
        or already finished is refused by name before the first request."""
        result = _checked(
            handlers.crawl_site(
                url=url or None,
                urls=urls,
                urls_file=urls_file,
                sitemap=sitemap,
                sitemap_only=sitemap_only or None,
                config=config,
                max_urls=max_urls,
                max_depth=max_depth,
                min_delay=min_delay,
                robots=robots,
                concurrency=concurrency,
                out_dir=out_dir,
                scan_out=scan_out,
                producer_build=producer_build,
                overrides=overrides,
                resume=resume,
                project=project,
                approve_large_crawl=approve_large_crawl,
                user_agent=user_agent,
                observer_run_id=observer_run_id,
            )
        )
        # The crawler calls its workspace argument ``project`` while observer
        # commands call it ``directory``.  Both identify the same local
        # workspace, so a bound host must receive the same scoped summary.
        if project is not None:
            result = with_project_notice(result, project, None)
        return _checked(result)

    @mcp.tool(annotations=pure, structured_output=True)
    def seo_crawl_describe_settings() -> dict[str, Any]:
        """List every crawl-site config setting: dotted path, type, default value,
        description, and whether it changes what the audit finds (results-affecting)
        or only cost/duration. The same source ``crawl-site --config-help`` reads, so
        an agent can discover the configuration surface without a filesystem."""
        return _checked(handlers.crawl_describe_settings())

    @mcp.tool(annotations=pure, structured_output=True)
    def seo_log_scan(
        run: str, images_dir: str | None = None, max_per_rule: int = 20
    ) -> dict[str, Any]:
        """Report claims a finished run makes that cannot all be true at once: a recorded
        size that disagrees with the file, a check firing more often than there are pages
        to fire on, a finding about a URL the run never fetched, a summary that disagrees
        with its own rows. Not a second audit and not a threshold — only contradictions,
        each naming both values and where each came from, so a surprising number can be
        traced instead of trusted. ``run`` is a native scan path, or a directory holding
        scan.sqlite, audit.json or pages.jsonl. Native scans without retained decision
        logs report those events as unavailable; none are invented. ``images_dir`` is
        an images-download directory whose manifest lets a recorded size be checked
        against the bytes on disk."""
        return _checked(
            handlers.log_scan(run=run, images_dir=images_dir, max_per_rule=max_per_rule)
        )

    @mcp.tool(annotations=pure, structured_output=True)
    def seo_crawl_diagnose(
        scan: str | None = None,
        run: str | None = None,
        max_decisions: int = 20,
    ) -> dict[str, Any]:
        """Explain a small or unfinished native crawl from retained scan or run evidence.
        This MCP tool is read-only and makes no network request. To deliberately write a
        new redacted JSON file, use ``seo_crawl_diagnose_export`` or the CLI's
        ``crawl-diagnose-export --export`` command.
        """
        return _checked(handlers.crawl_diagnose(scan=scan, run=run, max_decisions=max_decisions))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_crawl_diagnose_export(
        export: str,
        scan: str | None = None,
        run: str | None = None,
        max_decisions: int = 20,
    ) -> dict[str, Any]:
        """Create one new redacted crawl-diagnostic JSON file from retained evidence.
        This tool writes a file with no overwrite and makes no network request. Use the
        read-only ``seo_crawl_diagnose`` when a file is not needed.
        """
        return _checked(
            handlers.crawl_diagnose_export(
                export=export, scan=scan, run=run, max_decisions=max_decisions
            )
        )

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_sitemap_crawl(
        url: str, concurrency: int = 3, project: str | None = None
    ) -> dict[str, Any]:
        """Recursively parse a sitemap (index/urlset, gzip supported) into a URL tree.

        Fetches sitemap evidence. An explicit local project records an observation
        for the same origin; otherwise no project files are written. Declared URLs
        are not fetched HTML pages. Counters count parsed sitemap documents only.
        """
        return _checked(handlers.sitemap_crawl(url=url, concurrency=concurrency, project=project))

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_images_download(
        urls: list[str], output_dir: str, options: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        """Download images from a URL list, setting the correct extension by
        content-type and skipping already-downloaded files."""
        return _checked(handlers.images_download(urls=urls, output_dir=output_dir, options=options))

    @mcp.tool(annotations=rewrite_files_from_web, structured_output=True)
    def seo_images_optimize(
        files: list[str] | None = None,
        settings: dict[str, Any] | None = None,
        scan: str | None = None,
        max_images: int = 50,
    ) -> dict[str, Any]:
        """Compress, convert, or resize raster images and conservatively minify SVG.

        settings.quality="auto" uses bounded search and reports measured SSIM plus whether the target was met on an aligned 1024 px RGB thumbnail (0.98 by default).
        settings.strip_exif=true removes EXIF. Safe default: settings.out_dir is required. Source mutation needs
        settings.in_place=true and creates a backup by default. Existing destinations
        require settings.overwrite=true. Animated and multipage images are rejected. A retained
        scan may supply up to max_images declared image URLs; those images are downloaded through
        the bounded network client, optimized into out_dir, and mapped back to source occurrences.
        """
        return _checked(
            handlers.images_optimize(
                files=files, settings=settings, scan=scan, max_images=max_images
            )
        )

    @mcp.tool(annotations=pure, structured_output=True)
    def seo_keywords_cluster(
        keywords: list[str], algorithm: str = "kmeans", n_clusters: int | None = None
    ) -> dict[str, Any]:
        """Cluster keywords into topic groups (K-Means, DBSCAN, Agglomerative)."""
        params: dict[str, Any] = {"keywords": keywords, "algorithm": algorithm}
        if n_clusters is not None:
            params["n_clusters"] = n_clusters
        return _checked(handlers.keywords_cluster(**params))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_robots_check(
        url: str, user_agent: str = "*", paths: list[str] | None = None
    ) -> RobotsCheckResult:
        """Fetch and analyze a site's robots.txt: user-agent groups, declared
        sitemaps, and whether given paths are crawlable."""
        return _checked(handlers.robots_check(url=url, user_agent=user_agent, paths=paths))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_headers_check(url: str, method: Literal["GET", "HEAD"] = "GET") -> dict[str, Any]:
        """Inspect SEO-relevant response headers (X-Robots-Tag, canonical Link,
        Cache-Control, HSTS, ...), HTTP version, TTFB, and body size."""
        return _checked(handlers.headers_check(url=url, method=method))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_asset_weight_check(url: str, file_size_threshold: int | None = None) -> dict[str, Any]:
        """Fetch a page's linked CSS/JS and report delivery problems: render-blocking
        resources in <head>, oversized files, duplicate libraries bundled more than
        once (by content hash), missing minification, missing font-display, legacy
        polyfilled JS, and resources served without compression or a long-lived
        Cache-Control. Unused-code and cross-page outlier detection need a rendered
        DOM / a multi-page run and are reported under `skipped`, not silently clean."""
        return _checked(
            handlers.asset_weight_check(url=url, file_size_threshold=file_size_threshold)
        )

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_links_check(url: str, internal_only: bool = False, limit: int = 200) -> dict[str, Any]:
        """Check a page's outbound links for broken (4xx/5xx) targets and links
        that point at redirects (wasted crawl hops)."""
        return _checked(handlers.links_check(url=url, internal_only=internal_only, limit=limit))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_hreflang_check(url: str) -> dict[str, Any]:
        """Extract and validate a page's hreflang alternates (x-default,
        self-reference, duplicates, malformed codes)."""
        return _checked(handlers.hreflang_check(url=url))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_domain_profile(domain: str, with_tls: bool = True) -> dict[str, Any]:
        """Infrastructure profile of a domain: registrar and domain age (RDAP, whois
        fallback), DNS records with DNS/mail provider, hosting IP with ASN, owner and
        country, reverse DNS, TLS certificate and its expiry, plus risk flags."""
        return _checked(handlers.domain_profile(domain=domain, with_tls=with_tls))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_cdn_check(url: str) -> dict[str, Any]:
        """Which CDN sits in front of a URL and whether caching actually works:
        cache status on a repeat request (MISS->HIT), Cache-Control, ETag/Last-Modified,
        304 revalidation, HTTP version, HTTP/3 advertisement, brotli/gzip and TTFB."""
        return _checked(handlers.cdn_check(url=url))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_tech_detect(url: str) -> dict[str, Any]:
        """Detect the technologies behind a page: CMS, framework, server stack,
        analytics and ad pixels, chat widgets, consent tools, fonts and third-party
        script hosts. Every hit carries the marker it was detected by."""
        return _checked(handlers.tech_detect(url=url))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_security_check(url: str, probe_paths: bool = False) -> dict[str, Any]:
        """Security headers with a score and grade (HSTS, CSP, X-Frame-Options,
        X-Content-Type-Options, Referrer-Policy, Permissions-Policy), software version
        disclosure, cookie flags and the http->https upgrade. Set probe_paths=true to
        also check whether .git/.env and similar service files are exposed."""
        return _checked(handlers.security_check(url=url, probe_paths=probe_paths))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_backlinks_check(target: str, donors: list[str], concurrency: int = 3) -> dict[str, Any]:
        """Verify backlinks from a list of donor pages: is the link still there, its
        anchor and rel, whether it passes weight (nofollow/ugc/sponsored), and whether
        the donor page itself is indexable."""
        return _checked(
            handlers.backlinks_check(target=target, donors=donors, concurrency=concurrency)
        )

    @mcp.tool(annotations=paid, structured_output=True)
    def seo_backlinks_profile(
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
        """Build a bounded backlink profile from explicit CSV/imported evidence or selected sources.

        Offline by default. Optional project/db stores private SQLite history. Explicit sources
        may call free or paid providers; DataForSEO defaults sandbox, requires confirm_paid and
        account/production approval. No automatic pagination or web-wide completeness claim.
        """
        return _checked(
            handlers.backlinks_profile(
                domain=domain,
                history_only=history_only,
                imports=imports,
                evidence=evidence,
                sources=sources,
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
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_backlinks_disavow(
        domains: list[str] | None = None,
        urls: list[str] | None = None,
        output: str | None = None,
    ) -> dict[str, Any]:
        """Generate a reviewed disavow selection locally; never submit or overwrite a file."""
        return _checked(handlers.backlinks_disavow(domains=domains, urls=urls, output=output))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_schema_check(url: str = "", html: str = "") -> dict[str, Any]:
        """Validate a page's structured data in two layers. Layer one is the Schema.org
        vocabulary itself (1010 types, 1676 properties, shipped with the toolkit): does
        the type exist, is the property allowed on it once inheritance is resolved, does
        the value type match, is the term deprecated or still in the pending layer.
        Layer two is Google rich-result eligibility per type. Also analyses the JSON-LD
        as a GRAPH: which entities carry @id, which are linked, which hang as islands,
        and whether any @id reference dangles. Pass html to check markup offline."""
        return _checked(handlers.schema_check(url=url or None, html=html or None))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_schema_build(url: str = "", html: str = "", override_type: str = "") -> dict[str, Any]:
        """Suggest a connected Schema.org @graph for a page. Classifies the page
        (Article/Product/Service/LocalBusiness/...) from URL + content + existing
        JSON-LD, shows the signals it used and its confidence, then builds a linked
        graph (Organization <- WebSite <- WebPage <- <type>) using ONLY facts
        actually visible on the page. Also diffs the suggestion against markup the
        page already carries: which recommended fields are missing, which it can
        fill now. Set override_type when the classifier is unsure (confidence=low)."""
        return _checked(
            handlers.schema_build(
                url=url or None, html=html or None, override_type=override_type or None
            )
        )

    @mcp.tool(annotations=pure, structured_output=True)
    def seo_duplicate_check(
        items: list[dict] | None = None,
        scan: str | None = None,
        threshold: float = 0.92,
        with_fingerprints: bool = False,
        only_indexable: bool = True,
    ) -> dict[str, Any]:
        """Find near-duplicate pages among a list of {id, text} documents using
        simhash + locality-sensitive hashing (no O(n^2) pairwise comparison).
        Returns exact duplicates (by content hash) separately from near-duplicate
        clusters (similarity at or above the threshold, with exact pairwise
        similarity inside each cluster), so a byte-identical pair is never reported
        twice. Feed it page texts from a crawl (SF export, sitemap + parse) to
        surface thin/duplicate content on large sites; ideally each item's text is
        already scoped to the page's content area (see seo_markdown_extract or
        parse's content_text field), so shared navigation and footer boilerplate
        does not create false matches. only_indexable=True (default) compares only
        items whose indexable flag is true or absent, since a page canonicalised to
        another is an intended twin, not a defect; set it to false to audit the
        canonical tags themselves. Pass scan for a validated read-only scan.v1
        corpus; it streams retained page bodies and returns coverage. The corpus
        is capped at 10,000 analyzed documents and 16 MiB of retained input
        (extracted Markdown, not raw HTML); reaching a corpus bound reports the
        exact partial coverage. The analysis itself stays capped at one million
        shingles and 250,000 candidate comparisons; an exhausted analysis budget
        is unavailable, never clean."""
        return _checked(
            handlers.duplicate_check(
                items=items,
                scan=scan,
                threshold=threshold,
                with_fingerprints=with_fingerprints,
                only_indexable=only_indexable,
            )
        )

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_mirror_check(url: str, timeout: float = 12.0) -> dict[str, Any]:
        """Audit host consolidation across HTTP/HTTPS, ``www``, index.php/index.html,
        case, and trailing-slash variants. Returns every redirect hop and identifies live
        200 duplicates, HTTPS-to-HTTP downgrades, chains longer than one hop, dead variants,
        and ``www`` DNS availability. DNS is checked through DNS-over-HTTPS rather than the
        machine's local resolver so local cache or split-DNS state does not create false evidence.
        """
        return _checked(handlers.mirror_check(url=url, timeout=timeout))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_ai_bots_check(url: str = "", robots_text: str = "") -> dict[str, Any]:
        """Which AI crawlers (GPTBot, ClaudeBot, Perplexity, Google-Extended, CCBot,
        Bytespider, Meta-ExternalAgent, …) the site lets in, and which it blocks in
        robots.txt. For each bot: its role (training/retrieval/user), whether it has an
        explicit robots group, and whether the root path is blocked. Pass robots_text to
        check offline; otherwise it fetches /robots.txt from the url's host."""
        return _checked(handlers.ai_bots_check(url=url or None, robots_text=robots_text or None))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_llms_txt_check(url: str, brand: str = "") -> dict[str, Any]:
        """Fetch and score the site's /llms.txt (the LLM-facing manifest): 9 checks —
        H1 title, >=3 sections, >=3 links, brand mention, category mention, product/
        proof/docs pages, size <= 60KB. Returns a 0-10 score, a letter grade, and the
        per-check breakdown. A missing llms.txt is itself a finding (no AI-ready context).
        Set brand to verify the project name is mentioned."""
        return _checked(handlers.llms_txt_check(url=url, brand=brand or None))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_citability_check(
        url: str = "", text: str = "", content_area: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        """Score how citable a piece of content is for AI answers (GEO/AEO): 0-100 across
        four dimensions (25 each) — Answer Blocks (self-contained 20-200 word paragraphs),
        Self-Containment (no context-dependent phrases like 'as mentioned above'),
        Statistical Density (numbers/percentages/dates + evidence markers per 100 words),
        and Structure Quality (headings/lists/TL;DR). Pass text to score a fragment exactly
        as given, or url to fetch the page and score it: fetching scores the resolved
        content area's Markdown (navigation and footer excluded, headings/lists/paragraph
        breaks preserved), not the raw whole-document text, since a flat text blob has no
        structure for the scorer to find. content_area configures that region — see
        seo_markdown_extract / seo_parse's content_area option for its keys."""
        return _checked(
            handlers.citability_check(url=url or None, text=text or None, content_area=content_area)
        )

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_markdown_extract(
        url: str = "", html: str = "", content_area: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        """Render a page as Markdown in two scopes. content_markdown strips navigation
        and footer (boilerplate) while keeping headings, lists, and links -- the
        representation worth diffing between crawls, feeding to content scoring
        (seo_citability_check, seo_duplicate_check), or handing to a model. full_markdown
        keeps header/nav/footer too, for reading -- Markdown has already lost the tag
        structure seo_boilerplate_report hashes, so it is not a valid input there; pass
        that tool the original html (or a hash precomputed from it) instead.
        content_area_strategy records how the region was resolved for this page. Pass
        html to render offline, or url to fetch it first. content_area configures the
        region (root/include CSS selectors, tag/selector exclusions); defaults exclude
        <nav> and <footer>."""
        return _checked(
            handlers.markdown_extract(url=url or None, html=html or None, content_area=content_area)
        )

    @mcp.tool(annotations=pure, structured_output=True)
    def seo_boilerplate_report(
        pages: list[dict] | None = None, scan: str | None = None
    ) -> dict[str, Any]:
        """Answer "is the boilerplate actually the same everywhere?" across a crawled
        corpus. Hashes each page's header/nav/footer markup (structure kept, not just
        text, so a link dropped from a menu still changes the hash), groups pages by
        that hash, and reports every group that is not the dominant one -- with its
        fraction of the corpus and a sample URL. Catches a nav block that lost links on
        one template, a footer never migrated on old pages, or a menu that renders
        differently under one language branch. Each page is {"url", "html"}, or
        {"url", "hash"} when the hash was already computed upstream. Pass scan
        for a validated read-only scan.v1 corpus with retained page HTML. The
        corpus is streamed and only each page's digest is retained, so coverage
        does not depend on total HTML size; reaching the 10,000-document or
        16 MiB retained-input bound reports the exact partial coverage."""
        return _checked(handlers.boilerplate_report(pages=pages, scan=scan))

    @mcp.tool(annotations=pure, structured_output=True)
    def seo_semantic_inputs(
        items: list[dict] | None = None,
        scan: str | None = None,
        content_area: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Build the reproducible normalized-input manifest for semantic
        analysis over retained page content. Each document entry names the
        retained body hash, the exact decoded input hash, the normalized
        output hash, the content-area strategy that was applied, and the
        language evidence (the page's own <html lang> declaration plus
        letter-script shares over the normalized text) — the normalized text
        itself is never returned. Pass scan for a validated read-only scan.v1
        corpus: it streams retained complete bodies offline with no refetch,
        under the crawl's recorded content_area config, and a missing or
        partial body stays an explicit omission — never an empty clean result.
        Or pass items as a list of {"url", "html"} to normalize supplied
        markup offline under an optional content_area config. The corpus is
        capped at 10,000 documents and 16 MiB of normalized retained input;
        reaching a bound reports the exact partial coverage."""
        return _checked(handlers.semantic_inputs(items=items, scan=scan, content_area=content_area))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_semantic_similarity(
        items: list[dict] | None = None,
        scan: str | None = None,
        embeddings: list[dict] | None = None,
        adapter: dict | None = None,
        cache_path: str = "",
        threshold: float = 0.82,
        max_candidate_comparisons: int = 250_000,
    ) -> dict[str, Any]:
        """Group topical and internal-link review candidates from supplied vectors.

        Pass either supplied ``items`` ({url, html}) or a retained scan.v1
        ``scan``; scan selection and normalization reuse seo_semantic_inputs.
        ``embeddings`` contains {url, vector} rows and ``adapter`` declares the
        model/version/settings and transfer policy. SEOHEAD does not download,
        load or call a model here. A provider declaration requires explicit
        external_authorized=true even though this route only consumes supplied
        vectors. cache_path is a local SQLite cache keyed by source hash, model
        identity and settings. Groups are review candidates, never duplicate or
        cannibalization conclusions; missing vectors and bounded coverage stay
        explicit in the response."""
        return _checked(
            handlers.semantic_similarity(
                items=items,
                scan=scan,
                embeddings=embeddings,
                adapter=adapter,
                cache_path=cache_path or None,
                threshold=threshold,
                max_candidate_comparisons=max_candidate_comparisons,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_meta_description_drafts(
        items: list[dict] | None = None,
        scan: str | None = None,
        context: dict | None = None,
        drafts: list[dict] | None = None,
        executor: dict | None = None,
        checkpoint_path: str = "",
        batch_size: int = 20,
        json_path: str = "",
        csv_path: str = "",
    ) -> dict[str, Any]:
        """Prepare or validate a resumable, page-grounded meta-description batch.

        With no drafts this is a dry-run plan over supplied HTML or a retained
        scan.v1 corpus. With structured supplied drafts it validates URL/source
        hashes, Unicode length policy and review flags, then checkpoints only
        local results. The calling or delegated agent owns generation; this tool
        has no model key or provider call. executor declares its versioned
        contract and runtime kind, while checkpoint_path enables resume. Optional
        json_path and csv_path export a review artifact; neither path writes a
        CMS or metadata."""
        return _checked(
            handlers.meta_description_drafts(
                items=items,
                scan=scan,
                context=context,
                drafts=drafts,
                executor=executor,
                checkpoint_path=checkpoint_path or None,
                batch_size=batch_size,
                json_path=json_path or None,
                csv_path=csv_path or None,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_ai_column(
        items: list[dict] | None = None,
        scan: str | None = None,
        prompt: str = "",
        urls: list[str] | None = None,
        column: str = "ai_column",
        max_pages: int = 100,
        rows: list[dict] | None = None,
        csv_path: str = "",
    ) -> dict[str, Any]:
        """Plan a per-URL AI custom column over retained page evidence, then validate its values.

        With no rows this is a dry-run plan: a prompt, the selected URLs' title,
        headings and text excerpt from a retained scan.v1 or supplied items, a
        character-based size estimate with no price applied, and the consent scope.
        With rows, each value must echo the page's source hash and is validated
        for presence, staleness and length; missing or failed pages are reported,
        never filled. The calling or delegated agent owns the prompt run and the
        transfer of data; this tool has no model key and makes no provider call.
        Optional csv_path writes a formula-safe local CSV and never writes back to
        a site or CMS."""
        return _checked(
            handlers.ai_column(
                items=items,
                scan=scan,
                prompt=prompt,
                urls=urls,
                column=column,
                max_pages=max_pages,
                rows=rows,
                csv_path=csv_path or None,
            )
        )

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_social_meta_check(
        url: str = "", og: dict | None = None, twitter: dict | None = None
    ) -> dict[str, Any]:
        """Check OpenGraph and Twitter Card tags against the minimum needed for a link
        preview to render: which required tags (og:title/type/url/image/image:alt,
        twitter:card/title/description/image/image:alt) are missing, and which recommended
        ones. Pass url to fetch and check, or hand in pre-extracted og/twitter dicts."""
        return _checked(handlers.social_meta_check(url=url or None, og=og, twitter=twitter))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_soft404_check(url: str) -> dict[str, Any]:
        """Detect soft-404: whether the site returns an honest 404/410 for non-existent URLs,
        or silently serves 200/3xx (which pollutes the index with junk pages). Sends two
        deterministic probes (sha256 of origin, at ordinary root paths) and applies strict
        AND-logic: both 2xx/3xx -> soft-404 confirmed (warning); both 404/410 -> pass;
        anything else -> unknown. Screaming Frog cannot see this — it crawls known URLs,
        not invented ones, so this needs an active request."""
        return _checked(handlers.soft404_check(url=url))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_log_analyze(
        path: str,
        verify_bots: bool = False,
        max_lines: int = 5_000_000,
        offset: int = 0,
        limit: int = 100,
        max_dns: int = 20,
        bot: str | None = None,
        ip: str | None = None,
    ) -> dict[str, Any]:
        """Analyse an access log offline with exact disk-backed IP totals and a bounded IP page.

        Apache/Nginx Common/Combined, IIS and gzip are supported. User-agent labels
        are claims, not verified identities. verify_bots explicitly enables bounded,
        cached forward-confirmed reverse DNS for this page only (max_dns plus one
        canary). Every IP has its own PTR, forward match, verdict, visits and last visit.
        DNS failure/deadline remains unchecked; it never becomes a fake-bot verdict.
        """
        return _checked(
            handlers.log_analyze(
                path=path,
                verify_bots=verify_bots,
                max_lines=max_lines,
                offset=offset,
                limit=limit,
                max_dns=max_dns,
                bot=bot,
                ip=ip,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_logs_import(
        path: str, project: str, max_lines: int = 100_000_000, max_files: int = 1000
    ) -> dict[str, Any]:
        """Stream supplied access logs into a new local project logs.v1 artifact.

        Reads files/gzip or a bounded directory. Retains exact IP and date/URL/status
        aggregates, coverage and source digests; source files stay unchanged. No DNS,
        site request, provider call or scan mutation occurs. Raw client IPs stay local.
        """
        return _checked(
            handlers.logs_import(
                path=path, project=project, max_lines=max_lines, max_files=max_files
            )
        )

    @mcp.tool(annotations=pure, structured_output=True)
    def seo_logs_summary(
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
        """Read one bounded page of retained log URL, bot IP, bot, date, status or section aggregates.

        Totals remain exact for imported rows. UTC date and bot filters are explicit;
        unsupported/missing measurements remain unavailable. No DNS or source reimport.
        """
        return _checked(
            handlers.logs_summary(
                source=source,
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
        )

    @mcp.tool(annotations=pure, structured_output=True)
    def seo_logs_join(
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
        """Join retained access-log requests with a validated native scan, offline.

        Read bounded views for scan URLs without recorded bot visits, bot URLs absent
        from the scan (candidates), response-code waste, nonindexable requests and
        sections. Absence conclusions require complete compatible evidence; missing
        or partial scans remain unavailable. No crawl, DNS or scan schema change.
        """
        return _checked(
            handlers.logs_join(
                source=source,
                scan=scan,
                view=view,
                base_url=base_url,
                family=family,
                bot=bot,
                date_from=date_from,
                date_to=date_to,
                limit=limit,
                offset=offset,
            )
        )

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_logs_bots(
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
        """Read a bounded per-IP bot page with retained PTR, forward result, visits and last visit.

        No DNS runs by default. verify_bots explicitly refreshes only the selected page
        within max_dns (plus one canary), using a one-hour local cache. Requests are
        sequential and rate-limited. DNS failure/deadline is unchecked, not fake.
        Cached verdict filters are read-only; verification with a verdict filter is refused.
        The cache writes only to the supplied local log artifact, never to a scan.
        """
        return _checked(
            handlers.logs_bots(
                source=source,
                verify_bots=verify_bots,
                limit=limit,
                offset=offset,
                bot=bot,
                family=family,
                ip=ip,
                verdict=verdict,
                max_dns=max_dns,
            )
        )

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_regions_check(
        url: str, extra: list[str] | None = None, limit: int = 12, render: bool = False
    ) -> dict[str, Any]:
        """Audit a site's regional structure: subdomains (msk.site.ru), folders
        (site.ru/msk/) and satellite domains (site-msk.ru). Finds the city switcher on the
        page, recognises ~100 Russian cities by URL slug (msk/moskva/moscow all collapse to
        one region) and by Cyrillic anchor text, then fetches each regional page and reports
        what actually kills regional promotion: hosts that just redirect to the main domain
        (so the region does not exist), regional pages canonicalised to another host
        (self-removal from the index), noindex, identical content across cities, one shared
        phone number for the whole country, a missing city name in the title, and two
        schemes used at once. Satellite domains are invisible from the page — pass them in
        `extra`. Set render=true when the city switcher is drawn by JavaScript (needs
        Playwright) — on many large sites it is not in the raw HTML at all."""
        return _checked(handlers.regions_check(url=url, extra=extra, limit=limit, render=render))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_render_check(
        url: str,
        viewport: str = "desktop",
        wait: str = "load",
        user_agent: str | None = None,
        transport_config: dict[str, str] | None = None,
    ) -> dict[str, Any]:
        """Compare the raw server HTML with the DOM after JavaScript runs — the gap between
        them is what a non-rendering crawler loses. Reports an empty SPA shell
        (<div id="root"></div> means a robot gets a blank page), the share of text and
        internal links that appear only after JS, a title/canonical rewritten by script,
        and Schema.org markup injected client-side. Also returns lab timings (TTFB, FCP,
        LCP, CLS, load) measured in one Chromium run — these are lab numbers, not field
        Core Web Vitals from CrUX, and are labelled metrics_lab for that reason. Also
        returns dual_crawl (schema dualcrawl.v1): per-URL image/link evidence seen by only
        the raw pass or only the rendered pass, a separate question from the raw/rendered
        diff above. Requires Playwright; if it is missing the tool says so and gives the
        install command instead of failing. A render that did not finish — a document
        with no title, no h1, no canonical and no links, far smaller than the raw
        response — comes back as ok:false with reason "incomplete_render" and both
        snapshots, never as findings about the site: an unmeasured page is not a defect.
        A requested wait milestone that times out (networkidle on a site with long-polling
        scripts) falls back to reading the DOM at domcontentloaded, recorded in
        wait_reached. `viewport="mobile"` uses a stable smartphone diagnostic identity;
        user_agent overrides that identity for both requests. `transport_config`
        opts into an operator-supplied remote Playwright connection using
        `transport=remote`, `remote_protocol=playwright`, `remote_endpoint_env`
        and `remote_playwright_version`. It never provisions a server or falls
        back to local launch after a remote failure."""
        if transport_config is not None:
            return _checked(
                handlers.render_check(
                    url=url,
                    viewport=viewport,
                    wait=wait,
                    user_agent=user_agent,
                    transport_config=transport_config,
                )
            )
        return _checked(
            handlers.render_check(url=url, viewport=viewport, wait=wait, user_agent=user_agent)
        )

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_site_audit(
        url: str,
        urls: list[str] | None = None,
        limit: int = 25,
        concurrency: int = 5,
        render: bool = False,
        skip: list[str] | None = None,
        crux_evidence: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Run the whole live toolkit over one site and return a single audit document
        (schema seohead.site-audit/1). Site-level tools run once (domain profile, CDN and
        cache, tech stack, security headers, robots, AI crawlers, llms.txt, regions,
        raw-vs-rendered, sitemap); page-level tools run per URL (parse, Schema.org,
        Open Graph). URLs come from the sitemap unless you pass `urls`. Every finding is
        collected into one sorted list with a severity assigned by aggregator rules — the
        document says so explicitly, because severity here is a rule, not a measurement.
        A tool that fails does NOT fail the audit: it lands in summary.tools_failed with
        its reason, so silence is never mistaken for a clean result. Feed the returned
        document straight into seo_report_build. Optional crux_evidence is an already
        collected CrUX current record or bounded sample; no Google request occurs here.
        URL and origin field scopes remain distinct from Lighthouse lab results."""
        return _checked(
            handlers.site_audit(
                url=url,
                urls=urls,
                limit=limit,
                concurrency=concurrency,
                render=render,
                skip=skip,
                crux_evidence=crux_evidence,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_report_build(
        audit: dict | str,
        fmt: str = "xlsx",
        out: str | None = None,
        project: str | None = None,
        view: str | None = None,
        offset: int = 0,
        lang: str = "en",
        pdf_policy: Literal["overview-v1"] | None = None,
    ) -> dict[str, Any]:
        """Turn an audit document into a file: xlsx, docx, csv, md, json or pdf. Pass the dict
        returned by seo_site_audit, an SF Analyzer audit.json from sf_audit_run (or a
        path to either one's JSON, or a validated scan.v1 SQLite artifact) — both audit
        schemas are recognized and normalized before
        rendering. xlsx has four sheets with filters and a live Excel chart — for work;
        docx is prose with headings — for the client; csv writes separate findings,
        scope-evidence, and page tables for a tracker, listed under outputs;
        md is for reading and for git; pdf is a localized offline Chromium printout (en or ru).
        PDF requires the optional `pdf` dependencies and a local Chrome, Edge or Chromium.
        The generators compute nothing and reach no network:
        what is not in the JSON does not appear in the report. A document matching neither
        schema is refused with ok: false naming the mismatch, never rendered as an empty report.
        Pass project to include validated checklist coverage, reasons, scope and measurements in a
        human report. Optional view applies one saved finding view; it leaves health, evidence,
        coverage and source scan untouched. offset pages through the stable sorted view. This never
        makes a network request.

        For retained audit.v2 only, explicit pdf_policy='overview-v1' writes a bounded
        PDF overview with mandatory complete JSON/CSV companions and a hashed manifest.
        Exact source, displayed and omitted counts remain visible. No policy is chosen
        automatically; streamed PDF without this policy is refused. Other formats and
        materialized inputs reject the policy. PDF and companion directory must be new.
        """
        arguments = {"audit": audit, "fmt": fmt, "out": out, "project": project}
        if pdf_policy is not None:
            arguments["pdf_policy"] = pdf_policy
        if view is not None:
            arguments["view"] = view
        if offset != 0:
            arguments["offset"] = offset
        if (fmt or "").lower().lstrip(".") == "pdf" or lang != "en":
            arguments["lang"] = lang
        return _checked(handlers.report_build(**arguments))

    @mcp.tool(annotations=pure, structured_output=True)
    def seo_facts_export(sites: list[dict[str, Any]]) -> dict[str, Any]:
        """Build one comparable facts table (schema facts.v1) across several sites from
        crawl/site audits you already produced. Each site is
        {label, crawl_audit?, site_audit?} -- crawl_audit is an SF Analyzer audit.json
        (dict or path), site_audit is a seo_site_audit document (dict or path), both
        optional but at least one is needed for real facts; a site with neither still
        gets a full row of unavailable facts, named. Reads documents only: zero network
        requests, zero paid calls. Every leaf is a fact with one of five states --
        measured, absent (a true 0/[]), partial (bounded, e.g. a crawl that stopped
        early), unavailable (the source did not answer), or not_requested (this tool
        never asks, as for Google/Yandex index counts: no configured provider returns
        a result count) -- plus an is_not line saying what it must not be read as.
        This tool computes no score, rank, ratio or winner: it exports operands, not
        quotients, so a language model can compare the sites itself. Refuses before
        doing any work when a document's own domain does not match the label it is
        filed under, or when two labels resolve to the exact same domain; two
        subdomains of one registrable domain are allowed, only noted."""
        return _checked(handlers.facts_export(sites=sites))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_compare_crawls(
        before: Any,
        after: Any,
        force: bool = False,
        correspondence: Any = None,
        out_dir: str | None = None,
        compression: Literal["none", "gzip"] = "none",
    ) -> dict[str, Any]:
        """Diff two audit documents (dict, JSON path, or scan.v1 SQLite path) into four disjoint
        sets per finding: entered (new problem on a page that existed before),
        left (the page is still crawled and no longer matches — a real fix),
        appeared (a genuinely new page with a finding), disappeared (the page is
        not in this crawl at all, so a missing finding proves nothing). "left" and
        "disappeared" look identical in a naive diff and mean opposite things.
        Optional ``correspondence`` is a closed url-correspondence.v1 object (or
        local JSON path) declaring origin and explicit URL pairs for a migration;
        it never infers pairs from titles or content and adds a release_review.v1
        facts/finding artifact. Refuses a known difference in results-affecting
        settings unless ``force`` is true; partial-crawl warnings remain attached
        to the historical result.

        With out_dir, writes a new compare.v2 package with partition manifests,
        exact finding rows, checksums and before/after count conservation. Without
        it, returns the small compare.v1 document; retained audit.v2 populations
        above 10,000 pages or issues require explicit out_dir. Never overwrites
        a package and never collects evidence or calls the network. Explicit
        compression='gzip' requires out_dir and preserves every row in deterministic
        .ndjson.gz files; manifests record compressed checksums and both byte counts.
        Default compression='none' keeps uncompressed output.
        """
        return _checked(
            handlers.compare_crawls(
                before=before,
                after=after,
                force=force,
                correspondence=correspondence,
                out_dir=out_dir,
                compression=compression,
            )
        )

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_verify_fixes(
        baseline: Any,
        out_dir: str,
        finding_ids: list[str] | None = None,
        view: Any = None,
        urls: list[str] | None = None,
        urls_file: str | None = None,
        after: Any = None,
        config: str | None = None,
    ) -> dict[str, Any]:
        """Recheck selected baseline findings in an explicit, bounded URL subset.

        Select baseline finding IDs, a saved verification_view.v1 JSON view, or
        affected URLs. With ``after`` the comparison is offline and requires a
        distinct scan UUID plus a later observation time. Otherwise the
        recorded HTTP/robots/render policy is verified before the existing crawler
        fetches selected pages; JS baselines use one URL per rendered crawl. A new
        directory receives the recrawl evidence and immutable verification JSON
        and Markdown report. Unfetched, skipped, site-wide and incomparable
        findings remain not_verifiable, never resolved.
        """
        return _checked(
            handlers.verify_fixes(
                baseline=baseline,
                out_dir=out_dir,
                finding_ids=finding_ids,
                view=view,
                urls=urls,
                urls_file=urls_file,
                after=after,
                config=config,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_crawl_enrich(
        audit: Any,
        external_csv: str,
        url_column: str = "url",
        ignore_query: bool = False,
        ignore_scheme: bool = False,
        casefold_path: bool = False,
        out_urls: str | None = None,
        visits_column: str | None = None,
        bounce_column: str | None = None,
    ) -> dict[str, Any]:
        """Join an existing audit or scan to an offline URL-keyed CSV without a
        provider call. Matched rows retain both page and external data; crawl-only,
        external-only, and unkeyable rows remain separate. A completed crawl can
        write same-origin external-only URLs to ``out_urls`` for `crawl-site
        --urls-file`; a partial crawl never labels them as orphan URLs."""
        return _checked(
            handlers.crawl_enrich(
                audit=audit,
                external_csv=external_csv,
                url_column=url_column,
                ignore_query=ignore_query,
                ignore_scheme=ignore_scheme,
                casefold_path=casefold_path,
                out_urls=out_urls,
                visits_column=visits_column,
                bounce_column=bounce_column,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_crawl_import(manifest_path: str) -> dict[str, Any]:
        """Read a versioned third-party crawl CSV bundle from a local manifest.

        The manifest maps source headers to page, link, status, and redirect
        fields. The result is ``third_party_crawl.v1`` with source identity and
        per-field coverage; it is not scan.v1 or SF audit evidence.
        """
        return _checked(handlers.crawl_import(manifest_path=manifest_path))

    @mcp.tool(annotations=pure, structured_output=True)
    def seo_segment_diff(audit: Any, source: str, target: str) -> dict[str, Any]:
        """Cross-segment counterpart diff (#358): which pages in the ``source`` segment
        (e.g. "en") have a counterpart in the ``target`` segment (e.g. "pl"), and which
        do not. Requires a native crawl whose scope.segments were declared and whose
        pages carry hreflang alternates (#357) — an audit missing either is refused by
        name. Classifies every eligible page into exactly one of: declared (hreflang
        names the counterpart and it was crawled), declared_not_crawled (hreflang names
        one the crawl never reached), inferred (no hreflang, but the mirrored path
        exists and this site's own mirror rate measured high enough to trust that),
        absent (no counterpart by either method), or undetermined (naming why neither
        method could safely answer — e.g. a translated slug on a site whose mirror rate
        is too low for path inference, or a target segment the crawl only partially
        reached, where "absent" would be an unsupported negative claim)."""
        return _checked(handlers.segment_diff(audit=audit, source=source, target=target))

    # --- External data providers: demand, search results, traffic, and spend ---

    @mcp.tool(annotations=paid, structured_output=True)
    def seo_keywords_expand(
        phrase: str, limit: int = 300, regions: list[str] | None = None
    ) -> dict[str, Any]:
        """Expand a seed phrase via Yandex Wordstat: refinements (left column) plus similar
        queries (right column), each with its base frequency. Frequency here is BASE, not
        exact — the API has no !/+/[] operators and base runs roughly 9x higher than exact;
        good enough for a first cut, then top up exact via seo_keywords_exact. A
        multi-region request SUMS frequency, so query regions one at a time. Paid and
        quota-bound: 100 Wordstat requests per hour."""
        return _checked(handlers.keywords_expand(phrase=phrase, limit=limit, regions=regions))

    @mcp.tool(annotations=paid, structured_output=True)
    def seo_keywords_seasonality(
        phrase: str,
        from_date: str,
        to_date: str,
        period: str = "PERIOD_MONTHLY",
        regions: list[str] | None = None,
    ) -> dict[str, Any]:
        """Demand over time for one phrase (Yandex Wordstat dynamics). Dates are RFC3339,
        e.g. 2026-01-01T00:00:00Z. Use it to tell a dead query from a seasonal one before
        deciding a page is not worth building."""
        return _checked(
            handlers.keywords_seasonality(
                phrase=phrase, from_date=from_date, to_date=to_date, period=period, regions=regions
            )
        )

    @mcp.tool(annotations=paid, structured_output=True)
    def seo_keywords_exact(
        keywords: list[str], region: int = 225, wait: bool = True
    ) -> dict[str, Any]:
        """Exact frequency (!W) for a list of phrases via Arsenkin — the number Wordstat's
        API will not give you. Paid, spends account limits. The charge and task_id are
        journaled the moment the task is created, so a paid result is never lost: pass
        wait=false to get the task_id and collect the result later for free.

        On success with wait=true: ok, task_id, cost, region, frequencies, result (the raw
        provider payload). frequencies maps each phrase to {"base": N, "overal": N} —
        overal is Arsenkin's field for !W (!WS, exact wordform); quoted is the "WS" phrase
        operator and exact is the [!WS] strict-order operator, so neither appears here as
        !W. A phrase with no data for the requested region is omitted and named in
        warnings instead of borrowing another region's number; an absent frequency field
        stays null rather than becoming zero. cleaned lists phrases whose punctuation was
        stripped because the provider rejects them; those phrases are measured in their
        rewritten form. With wait=false the task is only created: the reply is ok,
        task_id, cost, region, cleaned, and a recovery note — no frequencies."""
        return _checked(handlers.keywords_exact(keywords=keywords, region=region, wait=wait))

    @mcp.tool(annotations=paid, structured_output=True)
    def seo_serp_fetch(
        query: str | None = None,
        queries: list[str] | None = None,
        region: str = "225",
        top: int = 10,
    ) -> dict[str, Any]:
        """Yandex search results for one query or a batch. Async only: the synchronous endpoint
        is materially more expensive and deliberately absent. A batch launches all operations
        at once and polls them together, so N queries take one batch's time, not N times
        one. Use it to see who actually ranks before promising a client a position."""
        return _checked(handlers.serp_fetch(query=query, queries=queries, region=region, top=top))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_dataforseo_estimate(
        operation: str,
        query_count: int = 1,
        row_limit: int = 0,
        cache_hits: int = 0,
        prices: dict[str, Any] | None = None,
        env: str | None = None,
        refresh_prices: bool = False,
        query: str | None = None,
    ) -> dict[str, Any]:
        """Calculate a quote offline using a supplied appendix/user_data price snapshot.

        Cache hits are caller-declared; this tool never claims to discover cached data.
        Unknown pricing is unavailable. refresh_prices explicitly reads the free account endpoint;
        without it, no provider request or credential access occurs.
        """
        return _checked(
            handlers.dataforseo_estimate(
                operation=operation,
                query_count=query_count,
                row_limit=row_limit,
                cache_hits=cache_hits,
                prices=prices,
                env=env,
                refresh_prices=refresh_prices,
                query=query,
            )
        )

    @mcp.tool(annotations=paid, structured_output=True)
    def seo_google_keywords(
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
        """Google demand via DataForSEO: pass `keywords` for search volume and competition on a
        list, or `seed` to expand semantics from one phrase (the Google counterpart of
        Wordstat's left column). `difficulty=true` returns keyword difficulty instead of volume.

        IMPORTANT: DataForSEO does not support locations in Russia or Belarus.
        A request with such a country does NOT go out — it returns a refusal naming what to use
        instead (Wordstat, Arsenkin). Use seo_keywords_expand / seo_keywords_exact for Yandex
        and the Russian-speaking market; this tool is for Google and everywhere else.

        Default environment is `sandbox`: real response shape, fake data, nothing charged.
        Production is an explicit opt-in via DATAFORSEO_ENV=prod."""
        return _checked(
            handlers.google_keywords(
                keywords=keywords,
                seed=seed,
                location_code=location_code,
                language=language,
                country=country,
                confirm_paid=confirm_paid,
                env=env,
                spend_ceiling_usd=spend_ceiling_usd,
                prices=prices,
                limit=limit,
                difficulty=difficulty,
            )
        )

    @mcp.tool(annotations=paid, structured_output=True)
    def seo_google_serp(
        query: str,
        location_code: int = 2840,
        language: str = "en",
        depth: int = 10,
        country: str | None = None,
        confirm_paid: bool = False,
        env: str | None = None,
        spend_ceiling_usd: float | None = None,
        prices: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Google organic results for a query — who actually ranks. Same geo rules as
        seo_google_keywords: Russia and Belarus are not covered, use seo_serp_fetch (Yandex)
        for those. Default environment is sandbox."""
        return _checked(
            handlers.google_serp(
                query=query,
                location_code=location_code,
                language=language,
                depth=depth,
                country=country,
                confirm_paid=confirm_paid,
                env=env,
                spend_ceiling_usd=spend_ceiling_usd,
                prices=prices,
            )
        )

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_topvisor_read(
        operation: str = "projects", params: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        """Read existing Topvisor projects, competitors, groups, keywords, history or summary.
        Uses topvisor/access_token and topvisor/user_id under the central credential root
        (or TOPVISOR_TOKEN and TOPVISOR_USER_ID). One page only: limit defaults to 100,
        maximum 1000; the continuation signal is the provider's nextOffset — present on
        non-final pages, absent on the last — not len(result) == limit. Provider total
        and limitedBy pass through when sent, separate from the echoed request limit/offset.
        projects/competitors/groups/keywords return arrays; history and summary return
        objects (history rows at result.keywords; summary covers the two requested dates).
        Non-project operations require project_id. History regions_indexes are project
        region indexes (projects with show_searchers_and_regions:2), not geographic region
        keys; summary takes the singular region_index. In positionsData, position is an
        integer ordinal rank; "--" means the query had no position inside the checked
        depth — unavailable, not rank 0 or 100 — and a requested date without an entry is
        a missing observation. headers.dates lists dates actually returned; topsByDepth is
        percent of queries in Top N; visitors, dynamics and tops are counts; avgs is an
        average rank. Does not launch checks, add/edit/delete records, or authorize paid
        operations. Transport redirects are refused and provider errors are redacted."""
        return _checked(handlers.topvisor_read(operation=operation, params=params))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_metrika_counters() -> dict[str, Any]:
        """List the Yandex Metrika counters this token can see (id, name, site). Start here to
        get the counter_id the report tools need. Requires a Metrika OAuth token."""
        return _checked(handlers.metrika_counters())

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_metrika_setup(counter_id: str) -> dict[str, Any]:
        """How a counter is configured: goals, filters, data operations. Check this BEFORE
        drawing conclusions from traffic. Operations can silently reshape reports (URL
        parameter trimming, for instance), and with no goals configured there are no
        conversions in the data at all — so "zero conversion" in a report would be a
        consequence of setup, not a fact about the site."""
        return _checked(handlers.metrika_setup(counter_id=counter_id))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_metrika_report(
        counter_id: str,
        metrics: str,
        dimensions: str | None = None,
        date1: str = "30daysAgo",
        date2: str = "today",
        filters: str | None = None,
        sort: str | None = None,
        limit: int = 100,
        paginate: bool = False,
    ) -> dict[str, Any]:
        """What visitors actually did, as flat records. metrics and dimensions are
        comma-separated in API notation (ym:s:visits, ym:s:startURL); dates accept relative
        forms like 30daysAgo. This is the missing half of an audit: a page can be technically
        perfect and get no visits at all. paginate=true walks every page but stops at
        100 000 rows, and says so via "capped". A "Query is too complicated" refusal is
        retried month by month and, when a month still refuses, at a sampled accuracy;
        "split", "accuracy", "sampled" and "sample_share" in the answer say what was
        actually used — a null "sampled" means the API did not report it, not "unsampled".
        Only count metrics additive over disjoint periods (ym:s:visits, ym:s:pageviews)
        can be merged — unique-visitor, ratio or average metrics fail rather than sum
        wrong."""
        return _checked(
            handlers.metrika_report(
                counter_id=counter_id,
                metrics=metrics,
                dimensions=dimensions,
                date1=date1,
                date2=date2,
                filters=filters,
                sort=sort,
                limit=limit,
                paginate=paginate,
            )
        )

    @mcp.tool(annotations=rewrite_files_from_web, structured_output=True)
    def seo_metrika_traffic_pdf(
        out_dir: str,
        counter_id: str | None = None,
        date1: str | None = None,
        date2: str | None = None,
        document: dict[str, Any] | str | None = None,
        attribution: Literal["last_significant", "last_click"] = "last_significant",
        traffic: Literal["organic", "all"] = "organic",
        filters: str | None = None,
        lang: Literal["en", "ru"] | None = None,
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
        """Static A4-landscape traffic report (HTML + PDF) in the style of an analytics
        dashboard, built from Yandex Metrika: KPI cards with % change, daily dynamics against the
        previous period and a year earlier, 3/6/12-month windows, search engines, cities,
        countries, devices, age, gender, top landing pages and search phrases, and all traffic
        channels.

        Collection (counter_id, date1, date2 as YYYY-MM-DD) makes read-only Reporting API calls
        (about 40 requests, free, Metrika quota) and writes metrika-traffic.json; pass an existing
        document instead to render offline. Attribution defaults to the last significant source
        (the figures Metrika's own interface shows); last_click is explicit. traffic=organic keeps
        search-engine visits; the channels block always covers all traffic. Every block carries
        status ok/unavailable/skipped with a reason; missing comparisons are null, never zero.
        Optional Search Console queries come from gsc_rows or gsc_site_url (a Search Console
        request). Writes only into out_dir; existing report files are refused unless
        overwrite=true replaces them. The PDF needs an installed Chrome, Edge or Chromium
        (SEOHEAD_CHROME overrides discovery); without one the HTML is still written and pdf is
        reported as skipped. brand is a JSON object or file: name, accent, ink, card,
        table_header, positive, negative, font_stack, logo_text."""
        return _checked(
            handlers.metrika_traffic_pdf(
                counter_id=counter_id,
                date1=date1,
                date2=date2,
                out_dir=out_dir,
                document=document,
                attribution=attribution,
                traffic=traffic,
                filters=filters,
                lang=lang,
                top=top,
                site_label=site_label,
                brand=brand,
                gsc_rows=gsc_rows,
                gsc_site_url=gsc_site_url,
                render=render,
                pdf=pdf,
                overwrite=overwrite,
                timeout=timeout,
            )
        )

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_sources_sync(
        source: str,
        resource: str,
        start_date: str | None = None,
        end_date: str | None = None,
        db: str | None = None,
        project: str | None = None,
        force: bool = False,
    ) -> dict[str, Any]:
        """Accumulate provider data in one local SQLite database: fetch missing or explicitly
        forced days. source is gsc, ga4, metrika, webmaster (queries per day) or
        webmaster_history (daily shows/clicks, pages in search, crawl by HTTP class); resource
        is the property (sc-domain:example.com), GA4 property ID, Metrika counter ID or
        Webmaster host ID (https:example.com:443). Pass db (a file path) or project (its
        sources.sqlite is used). Default period is the last 28 eligible days. Requested
        lagged days remain visible without being fetched. force=true re-fetches stored days;
        a partial or failed retry cannot erase a complete day. Tokens are never stored."""
        return _checked(
            handlers.sources_sync(
                source=source,
                resource=resource,
                start_date=start_date,
                end_date=end_date,
                db=db,
                project=project,
                force=force,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_sources_status(db: str | None = None, project: str | None = None) -> dict[str, Any]:
        """Offline local history: requested, complete, empty, partial, failed, pending and
        lagged dates, gaps, rows, metric additivity and reporting-timezone policy."""
        return _checked(handlers.sources_status(db=db, project=project))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_sources_export(
        source: str,
        resource: str | None = None,
        start_date: str | None = None,
        end_date: str | None = None,
        match: str | None = None,
        limit: int = 1000,
        out: str | None = None,
        db: str | None = None,
        project: str | None = None,
    ) -> dict[str, Any]:
        """Read stored rows of one source filtered by resource, date range and a substring
        of any dimension (match). Returns at most limit ordered rows with their daily
        coverage state; out writes the same bounded result to a new private CSV. The
        response names non-additive metrics and has_more when additional rows exist."""
        return _checked(
            handlers.sources_export(
                source=source,
                resource=resource,
                start_date=start_date,
                end_date=end_date,
                match=match,
                limit=limit,
                out=out,
                db=db,
                project=project,
            )
        )

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_regions_tree(save_to: str | None = None) -> dict[str, Any]:
        """Authoritative tree of Yandex region IDs for the regions[] parameter. This is the
        only FREE Wordstat method, so it is not journaled. Note that a multi-region request
        SUMS frequency rather than reporting per region — query regions one at a time."""
        return _checked(handlers.regions_tree(save_to=save_to))

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_spend_report(since: str | None = None, csv_path: str | None = None) -> dict[str, Any]:
        """What the paid sources have actually charged: totals by source, by operation and
        by day, read from the local journal. Estimating spend by eye has already missed the
        provider usage was recorded, so check here before and after a large run. since is
        YYYY-MM-DD. csv_path writes every journal row to that file instead of the totals."""
        return _checked(handlers.spend_report(since=since, csv_path=csv_path))

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_sources_doctor() -> dict[str, Any]:
        """Inspect redacted credential references, readiness and declared provider operations.

        Configured credentials do not verify account or target permission. This local check makes
        no provider requests; use seo_provider_verify for an explicit bounded read.
        """
        return _checked(handlers.sources_doctor())

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_wayback_history(
        url: str,
        limit: int | None = None,
        from_date: str | None = None,
        to_date: str | None = None,
    ) -> dict[str, Any]:
        """Every recorded Wayback Machine snapshot of a URL, oldest first: timestamp, HTTP
        status, and MIME type at capture time. Free and keyless. Answers what a crawl cannot —
        *when* a page started returning its current status, and what preceded it. A URL the
        archive never captured is not an error; it comes back with an empty snapshot list."""
        return _checked(
            handlers.wayback_history(url=url, limit=limit, from_date=from_date, to_date=to_date)
        )

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_cloudflare_traffic(
        zone: str, since: str | None = None, until: str | None = None
    ) -> dict[str, Any]:
        """Bot and human traffic for a Cloudflare zone from edge GraphQL analytics (read-only,
        free): bots x URLs x status codes x cache status per day, in the same shape as
        seo_log_analyze. Use it when origin logs are absent. It is aggregated data
        (source=cloudflare-aggregated), not raw logs: no client IPs, so bots cannot be verified
        by reverse DNS. Dates are UTC YYYY-MM-DD; the Free plan keeps 32 days. The token is read
        from CLOUDFLARE_API_TOKEN or SEOHEAD_CLOUDFLARE_TOKEN_FILE."""
        return _checked(handlers.cloudflare_traffic(zone=zone, since=since, until=until))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_crtsh_subdomains(domain: str) -> dict[str, Any]:
        """Subdomains discovered from public Certificate Transparency logs (crt.sh). Free and
        keyless. Every TLS certificate ever issued for a domain is public record, so this finds
        hosts no page links to — the gap seo_mirror_check and seo_regions_check both currently
        rely on being told about by hand. crt.sh is a free public service without an SLA; a
        slow or unavailable response is reported as a failure, never as "zero subdomains"."""
        return _checked(handlers.crtsh_subdomains(domain=domain))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_gsc_query(
        site_url: str,
        mode: str = "search_analytics",
        start_date: str = "28daysAgo",
        end_date: str = "today",
        dimensions: list[str] | None = None,
        row_limit: int = 1000,
        inspection_url: str | None = None,
    ) -> dict[str, Any]:
        """Google Search Console: search performance for a verified property
        (mode=search_analytics: clicks, impressions, position, CTR) or Google's own indexing
        verdict for one URL (mode=inspect_url).

        Requires an OAuth2 bearer token for an own, verified property — see seo_sources_doctor
        and docs/SETUP.md for how to obtain one. A missing token returns an explicit failure
        naming what to configure; it never fabricates a result."""
        return _checked(
            handlers.gsc_query(
                site_url=site_url,
                mode=mode,
                start_date=start_date,
                end_date=end_date,
                dimensions=dimensions,
                row_limit=row_limit,
                inspection_url=inspection_url,
            )
        )

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_webmaster_url_queries(
        host_id: str,
        url: str | None = None,
        url_contains: str | None = None,
        max_urls: int = 100,
        max_queries_per_url: int = 500,
        start_date: str | None = None,
        end_date: str | None = None,
    ) -> dict[str, Any]:
        """Read bounded Yandex Webmaster URL-to-query evidence for a verified host.

        This is provider data, not crawl evidence. It preserves URL/query statistics without
        summing CTR or average position across pages; caps remain explicit in the response.
        """
        return _checked(
            handlers.webmaster_url_queries(
                host_id=host_id,
                url=url,
                url_contains=url_contains,
                max_urls=max_urls,
                max_queries_per_url=max_queries_per_url,
                start_date=start_date,
                end_date=end_date,
            )
        )

    @mcp.tool(annotations=paid, structured_output=True)
    def seo_miratext_analyze(
        urls: list[str] | None = None,
        texts: list[str] | None = None,
        my: str | None = None,
        hash: str | None = None,
        check_type: str = "url",
        keywords: str | None = None,
        paid: bool = False,
        confirm_paid: bool = False,
        timeout: int = 120,
        top: int = 100,
    ) -> dict[str, Any]:
        """Start or resume Miratext analysis; paid and keyword modes need confirmation."""
        return _checked(
            handlers.miratext_analyze(
                urls=urls,
                texts=texts,
                my=my,
                hash=hash,
                check_type=check_type,
                keywords=keywords,
                paid=paid,
                confirm_paid=confirm_paid,
                timeout=timeout,
                top=top,
            )
        )

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_gsc_archive(
        database: str,
        action: Literal["status", "prepare", "run", "backup"] = "status",
        site_url: str | None = None,
        start_date: str | None = None,
        end_date: str | None = None,
        max_requests: int = 1,
        pause: float = 1.0,
        backup_path: str | None = None,
    ) -> dict[str, Any]:
        """Manage an explicit local GSC SQLite archive. Status is offline and never creates
        an absent archive. Prepare creates/extends the queue for a verified property and
        inclusive YYYY-MM-DD dates without calling Google. Run performs at most max_requests
        API calls (1..1000, default 1), writes checkpoints and obeys quota/retry waits. Backup
        makes a verified snapshot at a new backup_path. Only prepare creates a database.
        Search Analytics can omit anonymized/top-limited rows; never sum different datasets.
        """
        return _checked(
            handlers.gsc_archive(
                database=database,
                action=action,
                site_url=site_url,
                start_date=start_date,
                end_date=end_date,
                max_requests=max_requests,
                pause=pause,
                backup_path=backup_path,
            )
        )

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_crux_report(
        url: str | None = None,
        origin: str | None = None,
        urls: list[str] | None = None,
        form_factor: str | None = None,
        metrics: list[str] | None = None,
        max_samples: int = 25,
        cache_dir: str | None = None,
        cache_max_age_hours: float = 24,
    ) -> dict[str, Any]:
        """Field Core Web Vitals (LCP, INP, CLS) as real Chrome users experienced them, at the
        75th percentile — the honest counterpart to seo_render_check's synthesized-score-free
        design (issue #59). Pass exactly one of url/origin. Requires a Chrome UX Report API key.
        No eligible field record and missing metrics remain unavailable. Optional urls samples
        at most 25 targets; cache_dir enables an explicit local cache. Never substitutes
        Lighthouse lab metrics for CrUX field data."""
        return _checked(
            handlers.crux_report(
                url=url,
                origin=origin,
                urls=urls,
                form_factor=form_factor,
                metrics=metrics,
                max_samples=max_samples,
                cache_dir=cache_dir,
                cache_max_age_hours=cache_max_age_hours,
            )
        )

    @mcp.tool(annotations=submit, structured_output=True)
    def seo_indexnow_submit(
        urls: list[str], host: str, key_location: str | None = None
    ) -> dict[str, Any]:
        """Push up to 10,000 changed URLs to Bing, Yandex, Naver, and Seznam in one call.

        IMPORTANT: Google has not joined IndexNow as of 2026 — this does not affect Google's
        crawl schedule. Requires a self-generated key published at https://<host>/<key>.txt
        before the first call; see docs/SETUP.md. Natural pairing: submit exactly the URLs
        seo_compare_crawls reports as new or changed."""
        return _checked(handlers.indexnow_submit(urls=urls, host=host, key_location=key_location))

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_open(
        directory: str, expected_site: str | None = None, consumer: str | None = None
    ) -> dict[str, Any]:
        """Open a local project workspace without executing template references."""
        return _checked(
            with_project_notice(
                handlers.project_open(directory=directory, expected_site=expected_site),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_new(
        directory: str,
        target: str,
        label: str | None = None,
        facts: list[dict[str, Any]] | None = None,
        template_references: list[str] | None = None,
        profile_references: list[str] | None = None,
    ) -> dict[str, Any]:
        """Create a local project workspace; no crawl, checklist execution, or network work starts."""
        return _checked(
            handlers.project_new(
                directory=directory,
                target=target,
                label=label,
                facts=facts,
                template_references=template_references,
                profile_references=profile_references,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_sources_link(
        directory: str, service: str, resource: str, label: str | None = None
    ) -> dict[str, Any]:
        """Bind a provider resource ID to a local project; no provider call or discovery runs."""
        return _checked(
            handlers.project_sources_link(
                directory=directory, service=service, resource=resource, label=label
            )
        )

    @mcp.tool(annotations=rewrite_files, structured_output=True)
    def seo_project_sources_unlink(directory: str, service: str, resource: str) -> dict[str, Any]:
        """Remove one resource binding from a local project; no provider call runs."""
        return _checked(
            handlers.project_sources_unlink(directory=directory, service=service, resource=resource)
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_sources_list(directory: str) -> dict[str, Any]:
        """List the resource bindings of a local project; reads sources.json only."""
        return _checked(handlers.project_sources_list(directory=directory))

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_status(directory: str, consumer: str | None = None) -> dict[str, Any]:
        """Show project scan history and named pending checklist/preparation states.

        A stable consumer optionally receives a bounded inbox notice.  Reading a
        status never marks notes read or acknowledged.
        """
        kwargs = {"directory": directory}
        consumer = bound_consumer(directory, consumer)
        if consumer is not None:
            kwargs["consumer"] = consumer
        return _checked(handlers.project_status(**kwargs))

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_progress(
        directory: str, limit: int = 20, offset: int = 0, consumer: str | None = None
    ) -> dict[str, Any]:
        """Show a compact, paginated project checklist view and its next actions.

        The page contains at most 100 checklist items. Audit-task completion is a
        percentage only when every included site has an explicit agreed plan and
        the shared coverage axis has a measured, nonzero denominator. It is
        explicitly task completion, not a site-health or remediation percentage.
        """
        kwargs = {"directory": directory, "limit": limit, "offset": offset}
        consumer = bound_consumer(directory, consumer)
        if consumer is not None:
            kwargs["consumer"] = consumer
        return _checked(handlers.project_progress(**kwargs))

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_observe(
        directory: str,
        consumer: str | None = None,
        scan_limit: int = 20,
        run_offset: StrictInt = 0,
        run_limit: StrictInt = 20,
    ) -> dict[str, Any]:
        """Read the bounded project observer snapshot: tasks, methods, competitors,
        retained scan state and the execution-log tail.  It never starts work or
        consumes inbox entries; a consumer only receives its own unread summary.
        Workflow/checklist receipt observations are metadata-only, not fresh byte
        verification. Missing historical receipts remain unverified. Explicit
        project-status and workflow-status retain byte verification by default.
        Run pagination applies independently per site: all stored running records
        plus one terminal page, newest admitted first. run_offset must be nonnegative
        and run_limit 1..100. Totals cover at most 100 retained records per site,
        not lifetime history; earlier evictions were not counted. A changed runs
        revision requires the caller to restart terminal pagination.
        """
        return _checked(
            handlers.project_observe(
                directory=directory,
                consumer=bound_consumer(directory, consumer),
                scan_limit=scan_limit,
                run_offset=run_offset,
                run_limit=run_limit,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_inbox_submit(
        directory: str,
        text: str,
        kind: Literal["note", "proposed_goal", "question"] = "note",
        references: list[str] | None = None,
        author_role: Literal["specialist", "agent"] = "specialist",
        expected_revision: int | None = None,
    ) -> dict[str, Any]:
        """Persist a specialist note, proposed goal or question without starting any work."""
        return _checked(
            handlers.project_inbox_submit(
                directory, text, kind, references, author_role, expected_revision
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_inbox_list(
        directory: str,
        consumer: str,
        offset: int = 0,
        limit: int = 20,
        include_acknowledged: bool = True,
    ) -> dict[str, Any]:
        """List a bounded project inbox page without consuming any entries."""
        return _checked(
            handlers.project_inbox_list(directory, consumer, offset, limit, include_acknowledged)
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_inbox_read(
        directory: str, consumer: str, entry_ids: list[str], expected_revision: int | None = None
    ) -> dict[str, Any]:
        """Record an agent's explicit inspection; acknowledgment remains separate."""
        return _checked(
            handlers.project_inbox_read(directory, consumer, entry_ids, expected_revision)
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_inbox_acknowledge(
        directory: str, consumer: str, entry_ids: list[str], expected_revision: int | None = None
    ) -> dict[str, Any]:
        """Explicitly acknowledge entries.  This never accepts or completes a goal."""
        return _checked(
            handlers.project_inbox_acknowledge(directory, consumer, entry_ids, expected_revision)
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_inbox_goal(
        directory: str,
        entry_id: str,
        state: Literal["accepted", "completed"],
        expected_revision: int | None = None,
    ) -> dict[str, Any]:
        """Explicitly accept or complete a stored proposed goal; no executor is launched."""
        return _checked(handlers.project_inbox_goal(directory, entry_id, state, expected_revision))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_inbox_triage(
        directory: str,
        entry_id: str,
        outcome: dict[str, Any],
        actor: str,
        expected_revision: int | None = None,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Append an explicit task, goal, competitor, blocked, or rejected note outcome.

        This records no automated interpretation, does not read or acknowledge the
        note, and never launches a scan or a provider operation.
        """
        return _checked(
            with_project_notice(
                handlers.project_inbox_triage(
                    directory=directory,
                    entry_id=entry_id,
                    outcome=outcome,
                    actor=actor,
                    expected_revision=expected_revision,
                ),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_inbox_unread(directory: str, consumer: str, limit: int = 10) -> dict[str, Any]:
        """Return a bounded unread reference summary without changing delivery state."""
        return _checked(handlers.project_inbox_unread(directory, consumer, limit))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_event_append(
        directory: str,
        source: Literal["agent", "user", "scans", "app"],
        actor: Literal["user", "agent", "schedule"],
        text: str,
    ) -> dict[str, Any]:
        """Append one structured event to the project journal (events.jsonl).

        Earlier events are never edited. Nothing is executed; the event only records text.
        """
        return _checked(
            handlers.project_event_append(
                directory=directory, source=source, actor=actor, text=text
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_event_page(
        directory: str,
        offset: int = 0,
        limit: int = 50,
        source: Literal["agent", "user", "scans", "app"] | None = None,
        query: str = "",
    ) -> dict[str, Any]:
        """Read a newest-first bounded page of project events, optionally filtered."""
        return _checked(
            handlers.project_event_page(
                directory=directory, offset=offset, limit=limit, source=source, query=query
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_remediation_summary(ledger: str) -> dict[str, Any]:
        """Read explicit remediation and recheck coverage from retained local evidence.

        The result keeps verified original cases, resolved, persisting,
        regressed, false-positive-reviewed and unverifiable states separate.
        It does not run a crawl or infer that omitted evidence is clean.
        """
        return _checked(handlers.remediation_summary(ledger=ledger))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_remediation_create(path: str, project_dir: str, producer_build: str) -> dict[str, Any]:
        """Create one new empty remediation ledger bound to a validated local project.

        producer_build must be the full lowercase 40-character Git SHA of the
        build that writes the ledger. An existing path is refused; ledgers never
        overwrite.
        """
        return _checked(
            handlers.remediation_create(
                path=path, project_dir=project_dir, producer_build=producer_build
            )
        )

    @mcp.tool(annotations=rewrite_files, structured_output=True)
    def seo_remediation_ingest(ledger: str, scan: str) -> dict[str, Any]:
        """Ingest one retained saved audit into a local ledger without rerunning a crawl.

        The scan is opened read-only. Re-ingesting the same revision is idempotent;
        a new revision appends observation history.
        """
        return _checked(handlers.remediation_ingest(ledger=ledger, scan=scan))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_workflow_start(
        directory: str,
        scenario_id: str,
        steps: list[str],
        expected_revision: int = 0,
        context: dict[str, Any] | None = None,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Start a local registered workflow; it performs no scan or provider call."""
        return _checked(
            with_project_notice(
                handlers.workflow_start(directory, scenario_id, steps, expected_revision, context),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_workflow_checkpoint(
        directory: str,
        run_id: str,
        step_id: str,
        state: str,
        evidence: list[dict] | None = None,
        expected_revision: int = 0,
        review: dict[str, Any] | None = None,
        phase: str | None = None,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Persist one registered-step result before the next step or agent handoff."""
        return _checked(
            with_project_notice(
                handlers.workflow_checkpoint(
                    directory, run_id, step_id, state, evidence, expected_revision, review, phase
                ),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_workflow_status(directory: str, consumer: str | None = None) -> dict[str, Any]:
        """Recover the exact next registered step after interruption or handoff."""
        return _checked(
            with_project_notice(handlers.workflow_status(directory), directory, consumer)
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_workflow_execute(
        directory: str,
        scenario_id: str,
        steps: list[str],
        outcomes: list[dict],
        context: dict[str, Any] | None = None,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Checkpoint a supplied local sequence without executing network work."""
        return _checked(
            with_project_notice(
                handlers.workflow_execute(directory, scenario_id, steps, outcomes, context),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_workflow_resume(
        directory: str, run_id: str, expected_revision: int, consumer: str | None = None
    ) -> dict[str, Any]:
        """Reopen only the interrupted registered step after a second-agent handoff."""
        return _checked(
            with_project_notice(
                handlers.workflow_resume(directory, run_id, expected_revision), directory, consumer
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_monitor_configure(
        directory: str, policy: dict, expected_revision: int = 0, consumer: str | None = None
    ) -> dict[str, Any]:
        """Configure a disabled local incremental monitor; this starts no schedule or message delivery."""
        return _checked(
            with_project_notice(
                handlers.monitor_configure(directory, policy, expected_revision),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_monitor_run(
        directory: str,
        scan_id: str,
        observations: list[dict],
        expected_revision: int,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Record one bounded retained-scan diff; quiet runs do not notify anyone."""
        return _checked(
            with_project_notice(
                handlers.monitor_run(directory, scan_id, observations, expected_revision),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_monitor_schedule(
        directory: str, action: str, expected_revision: int, consumer: str | None = None
    ) -> dict[str, Any]:
        """Claim or recover a local run; no timer, crawl, or delivery starts here."""
        return _checked(
            with_project_notice(
                handlers.monitor_schedule(directory, action, expected_revision), directory, consumer
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_schedule_show(directory: str, consumer: str | None = None) -> dict[str, Any]:
        """Read a project's scheduled-scan settings, last run, and next due window."""
        return _checked(with_project_notice(handlers.schedule_show(directory), directory, consumer))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_schedule_set(
        directory: str,
        expected_revision: int,
        settings: dict | None = None,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Merge a partial update into the project's schedule settings; starts nothing."""
        return _checked(
            with_project_notice(
                handlers.schedule_set(directory, expected_revision, settings=settings),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_schedule_run_due(directory: str, consumer: str | None = None) -> dict[str, Any]:
        """Run the project's scheduled scans now if they are due; crawls over the network."""
        return _checked(
            with_project_notice(handlers.schedule_run_due(directory), directory, consumer)
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_schedule_install(
        directory: str,
        kind: str,
        dest: str | None = None,
        python: str | None = None,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Write a launchd plist, cron line, or Task Scheduler command; never loads it."""
        return _checked(
            with_project_notice(
                handlers.schedule_install(directory, kind, dest=dest, python=python),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=rewrite_files, structured_output=True)
    def seo_schedule_uninstall(
        directory: str, kind: str, dest: str | None = None, consumer: str | None = None
    ) -> dict[str, Any]:
        """Remove the trigger file written by seo_schedule_install; does not unload it."""
        return _checked(
            with_project_notice(
                handlers.schedule_uninstall(directory, kind, dest=dest), directory, consumer
            )
        )

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_monitor_collect(
        directory: str, expected_revision: int, apply: bool = False
    ) -> dict[str, Any]:
        """Preview or explicitly collect one previously claimed bounded monitor plan.

        The default preview makes no HTTP request. apply=true collects only the
        validated claim, retains body/validation provenance within its bounds and
        records failed, partial or complete outcomes. Expired or interrupted claims
        require an explicit new start. No timer, background service or delivery starts.
        """
        return _checked(
            handlers.monitor_collect(
                directory=directory, expected_revision=expected_revision, apply=apply
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_monitor_local_deliver(
        directory: str, scan_id: str, expected_revision: int
    ) -> dict[str, Any]:
        """Record a deduplicated local receipt for one retained monitoring run.

        The destination is fixed to local:receipt. It uses no network or external
        transport and does not claim that any recipient outside the project received it.
        """
        return _checked(
            handlers.monitor_local_deliver(
                directory=directory, scan_id=scan_id, expected_revision=expected_revision
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_monitor_status(directory: str, consumer: str | None = None) -> dict[str, Any]:
        """Read local monitor policy and its last retained checkpoint."""
        return _checked(
            with_project_notice(handlers.monitor_status(directory), directory, consumer)
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_remediation_cases(
        ledger: str,
        check: str | None = None,
        url: str | None = None,
        finding_key: str | None = None,
        limit: int = 100,
        offset: int = 0,
        source_scan_id: int | None = None,
        group_ref: str | None = None,
        max_bytes: int | None = None,
    ) -> dict[str, Any]:
        """Read a bounded page of exact remediation cases and decision history."""
        return _checked(
            handlers.remediation_cases(
                ledger=ledger,
                check=check,
                url=url,
                finding_key=finding_key,
                limit=limit,
                offset=offset,
                source_scan_id=source_scan_id,
                group_ref=group_ref,
                max_bytes=max_bytes,
            )
        )

    @mcp.tool(annotations=rewrite_files, structured_output=True)
    def seo_remediation_transition(
        ledger: str,
        occurrence_key: str,
        state: str,
        actor: str,
        reason: str,
        expected_revision: int,
        observation_id: int | None = None,
        decided_at: str | None = None,
    ) -> dict[str, Any]:
        """Append one revision-safe, evidence-bound lifecycle decision.

        Measured outcomes require a later retained observation. A user claim,
        missing source or failed fetch cannot resolve a case.
        """
        return _checked(
            handlers.remediation_transition(
                ledger=ledger,
                occurrence_key=occurrence_key,
                state=state,
                actor=actor,
                reason=reason,
                expected_revision=expected_revision,
                observation_id=observation_id,
                decided_at=decided_at,
            )
        )

    @mcp.tool(annotations=rewrite_files, structured_output=True)
    def seo_remediation_record_verification(
        ledger: str,
        verification_path: str,
        actor: str,
        expected_revision: int,
        occurrence_keys: list[str] | None = None,
        task_id: str = "unassigned",
    ) -> dict[str, Any]:
        """Attach a retained bounded verification artifact to pending cases.

        Every result must map to exactly one pending ledger case. The artifact
        byte digest is saved in the decision evidence; ambiguous or stale
        batches are rejected atomically.
        """
        return _checked(
            handlers.remediation_record_verification(
                ledger=ledger,
                verification_path=verification_path,
                actor=actor,
                expected_revision=expected_revision,
                occurrence_keys=occurrence_keys,
                task_id=task_id,
            )
        )

    @mcp.tool(annotations=rewrite_files, structured_output=True)
    def seo_remediation_recheck(
        ledger: str,
        baseline: str,
        occurrence_keys: list[str],
        actor: str,
        expected_revision: int,
        out_dir: str,
        task_id: str = "unassigned",
        after: str | None = None,
        config: str | None = None,
    ) -> dict[str, Any]:
        """Recheck exact pending ledger cases without widening to a site crawl.

        The baseline must be the retained audit that produced each case. The
        command writes a separate immutable verification artifact, then records
        its hashes and measured outcome atomically in the local ledger.
        """
        return _checked(
            handlers.remediation_recheck(
                ledger=ledger,
                baseline=baseline,
                occurrence_keys=occurrence_keys,
                actor=actor,
                expected_revision=expected_revision,
                out_dir=out_dir,
                task_id=task_id,
                after=after,
                config=config,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_remediation_report(
        ledger: str, out_dir: str | None = None, limit: int = 100, offset: int = 0
    ) -> dict[str, Any]:
        """Render retained before/after remediation evidence without network access.

        With out_dir this creates a new immutable JSON/Markdown review snapshot;
        without it, it returns the JSON-ready report document only.
        """
        return _checked(
            handlers.remediation_report(ledger=ledger, out_dir=out_dir, limit=limit, offset=offset)
        )

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_project_facts(
        directory: str,
        facts: list[dict[str, Any]] | None = None,
        detect: bool = False,
        apply: bool = False,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Preview or record project stack facts that stack-aware priorities then read.

        Supplied facts are operator decisions and always win. detect=true is the only
        thing that makes a request: it reads robots.txt and then fetches the project
        target once, and an unavailable or ambiguous detection leaves the fact absent
        with its reason instead of guessing. The default is a read-only preview;
        apply=true records the result in project.json.
        """
        return _checked(
            with_project_notice(
                handlers.project_facts(
                    directory=directory, facts=facts, detect=detect, apply=apply
                ),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_checklist_init(
        directory: str,
        template: dict | None = None,
        expected_revision: int | None = None,
        plan: dict | None = None,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Initialize or reconcile a local checklist without executing a check, skill, or scenario.

        template is an optional data-only ``seohead.checklist-template.v1`` document. plan is an
        optional agreed audit scope {reviewer, population, tasks}: population declares kind
        (``complete_set``, ``sample`` or ``unknown``), size or enumerated urls, a provenance
        ``source``, an optional ``name`` and ``reason``, and optional per-``templates``
        populations; ``unknown`` keeps the URL denominator null with a reason. Template
        populations are agreed sub-populations of the site population: enumerated
        template URLs must belong to an enumerated site set, and declared template
        membership can never exceed the agreed site size; incoherent plans are refused
        rather than trimmed. tasks is
        ``{kind: all_agreed}`` or a sourced ``{kind: selection, ids, source}`` naming the agreed
        checklist items; items outside a selection stay visible as ``not_agreed`` outside every
        denominator. A size-only population cannot verify measured-URL membership, so its
        numerator counts only enumerated URLs. Recording a plan upgrades the checklist to
        ``seohead.coverage.v3`` and appends to the retained plan history; an identical
        agreement is an idempotent no-op, while a changed agreement starts a new revision and
        stale-marks evidence recorded under an earlier agreement instead of shrinking
        denominators. The result
        returns the current state, revision, counts, coverage axes, views and items; use that
        revision for a later conditional write. This never makes a network request.
        """
        return _checked(
            with_project_notice(
                handlers.project_checklist_init(
                    directory=directory,
                    template=template,
                    expected_revision=expected_revision,
                    plan=plan,
                ),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_checklist_update(
        directory: str, item: dict, expected_revision: int, consumer: str | None = None
    ) -> dict[str, Any]:
        """Add or update one local checklist definition without executing it.

        expected_revision prevents a writer from silently replacing a newer checklist. Built-in
        identity and source provenance remain validated by the project core. This never makes a
        network request.
        """
        return _checked(
            with_project_notice(
                handlers.project_checklist_update(
                    directory=directory, item=item, expected_revision=expected_revision
                ),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_checklist_record(
        directory: str,
        item_id: str,
        record: dict,
        expected_revision: int,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Record supplied evidence for one checklist item without executing its operation.

        expected_revision prevents an overwrite of newer checklist history. The record is
        validated against the item's scope and evidence contract, then the returned status names
        remaining, blocked and manual-review work. Accepted shapes, all with a ``reason``:
        automatic check ``{"status": "succeeded", "reason", "artifact"}`` where artifact is a
        saved scan under ``scans/`` that proves the check ran (scan.v1 or streamed audit.v2);
        attempt ``{"status": "failed" | "unavailable" | "running", "reason"}``; manual signoff
        ``{"status": "succeeded", "reason", "reviewer", "signoff": true}``; reviewed artifact
        ``{"status": "succeeded", "reason", "reviewer", "review": "approved", "artifact"}``
        under ``reports/``; reviewed exclusion ``{"status": "not_applicable", "reason",
        "reviewer", "artifact" or "evidence"}``, which otherwise stays ``pending_exclusion``
        inside the denominator. This never makes a network request.
        """
        return _checked(
            with_project_notice(
                handlers.project_checklist_record(
                    directory=directory,
                    item_id=item_id,
                    record=record,
                    expected_revision=expected_revision,
                ),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_view_list(directory: str, consumer: str | None = None) -> dict[str, Any]:
        """List saved declarative finding views and the current project view-config revision."""
        return _checked(
            with_project_notice(
                handlers.project_view_list(directory=directory), directory, consumer
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_view_show(
        directory: str, name: str, consumer: str | None = None
    ) -> dict[str, Any]:
        """Read one saved finding view with its stable identity, schema version and revision."""
        return _checked(
            with_project_notice(
                handlers.project_view_show(directory=directory, name=name), directory, consumer
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_view_save(
        directory: str, view: dict[str, Any], expected_revision: int, consumer: str | None = None
    ) -> dict[str, Any]:
        """Create or revise a bounded declarative finding view using an expected config revision.

        Filters are closed severity/check/URL/segment selections. Sorting and column projection
        use registered fields only; no SQL, code, or regular expressions are accepted. This
        changes project view configuration only; it does not edit scans or affect scores/tasks."""
        return _checked(
            with_project_notice(
                handlers.project_view_save(
                    directory=directory, view=view, expected_revision=expected_revision
                ),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=rewrite_files, structured_output=True)
    def seo_project_view_delete(
        directory: str, name: str, expected_revision: int, consumer: str | None = None
    ) -> dict[str, Any]:
        """Delete one saved finding view using the current project view-config revision.

        Changes project view configuration only; it does not edit scans or affect scores/tasks."""
        return _checked(
            with_project_notice(
                handlers.project_view_delete(
                    directory=directory, name=name, expected_revision=expected_revision
                ),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_view_rename(
        directory: str,
        name: str,
        new_name: str,
        expected_revision: int,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Rename one saved finding view using the current project view-config revision.

        Keeps the view definition; the new name must be unused. Changes project view configuration
        only; it does not edit scans or affect scores/tasks."""
        return _checked(
            with_project_notice(
                handlers.project_view_rename(
                    directory=directory,
                    name=name,
                    new_name=new_name,
                    expected_revision=expected_revision,
                ),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_findings_view(
        directory: str, name: str, audit: dict | str, offset: int = 0, consumer: str | None = None
    ) -> dict[str, Any]:
        """Apply one saved view to an audit object, JSON file, or validated scan.v1 SQLite artifact.

        Returns a deterministic projected page with total matches, missing-field counts,
        truncation, source identity, and view/config revisions. Filtering never suppresses
        findings or changes audit coverage/scoring; no crawl or provider call occurs."""
        return _checked(
            with_project_notice(
                handlers.findings_view(directory=directory, name=name, audit=audit, offset=offset),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_priorities(
        directory: str,
        policy: dict | None = None,
        apply: bool = False,
        expected_revision: int | None = None,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Preview stack-aware project priorities from saved facts without network requests.

        The default is read-only preview. apply=true requires expected_revision and atomically
        saves policy provenance; it preserves explicit operator choices and completion evidence.
        Read-only status and reports never upgrade coverage schemas. A custom policy is data,
        not executable code; omitted policy uses the packaged defaults.
        """
        return _checked(
            with_project_notice(
                handlers.project_priorities(
                    directory=directory,
                    policy=policy,
                    apply=apply,
                    expected_revision=expected_revision,
                ),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_policy(
        directory: str,
        policy: dict | None = None,
        apply: bool = False,
        expected_revision: int | None = None,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Read or explicitly update operator crawl defaults and project admission thresholds."""
        return _checked(
            with_project_notice(
                handlers.project_policy(
                    directory, policy=policy, apply=apply, expected_revision=expected_revision
                ),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_competitors_add(
        directory: str,
        competitors: list,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Record supplied competitor candidates as separate local workspaces, without crawling.

        Each item needs url, source and observed_at. Scans are started separately per workspace.
        """
        return _checked(
            with_project_notice(
                handlers.project_competitors_add(directory, competitors=competitors),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_competitors_list(directory: str) -> dict[str, Any]:
        """List the recorded competitor candidates and their local workspaces; no change."""
        return _checked(handlers.project_competitors_list(directory))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_project_competitors_remove(directory: str, url: str) -> dict[str, Any]:
        """Drop one recorded competitor from the shortlist. Its local workspace and scans stay
        on disk, untouched; no crawl runs."""
        return _checked(handlers.project_competitors_remove(directory, url=url))

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_project_prepare(
        directory: str,
        template: dict | None = None,
        competitors: list | None = None,
        approve_large_crawl: bool = False,
        producer_build: str | None = None,
        consumer: str | None = None,
        max_urls: int | None = None,
    ) -> dict[str, Any]:
        """Prepare an existing project with a bounded native crawl and saved sitemap coverage.

        Reuses the latest suitable saved scan when one answers the request. ``max_urls`` sets the
        owner's URL cap for a new crawl. Competitors must be supplied candidates with provenance;
        absent sources stay pending. All site checklists remain separate. Paid provider calls are
        never hidden in preparation.
        """
        return _checked(
            with_project_notice(
                handlers.project_prepare(
                    directory,
                    template=template,
                    competitors=competitors,
                    approve_large_crawl=approve_large_crawl,
                    producer_build=producer_build,
                    max_urls=max_urls,
                ),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_project_start(
        directory: str,
        target: str,
        facts: list[dict[str, Any]] | None = None,
        template: dict | None = None,
        competitors: list | None = None,
        approve_large_crawl: bool = False,
        producer_build: str | None = None,
        consumer: str | None = None,
    ) -> dict[str, Any]:
        """Create and prepare a new bounded project; failures leave inspectable pending work."""
        return _checked(
            with_project_notice(
                handlers.project_start(
                    directory,
                    target,
                    facts=facts,
                    template=template,
                    competitors=competitors,
                    approve_large_crawl=approve_large_crawl,
                    producer_build=producer_build,
                ),
                directory,
                consumer,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_skill_list() -> dict[str, Any]:
        """List the packaged, source-derived method playbooks without executing them."""
        return _checked(handlers.skill_list())

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_skill_show(name: str) -> dict[str, Any]:
        """Return a packaged skill's exact text and definition identity."""
        return _checked(handlers.skill_show(name))

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scenario_list() -> dict[str, Any]:
        """List the packaged workflow scenarios with their ordered steps, without running them."""
        return _checked(handlers.scenario_list())

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scenario_show(name: str) -> dict[str, Any]:
        """Return a packaged workflow scenario's text without running its commands."""
        return _checked(handlers.scenario_show(name))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_provider_replay(
        input_path: str,
        evidence_file: str,
        out_dir: str,
        url_column: str = "url",
        review_external_only: bool = False,
    ) -> dict[str, Any]:
        """Join a saved private provider collection to a saved scan with no network; retain raw rows locally and return counts."""
        return _checked(
            handlers.provider_replay(
                input_path=input_path,
                evidence_file=evidence_file,
                out_dir=out_dir,
                url_column=url_column,
                review_external_only=review_external_only,
            )
        )

    @mcp.tool(annotations=submit, structured_output=True)
    def seo_provider_auth(
        provider: str,
        action: Literal[
            "status", "connect", "refresh", "cancel", "disconnect", "revoke"
        ] = "status",
        grant_file: str | None = None,
        confirm: bool = False,
    ) -> dict[str, Any]:
        """Manage GSC read-only OAuth grants: import private file, refresh, cancel a pending flow, or explicitly revoke. Never returns secrets."""
        return _checked(
            handlers.provider_auth(
                provider=provider, action=action, grant_file=grant_file, confirm=confirm
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_provider_registry() -> dict[str, Any]:
        """List provider operations, credential components, quota and privacy boundaries."""
        return _checked(handlers.provider_registry())

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_provider_readiness(
        provider: str | None = None, operation: str | None = None
    ) -> dict[str, Any]:
        """Inspect configured credential sources and declared operation routes offline.

        Credential configuration never proves account or target permission. Use
        seo_provider_verify for an explicit bounded read-only access check.
        """
        return _checked(handlers.provider_readiness(provider=provider, operation=operation))

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_provider_verify(provider: str, request: dict[str, Any] | None = None) -> dict[str, Any]:
        """Explicitly verify bounded read-only account/target access; present credentials are not verification."""
        return _checked(handlers.provider_verify(provider, request))

    @mcp.tool(annotations=paid, structured_output=True)
    def seo_provider_collect(
        provider: str, operation: str, request: dict[str, Any], artifact_dir: str | None = None
    ) -> dict[str, Any]:
        """Collect a declared provider operation with versioned redacted evidence.

        Raw identifiers and rows belong only in an explicit restricted artifact directory.
        Paid operations require their provider's explicit production and cost guards; the
        backlink-index adapter is disabled by default. Collection never implies indexing.
        """
        return _checked(
            handlers.provider_collect(provider, operation, request, artifact_dir=artifact_dir)
        )

    @mcp.tool(annotations=pure, structured_output=True)
    def seo_provider_join(
        crawl_pages: list[dict[str, Any]],
        evidence_rows: list[dict[str, Any]],
        review_external_only: bool = False,
        adjustments: list[dict[str, Any]] | None = None,
    ) -> dict[str, Any]:
        """Join supplied URL evidence exactly and preserve unmatched populations and technical severity."""
        return _checked(
            handlers.provider_join(
                crawl_pages,
                evidence_rows,
                review_external_only=review_external_only,
                adjustments=adjustments,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_evidence_normalize(
        file: str,
        mapping: Any = None,
        sheet: str | None = None,
        site_origin: str | None = None,
        out_dir: str | None = None,
    ) -> dict[str, Any]:
        """Normalize a supplied CSV/XLSX/JSON or saved provider envelope, fully offline.

        Every row keeps its declared grain, provenance and availability state: a
        measured zero stays zero while missing, null, blank, suppressed,
        uncollected and failed inputs stay unavailable. Restricted sources
        return counts and redacted provenance; normalized rows live only in an
        explicit private ``out_dir`` artifact. No provider, DNS or page fetch
        ever runs here.
        """
        return _checked(
            handlers.evidence_normalize(
                file=file,
                mapping=mapping,
                sheet=sheet,
                site_origin=site_origin,
                out_dir=out_dir,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_evidence_join(
        evidence: Any,
        audit: Any = None,
        scan: str | None = None,
        pages: Any = None,
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

        Retains matched, crawl-only, external-only and unkeyable populations
        with per-field provenance, and reports key collisions instead of
        multiplying rows. ``compare`` plus a declared ``policy`` yields a pure
        compatible/incompatible/unknown decision across period, timezone,
        identity, attribution, engine and grain; source metrics such as GSC
        clicks and GA4 sessions stay distinct and are never summed. Restricted
        inputs return counts only.
        """
        return _checked(
            handlers.evidence_join(
                audit=audit,
                scan=scan,
                pages=pages,
                evidence=evidence,
                compare=compare,
                mapping=mapping,
                compare_mapping=compare_mapping,
                policy=policy,
                sheet=sheet,
                compare_sheet=compare_sheet,
                site_origin=site_origin,
                compare_site_origin=compare_site_origin,
                ignore_query=ignore_query,
                ignore_scheme=ignore_scheme,
                casefold_path=casefold_path,
                out_dir=out_dir,
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_bi_filter(
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
        """Filter a verified local BI package into a new typed package and optional XLSX.

        Exact equality filters over declared fields only; source and selected row counts
        and coverage are preserved. Creates new local outputs without network calls.
        """
        return _checked(
            handlers.bi_filter(
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
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_navigation(
        input_path: str, document_id: int | None = None, limit: int = 100, offset: int = 0
    ) -> dict[str, Any]:
        """Read bounded observed navigation evidence from a retained local scan.

        Reads saved local evidence only; does not collect, fetch or modify it.
        """
        return _checked(
            handlers.scan_navigation(
                input_path=input_path, document_id=document_id, limit=limit, offset=offset
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_activity(directory: str) -> dict[str, Any]:
        """Read lightweight current activity for a local project and its sites.

        Reads saved local evidence only; does not collect, fetch or modify it.
        """
        return _checked(handlers.project_activity(directory=directory))

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_checklist_page(
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
        """Read a bounded searchable page of project checklist evidence.

        sort is one of id (stored order, default), priority, state or updated; states
        keeps up to 8 display states. Each item carries updated, the newest record time.
        Reads saved local evidence with metadata-only receipt status; this is not
        fresh byte verification. Missing historical receipts remain unverified.
        Does not collect, fetch or modify evidence.
        """
        return _checked(
            handlers.project_checklist_page(
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
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_task_detail(directory: str, item_id: str) -> dict[str, Any]:
        """Read one project task definition, evidence and bounded history.

        Reads saved local evidence with metadata-only receipt status; this is not
        fresh byte verification. Missing historical receipts remain unverified.
        Does not collect, fetch or modify evidence.
        """
        return _checked(handlers.project_task_detail(directory=directory, item_id=item_id))

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_project_scans(directory: str, offset: int = 0, limit: int = 20) -> dict[str, Any]:
        """Read a bounded page of retained project scans with evidence metadata.

        Reads saved local evidence only; does not collect, fetch or modify it.
        """
        return _checked(handlers.project_scans(directory=directory, offset=offset, limit=limit))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_bi_export(
        out_dir: str,
        scan: str | None = None,
        audit: Any = None,
        provider_joins: list[str] | None = None,
        max_rows_per_file: int = 25_000,
        max_bytes_per_file: int = 8 * 1024 * 1024,
        max_output_bytes: int = 4 * 1024 * 1024 * 1024,
        max_scan_bytes: int = 8 * 1024 * 1024 * 1024,
        search_metric: str | None = None,
        xlsx_out: str | None = None,
        xlsx_dataset: str | None = None,
        xlsx_max_rows_per_sheet: int = 1_048_575,
    ) -> dict[str, Any]:
        """Project a saved scan or audit and optional issue #781 joins into a local typed BI package.

        Reads existing evidence only. CSVs are partitioned deterministically;
        null values retain explicit states and reasons, and output limits fail
        without publishing a partial package. No crawl or provider request runs.
        """
        return _checked(
            handlers.bi_export(
                scan=scan,
                audit=audit,
                provider_joins=provider_joins,
                out_dir=out_dir,
                max_rows_per_file=max_rows_per_file,
                max_bytes_per_file=max_bytes_per_file,
                max_output_bytes=max_output_bytes,
                max_scan_bytes=max_scan_bytes,
                search_metric=search_metric,
                xlsx_out=xlsx_out,
                xlsx_dataset=xlsx_dataset,
                xlsx_max_rows_per_sheet=xlsx_max_rows_per_sheet,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_bi_sheets_plan(package: str, max_cells: int = 10_000_000) -> dict[str, Any]:
        """Preflight a complete local BI package for Sheets without Google access or writes."""
        return _checked(handlers.bi_sheets_plan(package=package, max_cells=max_cells))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_publication_cohorts(
        out_dir: str, document: dict[str, Any] | None = None, file: str | None = None
    ) -> dict[str, Any]:
        """Project saved publication metadata and normalized provider evidence locally.

        Publication, modification and first-observed dates stay distinct. GSC
        and analytics observations stay separately labelled; this makes no
        causal traffic claim and never calls a provider.
        """
        return _checked(handlers.publication_cohorts(document=document, file=file, out_dir=out_dir))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_gsc_progress(
        out_dir: str, document: dict[str, Any] | None = None, file: str | None = None
    ) -> dict[str, Any]:
        """Project saved GSC query evidence into branded/non-branded summaries.

        Matching is supplied by versioned aliases. Missing query rows remain
        unknown, and average-position values are never rank-placement claims.
        """
        return _checked(handlers.gsc_progress(document=document, file=file, out_dir=out_dir))

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_bi_bigquery_plan(
        package: str, dataset: str, operation: str = "replace"
    ) -> dict[str, Any]:
        """Describe an optional BigQuery load offline; it never selects a project or writes data."""
        return _checked(
            handlers.bi_bigquery_plan(package=package, dataset=dataset, operation=operation)
        )

    @mcp.tool(annotations=submit, structured_output=True)
    def seo_bi_destination_apply(
        package: str,
        target: str,
        destination: str,
        operation: str = "replace",
        apply: bool = False,
        reconcile: bool = False,
    ) -> dict[str, Any]:
        """Request a BI destination write; host authorization is required and credentials are never accepted."""
        return _checked(
            handlers.bi_destination_apply(
                package=package,
                target=target,
                destination=destination,
                operation=operation,
                apply=apply,
                reconcile=reconcile,
            )
        )

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_inspect_url(url: str, checks: list[str] | None = None) -> dict[str, Any]:
        """Inspect one URL with bounded metadata/header/robots/redirect/structured/render steps."""
        return _checked(handlers.inspect_url(url=url, checks=checks))

    @mcp.tool(annotations=create_files_from_web, structured_output=True)
    def seo_audit_workflow(
        directory: str,
        action: Literal["status", "start", "prepare", "report"] = "status",
        target: str | None = None,
        competitors: list | None = None,
        template: dict | None = None,
        audit: Any = None,
        fmt: str = "md",
        out: str | None = None,
        approve_large_crawl: bool = False,
        max_urls: int | None = None,
    ) -> dict[str, Any]:
        """Use a closed project workflow: status, bounded start/prepare, or an evidence-backed report.

        A report without ``audit`` is built from the project's latest complete audited scan.
        """
        return _checked(
            handlers.audit_workflow(
                directory=directory,
                action=action,
                target=target,
                competitors=competitors,
                template=template,
                audit=audit,
                fmt=fmt,
                out=out,
                approve_large_crawl=approve_large_crawl,
                max_urls=max_urls,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_tool_catalog(
        query: str = "", limit: int = 10, offset: int = 0, include_arguments: bool = False
    ) -> dict[str, Any]:
        """Search source-derived tool metadata with categories, project awareness, and bounded offset paging."""
        return _checked(
            handlers.tool_catalog(
                query=query, limit=limit, offset=offset, include_arguments=include_arguments
            )
        )

    @mcp.tool(annotations=fetch, structured_output=True)
    def seo_url_check_batch(
        urls: list[str] | None = None,
        checks: list[str] | None = None,
        options: dict[str, Any] | None = None,
        scan: str | None = None,
        offset: int = 0,
        max_urls: int = 50,
    ) -> dict[str, Any]:
        """Run up to 50 URL/check pairs from the supported single-URL checks over a URL list or a page of retained scan URLs. Per-pair network errors remain attached to their input URL."""
        return _checked(
            handlers.url_check_batch(
                urls=urls,
                checks=checks,
                options=options,
                scan=scan,
                offset=offset,
                max_urls=max_urls,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_evidence(
        input_path: str,
        section: Literal[
            "capabilities",
            "corpus",
            "structured",
            "routes",
            "resources",
            "timeline",
            "relations",
            "browser",
            "extraction",
        ] = "capabilities",
        limit: int = 1000,
        offset: int = 0,
    ) -> dict[str, Any]:
        """Read captured evidence, resource windows or the event timeline without fetching or migration."""
        return _checked(
            handlers.scan_evidence(
                input_path=input_path, section=section, limit=limit, offset=offset
            )
        )

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_scan_content_search(
        input_path: str,
        query: str,
        out_dir: str,
        scope: Literal["raw_html", "head_markup", "body_text", "selector_markup"] = "raw_html",
        mode: Literal["contains", "not_contains"] = "contains",
        representation: Literal["static", "rendered"] = "static",
        selector: str | None = None,
        case_sensitive: bool = False,
        include_snippets: bool = False,
        kind: Literal["literal", "regex"] = "literal",
    ) -> dict[str, Any]:
        """Search one closed retained scan offline and create an indexed local NDJSON package.

        The tool never fetches or reconstructs a body. Static and rendered are
        separate requests; a missing body remains unavailable rather than an
        absence. It returns only source identity, counts and output paths.
        ``include_snippets`` is opt-in and redacts common credential-shaped values.
        ``kind="regex"`` treats ``query`` as a regular expression; a document
        that exceeds the per-document regex budget is unavailable, not absent.
        """
        return _checked(
            handlers.scan_content_search(
                input_path=input_path,
                query=query,
                out_dir=out_dir,
                scope=scope,
                mode=mode,
                representation=representation,
                selector=selector,
                case_sensitive=case_sensitive,
                include_snippets=include_snippets,
                kind=kind,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_content_search_page(
        package: str,
        offset: int = 0,
        limit: int = 100,
        status: Literal["matched", "not_matched", "unavailable"] | None = None,
        status_code: int | None = None,
    ) -> dict[str, Any]:
        """Read up to 100 indexed derived content-search records without rereading the scan.

        Optional status or status_code filters the stream; offset then counts matching records.
        """
        return _checked(
            handlers.scan_content_search_page(
                package=package,
                offset=offset,
                limit=limit,
                status=status,
                status_code=status_code,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_extract(
        input_path: str,
        rules: list[dict[str, Any]],
        url: str | None = None,
        representation: str = "static",
        limit: int = 100,
    ) -> dict[str, Any]:
        """Run bounded data-only extraction rules on retained complete bodies, without network or writes."""
        return _checked(
            handlers.scan_extract(
                input_path=input_path,
                rules=rules,
                url=url,
                representation=representation,
                limit=limit,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_structured_blocks(
        input_path: str, url: str, representation: str = "static"
    ) -> dict[str, Any]:
        """Describe one retained page's JSON-LD blocks: line, state, graph findings and source JSON, offline."""
        return _checked(
            handlers.scan_structured_blocks(
                input_path=input_path, url=url, representation=representation
            )
        )

    @mcp.tool(annotations=rewrite_files, structured_output=True)
    def seo_marketing_inventory(
        documents: list[dict[str, Any]],
        cta_selector: str | None = None,
        form_selector: str | None = None,
        id_attributes: list[str] | None = None,
        id_parameters: list[str] | None = None,
        out_dir: str | None = None,
    ) -> dict[str, Any]:
        """Inventory supplied CTA/form DOM occurrences without fetching or submitting forms."""
        return _checked(
            handlers.marketing_inventory(
                documents=documents,
                cta_selector=cta_selector,
                form_selector=form_selector,
                id_attributes=id_attributes,
                id_parameters=id_parameters,
                out_dir=out_dir,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_fragment_links(
        input_path: str,
        state: Literal["resolved", "missing", "skipped"] | None = None,
        representation: Literal["static", "rendered", "legacy_fragment"] | None = None,
        offset: int = 0,
        limit: int = 100,
    ) -> dict[str, Any]:
        """Evaluate every retained fragment anchor offline and page the results.

        Only complete retained HTML/DOM is measured: a missing, truncated,
        unsupported, failed or budget-exhausted body is a named skipped
        occurrence or unavailable source, never a broken fragment. Nothing is
        fetched and the artifact is not modified.
        """
        return _checked(
            handlers.scan_fragment_links(
                input_path=input_path,
                offset=offset,
                limit=limit,
                state=state,
                representation=representation,
            )
        )

    @mcp.tool(annotations=rewrite_files, structured_output=True)
    def seo_scan_requeue(
        input_path: str, where: str, backup_path: str, from_scan: str | None = None
    ) -> dict[str, Any]:
        """Explicitly requeue selected saved URLs in the same SQLite with verified backup and attempt history.

        where is a restricted validated predicate, never arbitrary SQL. This operation can perform
        an explicit write-time v1 to v2 upgrade; readers never upgrade. It makes no network request.
        """
        return _checked(
            handlers.scan_requeue(
                input_path=input_path,
                where=where,
                backup_path=backup_path,
                from_scan=from_scan,
            )
        )

    @mcp.tool(annotations=rewrite_files, structured_output=True)
    def seo_scan_import_urls(input_path: str, urls_file: str, backup_path: str) -> dict[str, Any]:
        """Explicitly import a TXT/CSV/XLSX/XML seed list through stored scope and query guards with backup."""
        return _checked(
            handlers.scan_import_urls(
                input_path=input_path, urls_file=urls_file, backup_path=backup_path
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_list(
        directory: str | None = None, offset: int = 0, limit: int = 100, project: str | None = None
    ) -> dict[str, Any]:
        """List saved SQLite scan metadata without loading retained bodies."""
        return _checked(
            handlers.scan_list(directory=directory, offset=offset, limit=limit, project=project)
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_inspect(
        input_path: str,
        table: str = "pages",
        offset: int = 0,
        limit: int = 100,
        max_bytes: int = 1_048_576,
        columns: list[str] | None = None,
        total: bool = False,
    ) -> dict[str, Any]:
        """Read a bounded, paginated table view from one saved scan.

        ``columns`` projects the named table columns; ``total`` adds the table's row count.
        """
        return _checked(
            handlers.scan_inspect(
                input_path=input_path,
                table=table,
                offset=offset,
                limit=limit,
                max_bytes=max_bytes,
                columns=columns,
                total=total,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_url_detail(
        input_path: str,
        url: str,
        response_offset: int = 0,
        response_limit: int = 10,
        form_offset: int = 0,
        form_limit: int = 20,
        max_bytes: int = 1_048_576,
    ) -> dict[str, Any]:
        """Read one exact native URL's retained headers, redirects, page fields, and forms.

        The query is offline and read-only. Header and query values stay redacted,
        body bytes remain unavailable here, and legacy/Screaming Frog sources
        return named unavailable evidence instead of an empty detail record.
        """
        return _checked(
            handlers.scan_url_detail(
                input_path=input_path,
                url=url,
                response_offset=response_offset,
                response_limit=response_limit,
                form_offset=form_offset,
                form_limit=form_limit,
                max_bytes=max_bytes,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_url_history(project: str, url: str, limit: int = 50) -> dict[str, Any]:
        """State of one exact URL across the retained scans of a project, newest first.

        Read-only. For each of the newest ``limit`` scans returns state (present, absent or
        unavailable), status_code, indexability, title_hash, canonical, redirect_target and
        content_hash; ``number`` counts all scans from 1 for the oldest. Lookup is by exact
        retained URL text through an index, not a scan of the page table.
        """
        return _checked(handlers.scan_url_history(project=project, url=url, limit=limit))

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_url_query(
        input_path: str,
        filters: list[dict[str, Any]] | None = None,
        sort: str | None = None,
        direction: str = "asc",
        columns: list[str] | None = None,
        offset: int = 0,
        limit: int = 200,
        count_timeout_seconds: float = 1.0,
        max_bytes: int = 1_048_576,
        preset: str | None = None,
        facets: list[str] | str | None = None,
        issue_check: str | list[str] | None = None,
        issue_severity: str | None = None,
    ) -> dict[str, Any]:
        """Filter, sort and paginate the page table of one saved scan across the whole scan.

        Read-only. Filters are {column, op, value} objects combined with AND; preset names a
        ready-made filter set (for example status_4xx, title_missing, title_over_60, noindex_meta)
        that is combined with them. Sorting by a non-indexed column needs a filter that leaves at
        most 100,000 rows (else reason_code sort_not_indexed). Returns total, filtered_total (null
        with state capped when the count exceeds count_timeout_seconds) and at most 200 rows of the
        requested columns. facets (a list of group ids, or "all") adds facets: {group: count} over
        the same filters, with facets_state exact or capped. issue_check (1..50 check ids) and
        issue_severity (critical|warning|notice) filter by saved audit findings, including grouped
        URLs. Check ids are case-insensitive. Missing saved findings return state unavailable
        with reason_code issue_index_missing. Skipped, disabled or absent saved checks return
        issue_check_unmeasured; a bounded audit read can return issue_selection_timeout.
        No unfiltered result is substituted.
        """
        return _checked(
            handlers.scan_url_query(
                input_path=input_path,
                filters=filters,
                sort=sort,
                direction=direction,
                columns=columns,
                offset=offset,
                limit=limit,
                count_timeout_seconds=count_timeout_seconds,
                max_bytes=max_bytes,
                facets=facets,
                preset=preset,
                issue_check=issue_check,
                issue_severity=issue_severity,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_link_inspect(
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
    ) -> dict[str, Any]:
        """Inspect saved shortest paths, reverse inlinks, per-link DOM context, or one URL's links.

        view=links pages the outgoing (direction=out) or incoming (direction=in) hyperlinks of
        one URL (`url`, not a document id): anchor, rel with nofollow/sponsored/ugc flags,
        internal/external, position when classified at scan time, and the HTTP status of the
        target (outgoing) or source (incoming); the status is null when that page was not
        scanned. Filters: link_type, follow, status_class (2xx..5xx, broken, error, unscanned),
        contains; sort (out only): order, url, status. At most 200 rows per page with offset;
        `total` and `filtered_total` are exact or flagged capped/null. The scan is opened
        read-only without full validation; failures carry a machine `reason_code`
        (url_not_found, scan_not_available, url_not_scanned, links_not_retained).

        Path hops and inlinks cite exact link IDs and scan identity; absence in a
        partial graph is never a confirmed orphan. Inlinks use a cursor bound to
        scan/revision/target/representation. Context requires link_id or
        document_id, and missing retained bodies return unavailable evidence.
        No network request or scan mutation occurs. Path defaults to 10,000
        visited nodes, 200,000 examined edges, 20 hops and 15 seconds (hard
        maxima 100,000/2,000,000/100/30). Inlinks page at most 500 rows; context
        pages at most 500 rows and one document, with an 8 MiB body hard cap.
        max_bytes bounds serialized item output (4 KiB..8 MiB); an over-budget
        full response returns a named limit result. Invalid scans and URLs are
        error results, not empty or clean graph evidence.
        """
        return _checked(
            handlers.scan_link_inspect(
                input_path=input_path,
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
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_status(input_path: str, full_validation: bool = False) -> dict[str, Any]:
        """Summarize frontier work and committed page outcomes from one saved scan offline.

        The scan is accepted by a light header/schema check (`validation: "light"`);
        full_validation=true runs the complete artifact validation (`validation: "full"`).
        """
        return _checked(
            handlers.scan_status(input_path=input_path, full_validation=full_validation)
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_rendered_routes(input_path: str) -> dict[str, Any]:
        """Read stored static/rendered route evidence without fetching routes."""
        return _checked(handlers.scan_rendered_routes(input_path=input_path))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_scan_snapshot(input_path: str, out: str) -> dict[str, Any]:
        """Create a consistent new SQLite snapshot without overwriting a destination."""
        return _checked(handlers.scan_snapshot(input_path=input_path, out=out))

    @mcp.tool(annotations=create_files, structured_output=True)
    def seo_scan_export(
        input_path: str,
        out: str,
        format: str = "json",
        records: list[str] | None = None,
        fields: dict[str, list[str]] | None = None,
    ) -> dict[str, Any]:
        """Export retained scan data under scan_export.v1 as CSV, XLSX, JSON, or XML."""
        return _checked(
            handlers.scan_export(
                input_path=input_path,
                out=out,
                format=format,
                records=records,
                fields=fields,
            )
        )

    @mcp.tool(annotations=rewrite_files, structured_output=True)
    def seo_scan_pin(input_path: str, pinned: bool = True) -> dict[str, Any]:
        """Pin or unpin a finished scan; this is an explicit metadata mutation."""
        return _checked(handlers.scan_pin(input_path=input_path, pinned=pinned))

    @mcp.tool(annotations=rewrite_files, structured_output=True)
    def seo_scan_prune(
        directory: str | None = None,
        older_than_days: int = 30,
        keep_newest: int = 5,
        plan: dict[str, Any] | None = None,
        apply: bool = False,
        project: str | None = None,
    ) -> dict[str, Any]:
        """Preview candidates by default; deletion requires apply plus the reviewed plan."""
        return _checked(
            handlers.scan_prune(
                directory=directory,
                older_than_days=older_than_days,
                keep_newest=keep_newest,
                plan=plan,
                apply=apply,
                project=project,
            )
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def seo_scan_body_diff(
        left: str,
        right: str,
        url: str,
        variant_key: str | None = None,
        representation: str = "static",
        text: bool = False,
        max_bytes: int = 5 * 1024 * 1024,
        max_lines: int = 10_000,
    ) -> dict[str, Any]:
        """Compare compatible retained bodies offline; a change is not an SEO verdict."""
        return _checked(
            handlers.scan_body_diff(
                left=left,
                right=right,
                url=url,
                variant_key=variant_key,
                representation=representation,
                text=text,
                max_bytes=max_bytes,
                max_lines=max_lines,
            )
        )

    # Register Screaming Frog crawl-export tools on the same local connector.
    from seohead.mcp import sf_mcp

    sf_mcp.register(mcp)
    # Semantic-core tools (seohead semantics) share the same local connector.
    from seohead.mcp import semantics_mcp

    semantics_mcp.register(mcp, _checked)
    from seohead.mcp.mcp_profiles import configure_profile
    from seohead.mcp.mcp_progress import install_progress, wrap_long_tools

    if progress_notifications:
        install_progress(mcp)
        wrap_long_tools(mcp)
    configure_profile(mcp, profile)
    return mcp


def main(profile: str | None = None, progress_notifications: bool = True) -> int:
    """Run the stdio server; return an exit code instead of letting the caller import
    ``mcp`` itself to find out whether the server started.

    ``build_server()`` imports the optional MCP SDK lazily so the rest of the CLI stays
    usable without it (docs/SETUP.md). Before that import runs, no MCP session exists
    and nothing has reached stdout, so a missing SDK is reported as one stderr
    diagnostic naming the install command, with exit code 1 -- instead of an uncaught
    ModuleNotFoundError traceback (#366). This is the single place that diagnostic is
    produced, so both ``seohead mcp`` (seohead/cli/__init__.py) and the direct
    ``python -m seohead.mcp.mcp_server`` invocation advertised above give the same
    outcome.
    """
    from seohead.mcp import mcp_control

    try:
        mcp_control.require_enabled()
        profile = profile or mcp_control.read_state()["profile"]
    except (OSError, ValueError) as exc:
        print(str(exc) if isinstance(exc, ValueError) else "MCP state unavailable", file=sys.stderr)
        return 1
    try:
        build_server(profile=profile, progress_notifications=progress_notifications).run()
    except ModuleNotFoundError:
        # build_server()'s only lazy import is the optional "mcp" SDK (see its own
        # docstring) -- nothing else in this path is optional, so any
        # ModuleNotFoundError reaching here is that one.
        print(
            'seohead mcp requires the optional "mcp" dependency. '
            'Install it with: pip install "seohead-seotools[mcp]"',
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
