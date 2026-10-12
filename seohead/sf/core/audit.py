"""Pipeline orchestrator: ``run_audit`` — the one entry point CLI and MCP share.

It is interface-agnostic and returns an :class:`AuditResult`; serialization is
the reporters' job.
"""

from __future__ import annotations

import os
import re
from collections.abc import Callable
from datetime import datetime, timezone
from typing import Any

from ..config import apply_profile, load_config, validate_config
from ..export_manifest import (
    is_full_manifest,
    requests_from_config,
    resolved_profile,
    validate_loaded_requests,
)
from .aggregate import aggregate
from .context import AuditContext
from .eeat import run_eeat
from .heuristics import run_heuristics
from .inlinks import run_inlinks
from .loader import load_exports
from .models import AuditResult
from .rules import run_rules
from .sitemap_coverage import run_sitemap

CRAWL_MODES = {"crawl", "crawl-list", "load-crawl"}

# The complete input-mode vocabulary any caller (CLI, MCP) may pass to
# ``run_audit``: the live crawl modes above, plus ``parse-exports`` -- reading
# an already-produced export set, which never touches the live-mode branch.
# Declared once here so an interface layer (e.g. sf_mcp) that imports it
# cannot drift from the core by forgetting to mirror a change by hand.
INPUT_MODES = CRAWL_MODES | {"parse-exports"}
_VERSION_RE = re.compile(rb"(\d+\.\d+)")


def detect_seospider_version(path: str) -> str | None:
    """Best-effort: read the version string baked into a .seospider header."""
    try:
        with open(path, "rb") as fh:
            head = fh.read(4096)
    except OSError:
        return None
    if head[:4] != b"\xac\xed\x00\x05":  # Java serialization magic
        return None
    # the version (e.g. 19.4) appears as a UTF string early in the header
    matches = _VERSION_RE.findall(head)
    for m in matches:
        text = m.decode("ascii", "ignore")
        if text.split(".")[0].isdigit() and 1 <= int(text.split(".")[0]) <= 99:
            return text
    return None


def _project_name(source: str | None, pages) -> str:
    if source:
        base = os.path.basename(source)
        cleaned = re.sub(r"^https?[-_]", "", base)
        cleaned = re.sub(r"\.(seospider|dbseospider)$", "", cleaned)
        cleaned = cleaned.rstrip("-_").replace("-", ".")
        if cleaned:
            return cleaned
    for page in pages:
        parts = page.url.split("/")
        if len(parts) >= 3:
            return parts[2]
    return "unknown"


def run_audit(
    *,
    input_mode: str,
    source: str | None = None,
    exports_dir: str | None = None,
    config: dict[str, Any] | None = None,
    config_path: str | None = None,
    config_overrides: dict[str, Any] | None = None,
    url_rewrite: tuple[str, str] | None = None,
    sf_cli: str | None = None,
    sitemap_url: str | None = None,
    profile: str | None = None,
    fetch_all_inlinks: bool | None = None,
    live_recheck: bool | None = None,
    output_dir: str | None = None,
    project_dir: str | None = None,
    inspect_gsc: bool = False,
    log: Callable[[str], None] = print,
    on_started: Callable[[int], None] | None = None,
    on_progress: Callable[[dict], None] | None = None,
) -> AuditResult:
    if type(inspect_gsc) is not bool:
        raise ValueError("inspect_gsc must be a boolean")
    if inspect_gsc and project_dir is None:
        raise ValueError("inspect_gsc requires a project for source binding and quota history")
    cfg = config if config is not None else load_config(config_path)
    # Apply targeted CLI overrides, such as an SF authentication profile.
    for section, values in (config_overrides or {}).items():
        if isinstance(values, dict):
            cfg.setdefault(section, {}).update(values)
        else:
            cfg[section] = values
    if profile:
        cfg["profile"] = profile
    # Expand the profile once, then let explicit flags win over its defaults.
    cfg = apply_profile(cfg)
    if fetch_all_inlinks is not None:
        cfg.setdefault("exports", {})["fetch_all_inlinks"] = fetch_all_inlinks
    if live_recheck is not None:
        cfg.setdefault("live_recheck", {})["enabled"] = live_recheck
    # Domain-only convenience: when crawling a live site, auto-discover its sitemap
    # from robots.txt (unless the user pinned --sitemap or toggled live-recheck).
    if input_mode in CRAWL_MODES and live_recheck is None and not sitemap_url:
        cfg.setdefault("live_recheck", {})["enabled"] = True
    # Every override above is final by this point — validate before anything is
    # crawled or parsed, so a bad config fails loudly instead of shipping a
    # corrupted score (issue #211).
    validate_config(cfg)

    # --- obtain the exports directory ------------------------------------
    sf_version: str | None = None
    sf_run_info: dict[str, Any] = {}
    export_requests = ()
    if input_mode in CRAWL_MODES:
        from .runner import run_sf

        if not source:
            raise ValueError(f"input_mode {input_mode!r} requires a source")
        export_requests = requests_from_config(cfg.get("exports", {}))
        if input_mode == "load-crawl":
            sf_version = detect_seospider_version(source)
        out = output_dir or cfg.get("input", {}).get("exports_dir", "exports")
        exports_dir = run_sf(
            mode=input_mode,
            source=source,
            output_folder=out,
            config=cfg,
            cli_override=sf_cli,
            log=log,
            run_info=sf_run_info,
            on_started=on_started,
            **({"on_progress": on_progress} if on_progress is not None else {}),
        )
        if sf_version is None:
            sf_version = (sf_run_info.get("sf_capability") or {}).get("version")
        log(f"[audit] exports written to {exports_dir}")
        # A Basic-Auth crawl uses the local proxy as its visible origin. Restore
        # the real host so the audit does not analyze 127.0.0.1 URLs.
        if url_rewrite:
            from .auth_proxy import rewrite_exports

            changed = rewrite_exports(exports_dir, url_rewrite[0], url_rewrite[1])
            log(f"[audit] restored original host URLs in {changed} proxy-derived exports")
    else:  # parse-exports
        exports_dir = exports_dir or cfg.get("input", {}).get("exports_dir", "exports")

    # --- load + audit -----------------------------------------------------
    resolved = resolved_profile(cfg.get("profile", "full"), cfg.get("exports", {}))
    derive_sitemap_status = input_mode in CRAWL_MODES and is_full_manifest(cfg.get("exports", {}))
    exports = load_exports(exports_dir, derive_sitemap_status=derive_sitemap_status)
    if input_mode in CRAWL_MODES:
        validate_loaded_requests(
            exports, export_requests, derive_sitemap_status=derive_sitemap_status
        )
        manifest = sf_run_info.get("export_manifest")
        if manifest is not None:
            manifest["state"] = "complete"
            manifest["loaded_keys"] = list(exports.found)
            manifest["derived_from"] = dict(exports.derived_from)
    log(f"[audit] loaded exports: {', '.join(exports.found)}")
    ctx = AuditContext(exports, cfg)
    log(f"[audit] {len(ctx.pages)} URLs ({len(ctx.html_pages())} HTML)")

    # Declared-missing evidence is skipped before any check runs, so a check
    # that never fired cannot be mistaken for a check that found nothing.
    from seohead.sf.core.rules import native_check_ids

    ctx.skip_unsupported(set(exports.frames), native=native_check_ids(ctx))
    for check_id in ("MIXED_LINK_REL", "OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE"):
        ctx.skip(
            check_id,
            "export input retains no full rel-token/context evidence; native retained links are required",
        )
    # Fragment targets are resolved inside the retained destination document
    # (issue #827); an export keeps no destination DOM inventory, so the check
    # must be named unavailable rather than silently land in "ran clean".
    ctx.skip(
        "BROKEN_BOOKMARK",
        "export input retains no HTML/DOM bodies; fragment targets are measured "
        "only from a native retained scan",
    )

    run_rules(ctx)
    run_inlinks(ctx)
    run_eeat(ctx)
    size_stats = run_heuristics(ctx)
    sitemap_summary = run_sitemap(ctx, sitemap_url=sitemap_url)

    run_meta = {
        "project": _project_name(source, ctx.pages),
        "input_mode": input_mode,
        "source": source,
        "exports_dir": exports_dir,
        "sf_version_detected": sf_version,
        "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "profile": resolved if input_mode in CRAWL_MODES else cfg.get("profile"),
        "exports_used": exports.found,
        "exports_missing": exports.missing,
        # A cp1251 export decodes with no exception (#160) -- the codec is the
        # only place left for a reviewer to spot a mojibake'd title instead of
        # trusting that a successful run read the bytes correctly.
        "exports_encodings": exports.encodings,
    }
    if input_mode in CRAWL_MODES:
        rate_info = sf_run_info.get("rate_limit") or {}
        run_meta.update(
            {
                "profile_requested": cfg.get("profile"),
                "exports_derived": exports.derived_from,
                "sf_export_manifest": sf_run_info.get("export_manifest"),
                "sf_capability": sf_run_info.get("sf_capability"),
                "sf_rate_limit": rate_info,
                "effective_max_requests_per_second": (
                    rate_info.get("effective_urls_per_second")
                    if rate_info.get("state") in {"verified", "verified_from_base"}
                    else rate_info.get("state", "unknown")
                ),
            }
        )
    from itertools import islice

    from .aggregate import PARTIAL_CRAWL_RATIO, _crawl_validity
    from .provider_findings import (
        MAX_CRAWL_PAGES,
        emit_provider_issues,
        project_provider_findings,
    )

    included_pages = []
    omitted_pages = 0
    crawl_partial = False
    if project_dir is not None:
        page_count = len(ctx.pages)
        included_pages = [
            {"url": page.url, "indexability": page.indexability}
            for page in islice(ctx.pages, MAX_CRAWL_PAGES)
        ]
        omitted_pages = max(0, page_count - len(included_pages))
        html_count = len(ctx.html_pages())
        crawl_valid, _ = _crawl_validity(html_count, {}, page_count)
        urls_in_sitemap = int((sitemap_summary or {}).get("urls_in_sitemap") or 0)
        sitemap_partial = bool(
            urls_in_sitemap and crawl_valid and page_count < urls_in_sitemap * PARTIAL_CRAWL_RATIO
        )
        crawl_partial = sitemap_partial or omitted_pages > 0
    try:
        if project_dir is None:
            provider_evidence = {
                "schema_version": "provider_findings.v1",
                "state": "unavailable",
                "reason": "no validated project selected for provider bindings and retained history",
                "sources": {},
            }
        else:
            provider_evidence = project_provider_findings(
                included_pages,
                project_dir=project_dir,
                crawl_partial=crawl_partial,
                crawl_pages_omitted=omitted_pages,
                inspect_gsc=inspect_gsc,
            )
    except Exception:
        provider_evidence = {
            "schema_version": "provider_findings.v1",
            "state": "unavailable",
            "reason": "retained provider history could not be read",
            "sources": {},
        }
    emit_provider_issues(ctx, provider_evidence)
    result = aggregate(ctx, run_meta, size_stats, sitemap_summary)
    result.summary["provider_findings"] = provider_evidence
    log(
        f"[audit] {result.summary['totals']['issues_total']} issues "
        f"(critical={result.summary['by_severity']['critical']}, "
        f"warning={result.summary['by_severity']['warning']}, "
        f"notice={result.summary['by_severity']['notice']})"
    )
    return result
