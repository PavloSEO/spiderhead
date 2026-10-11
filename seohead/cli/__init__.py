"""Unified ``seohead`` CLI over the toolkit's shared handler layer.

Usage:
    seohead <command> [--input '<json>'] [convenience flags]
    seohead sf <run|tasks|doctor> ...   # audit Screaming Frog crawl data
    seohead mcp            # run the MCP server (stdio)

Primary input is --input '<json>' (an object mapped onto the handler kwargs) or
piped stdin JSON; a few convenience flags are also accepted. Output is pretty JSON
to stdout. Exit codes are documented in one place: docs/USAGE.md's "Input conventions".
"""

from __future__ import annotations

import argparse
import contextlib
import json
import sys
import zipfile
from pathlib import Path
from typing import TYPE_CHECKING, Any

from seohead import __version__
from seohead.core import runlog

if TYPE_CHECKING:  # imported for the annotation only; the CLI keeps its imports lazy
    from seohead.crawl.progress import CrawlProgress
from seohead.mcp import handlers

MAX_CRUX_EVIDENCE_BYTES = 2 * 1024 * 1024

# command -> handler kwarg builder. Each maps CLI namespace + --input dict -> kwargs.
COMMANDS = (
    "parse",
    "crawl-site",
    "crawl-describe-settings",
    "scan-reanalyze",
    "scan-graph",
    "scan-sitemap-generate",
    "log-scan",
    "crawl-diagnose",
    "crawl-diagnose-export",
    "compare-crawls",
    "verify-fixes",
    "crawl-enrich",
    "crawl-import",
    "segment-diff",
    "redirects-generate",
    "redirects-check",
    "sitemap-crawl",
    "images-download",
    "images-optimize",
    "keywords-cluster",
    "robots-check",
    "headers-check",
    "asset-weight-check",
    "links-check",
    "hreflang-check",
    "domain-profile",
    "cdn-check",
    "tech-detect",
    "security-check",
    "backlinks-check",
    "backlinks-profile",
    "backlinks-disavow",
    "schema-check",
    "schema-build",
    "duplicate-check",
    "ai-bots-check",
    "mirror-check",
    "llms-txt-check",
    "citability-check",
    "markdown-extract",
    "boilerplate-report",
    "semantic-inputs",
    "semantic-similarity",
    "meta-description-drafts",
    "ai-column",
    "social-meta-check",
    "soft404-check",
    "log-analyze",
    "logs-import",
    "logs-summary",
    "logs-join",
    "logs-bots",
    "regions-check",
    "render-check",
    "site-audit",
    "report-build",
    "facts-export",
    "keywords-expand",
    "keywords-seasonality",
    "keywords-exact",
    "serp-fetch",
    "spend-report",
    "sources-doctor",
    "sources-sync",
    "sources-status",
    "sources-export",
    "regions-tree",
    "topvisor-read",
    "metrika-counters",
    "metrika-setup",
    "metrika-report",
    "metrika-traffic-pdf",
    "google-keywords",
    "dataforseo-estimate",
    "google-serp",
    "wayback-history",
    "crtsh-subdomains",
    "cloudflare-traffic",
    "gsc-query",
    "webmaster-url-queries",
    "miratext-analyze",
    "gsc-archive",
    "crux-report",
    "indexnow-submit",
    "scan-list",
    "scan-inspect",
    "scan-url-detail",
    "scan-url-query",
    "scan-url-history",
    "scan-link-inspect",
    "scan-status",
    "scan-rendered-routes",
    "scan-content-search",
    "scan-content-search-page",
    "scan-snapshot",
    "scan-export",
    "scan-pin",
    "scan-prune",
    "scan-body-diff",
    "project-new",
    "project-open",
    "project-update",
    "project-delete",
    "project-settings",
    "project-settings-proposals",
    "project-settings-review",
    "project-action-log",
    "project-status",
    "project-progress",
    "project-sources-link",
    "project-sources-unlink",
    "project-sources-list",
    "remediation-create",
    "remediation-ingest",
    "remediation-summary",
    "remediation-cases",
    "remediation-transition",
    "remediation-record-verification",
    "remediation-recheck",
    "remediation-report",
    "project-observe",
    "project-inbox-submit",
    "project-inbox-list",
    "project-inbox-read",
    "project-inbox-acknowledge",
    "project-inbox-goal",
    "project-inbox-triage",
    "project-inbox-unread",
    "project-event-append",
    "project-event-page",
    "workflow-start",
    "workflow-checkpoint",
    "workflow-status",
    "workflow-execute",
    "workflow-resume",
    "monitor-configure",
    "monitor-run",
    "monitor-collect",
    "monitor-local-deliver",
    "monitor-status",
    "monitor-schedule",
    "schedule-show",
    "schedule-set",
    "schedule-run-due",
    "schedule-install",
    "schedule-uninstall",
    "project-facts",
    "project-checklist-init",
    "project-checklist-update",
    "project-checklist-record",
    "project-priorities",
    "project-view-list",
    "project-view-show",
    "project-view-save",
    "project-view-delete",
    "project-view-rename",
    "findings-view",
    "project-policy",
    "project-prepare",
    "project-start",
    "project-competitors-add",
    "project-competitors-list",
    "project-competitors-remove",
    "skill-list",
    "skill-show",
    "scenario-list",
    "scenario-show",
    "provider-replay",
    "provider-auth",
    "provider-registry",
    "provider-readiness",
    "provider-verify",
    "provider-collect",
    "provider-join",
    "evidence-normalize",
    "evidence-join",
    "bi-export",
    "bi-filter",
    "scan-navigation",
    "project-activity",
    "project-checklist-page",
    "project-task-detail",
    "project-scans",
    "publication-cohorts",
    "gsc-progress",
    "bi-sheets-plan",
    "bi-bigquery-plan",
    "bi-destination-apply",
    "inspect-url",
    "audit-workflow",
    "tool-catalog",
    "scan-evidence",
    "scan-extract",
    "marketing-inventory",
    "scan-fragment-links",
    "scan-structured-blocks",
    "scan-requeue",
    "scan-import-urls",
)

# These are real top-level CLI entry points but deliberately do not belong in
# ``COMMANDS``: that registry is the one-to-one shared core handler/MCP surface.
# The terminal shell has neither a generic handler nor an MCP equivalent.
INTERACTIVE_COMMANDS = ("tui", "watch")

# Public-doc checks need to distinguish an actual CLI entry point from an
# unknown spelling without pretending every entry point is an MCP tool.  The
# namespace entries own subcommand parsers; ``mcp`` and the interactive shell
# own process/session behavior rather than a shared handler.
DOCUMENTED_CLI_ENTRYPOINTS = (
    "logs",
    "sf",
    "semantics",
    "mcp",
    "scan",
    "project",
    "crawl-profile",
    *INTERACTIVE_COMMANDS,
)

# Tools whose complete direct CLI input can be supplied by one --url flag.
URL_COMMANDS = (
    "crawl-site",
    "parse",
    "redirects-check",
    "sitemap-crawl",
    "robots-check",
    "headers-check",
    "asset-weight-check",
    "links-check",
    "hreflang-check",
    "cdn-check",
    "tech-detect",
    "security-check",
    "schema-check",
    "mirror-check",
    "schema-build",
    "ai-bots-check",
    "llms-txt-check",
    "citability-check",
    "markdown-extract",
    "social-meta-check",
    "soft404-check",
    "render-check",
)


# Wait briefly for piped stdin before deciding that no JSON input is coming.
# A zero wait races with `echo '{...}' | seohead parse`; an unlimited wait hangs a command launched
# by a script or CI job whose pipe is open but empty. The latter occurred in a real site-audit run.
STDIN_WAIT_SECONDS = 0.2

_SCAN_PATH_COMMANDS = frozenset(
    {
        "scan-inspect",
        "scan-url-detail",
        "scan-url-query",
        "scan-link-inspect",
        "scan-status",
        "scan-rendered-routes",
        "scan-snapshot",
        "scan-pin",
        "scan-reanalyze",
    }
)


def _is_json_object(value: str) -> bool:
    """Whether a legacy-ambiguous value is an inline JSON object, not a path."""
    try:
        return isinstance(json.loads(value), dict)
    except json.JSONDecodeError:
        return False


def _rewrite_deprecated_scan_flags(argv: list[str] | None) -> tuple[list[str] | None, list[str]]:
    """Keep old saved-scan flags working without registering two ``--input`` meanings.

    ``--input`` is JSON for every command.  The five scan readers used it as a
    path before #701, so rewrite an old path-shaped value to ``--scan`` before
    argparse sees it.  JSON objects retain the common meaning; ``--json-input``
    is the explicit deprecated spelling for that same JSON value.
    """
    if argv is None:
        argv = sys.argv[1:]
    rewritten = list(argv)
    warnings: list[str] = []
    command = None
    for index, token in enumerate(rewritten):
        if token == "scan" and index + 1 < len(rewritten):
            command = f"scan-{rewritten[index + 1]}"
            continue
        if token in _SCAN_PATH_COMMANDS:
            command = token
            continue
        if not command or not command.startswith("scan-"):
            continue
        if token == "--json-input":
            rewritten[index] = "--input"
            warnings.append("--json-input is deprecated; use --input for inline JSON")
        elif token.startswith("--json-input="):
            rewritten[index] = "--input=" + token.removeprefix("--json-input=")
            warnings.append("--json-input is deprecated; use --input for inline JSON")
        elif command in _SCAN_PATH_COMMANDS and token == "--input" and index + 1 < len(rewritten):
            value = rewritten[index + 1]
            if not _is_json_object(value):
                rewritten[index] = "--scan"
                warnings.append("--input FILE is deprecated for scan artifacts; use --scan FILE")
        elif command in _SCAN_PATH_COMMANDS and token.startswith("--input="):
            value = token.removeprefix("--input=")
            if not _is_json_object(value):
                rewritten[index] = "--scan=" + token.removeprefix("--input=")
                warnings.append("--input FILE is deprecated for scan artifacts; use --scan FILE")
    return rewritten, list(dict.fromkeys(warnings))


def _configure_windows_streams() -> None:
    """Keep redirected CLI JSON UTF-8 on Windows' legacy console defaults."""
    if sys.platform != "win32":
        return
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if callable(reconfigure):
            with contextlib.suppress(OSError, ValueError):
                reconfigure(encoding="utf-8")


def _stdin_has_data() -> bool:
    if sys.stdin is None or sys.stdin.closed or sys.stdin.isatty():
        return False
    try:
        import select

        ready, _, _ = select.select([sys.stdin], [], [], STDIN_WAIT_SECONDS)
        return bool(ready)
    except (ImportError, OSError, ValueError):
        # Some file-like objects do not support select (notably replaced stdin in tests). Fall back
        # to reading them directly; those controlled streams deliver EOF immediately.
        return True


# These flags already identify the input source. When any is present, never inspect stdin. Otherwise
# `while read u; do seohead parse --url "$u"; done < urls.txt` loses the remainder of the file on
# its first iteration: regular-file stdin is always reported as ready, so the command consumes every
# unread URL and the loop processes only one.
#
def _json_list(text: str) -> list[Any]:
    try:
        value = json.loads(text)
    except ValueError as exc:
        raise argparse.ArgumentTypeError(f"not valid JSON: {exc}") from exc
    if not isinstance(value, list):
        raise argparse.ArgumentTypeError("expected a JSON list")
    return value


def _source_flag(sub: argparse.ArgumentParser, *args: str, **kwargs: Any) -> argparse.Action:
    """Add a flag whose value alone supplies a command's complete input (a URL, a file path, a
    search phrase, a counter ID, ...) and register it on that command's parser. Keeping the
    destinations with their parser prevents an identically named flag on another command from
    changing whether this command reads stdin.
    """
    action = sub.add_argument(*args, **kwargs)
    source_flags = set(sub.get_default("_source_flags") or ())
    source_flags.add(action.dest)
    sub.set_defaults(_source_flags=frozenset(source_flags))
    return action


def _has_source_flag(args: argparse.Namespace) -> bool:
    return any(getattr(args, name, None) for name in getattr(args, "_source_flags", ()))


def _load_input(raw: str | None, allow_stdin: bool = True) -> dict[str, Any]:
    if raw:
        return json.loads(raw)
    if allow_stdin and _stdin_has_data():
        data = sys.stdin.read().strip()
        if data:
            return json.loads(data)
    return {}


def _split_list(val: str | None) -> list[str] | None:
    """Parse a comma-separated list while preserving quoted commas inside an item.

        --queries "technical SEO,site audit"       -> two items
        --queries "'SEO audit, enterprise sites'"  -> one item with its comma preserved

    Search queries and page titles commonly contain commas. Silently splitting such a value would
    change the requested input and, for paid providers, could issue two billable calls instead of one.
    """
    if not val:
        return None
    import csv
    from io import StringIO

    # The CSV parser handles quoting while retaining commas inside fields.
    for quote in ("'", '"'):
        if quote in val:
            rows = list(csv.reader(StringIO(val), quotechar=quote, skipinitialspace=True))
            if rows:
                return [s.strip() for s in rows[0] if s.strip()]
    return [s.strip() for s in val.split(",") if s.strip()]


def _build_kwargs(cmd: str, args: argparse.Namespace) -> tuple[str, dict[str, Any]]:
    """Return (handler_name, kwargs) for a command from flags + --input JSON."""
    data = _load_input(getattr(args, "input", None), allow_stdin=not _has_source_flag(args))
    if cmd == "gsc-archive" and not isinstance(data, dict):
        raise ValueError("gsc-archive input must be a JSON object")
    handler_name = cmd.replace("-", "_")
    kw: dict[str, Any] = dict(data)  # --input is the base; flags override/augment

    if cmd == "scan-reanalyze":
        for flag in ("input_path", "out", "producer_build", "step", "max_documents"):
            value = getattr(args, flag, None)
            if value is not None:
                kw[flag] = value
    elif cmd in {"scan-graph", "scan-sitemap-generate"}:
        for flag in (
            "input_path",
            "positions",
            "section",
            "around",
            "depth",
            "aggregate",
            "representation",
            "node_offset",
            "edge_offset",
            "node_limit",
            "edge_limit",
            "max_nodes",
            "max_edges",
            "compare_to",
            "timeout_seconds",
            "max_bytes",
            "out_dir",
            "sitemap_base_url",
            "exclude",
        ):
            value = getattr(args, flag, None)
            if value is not None:
                kw[flag] = value
        if getattr(args, "include_images", False):
            kw["include_images"] = True
    elif cmd == "project-inbox-triage":
        for name in ("entry_id", "actor", "expected_revision"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        if getattr(args, "directory", None):
            kw["directory"] = args.directory
    elif cmd in {
        "scan-evidence",
        "scan-extract",
        "scan-fragment-links",
        "scan-structured-blocks",
        "scan-requeue",
        "scan-import-urls",
    }:
        for name in (
            "input_path",
            "section",
            "state",
            "limit",
            "offset",
            "where",
            "backup_path",
            "from_scan",
            "urls_file",
            "url",
            "representation",
        ):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd == "scan-content-search":
        for name in (
            "input_path",
            "query",
            "out_dir",
            "scope",
            "mode",
            "representation",
            "selector",
            "kind",
        ):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
        for name in ("case_sensitive", "include_snippets"):
            if getattr(args, name, False):
                kw[name] = True
    elif cmd == "scan-content-search-page":
        for name in ("package", "offset", "limit", "status", "status_code"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd == "scan-url-query":
        for name in (
            "input_path",
            "filters",
            "sort",
            "direction",
            "columns",
            "offset",
            "limit",
            "count_timeout_seconds",
            "max_bytes",
            "facets",
            "preset",
            "export",
            "export_format",
            "export_max_rows",
            "issue_check",
            "issue_severity",
        ):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd == "scan-url-history":
        for name in ("project", "url", "limit"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd == "scan-url-detail":
        for name in (
            "input_path",
            "url",
            "response_offset",
            "response_limit",
            "form_offset",
            "form_limit",
            "max_bytes",
        ):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd == "inspect-url":
        if getattr(args, "url", None):
            kw["url"] = args.url
    elif cmd == "audit-workflow":
        for name in ("directory", "action", "target", "out", "fmt"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd == "tool-catalog":
        for name in ("query", "limit"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
        if getattr(args, "include_arguments", False):
            kw["include_arguments"] = True
    elif cmd == "parse":
        if args.url:
            kw["url"] = args.url
        if args.urls:
            kw["urls"] = _split_list(args.urls)
    elif cmd == "redirects-generate":
        kw.setdefault("redirects", data.get("redirects", []))
        if args.format:
            kw["fmt"] = args.format
    elif cmd == "redirects-check":
        if args.url:
            kw["url"] = args.url
    elif cmd == "crawl-site":
        if args.url:
            kw["url"] = args.url
        if getattr(args, "urls", None):
            kw["urls"] = _split_list(args.urls)
        if getattr(args, "urls_file", None):
            kw["urls_file"] = args.urls_file
        if getattr(args, "project", None):
            kw["project"] = args.project
        if getattr(args, "approve_large_crawl", False):
            kw["approve_large_crawl"] = True
        if getattr(args, "user_agent", None):
            kw["user_agent"] = args.user_agent
        if getattr(args, "observer_run_id", None):
            kw["observer_run_id"] = args.observer_run_id
        if getattr(args, "sitemap_only", False):
            kw["sitemap_only"] = True
        for flag in (
            "config",
            "max_urls",
            "max_depth",
            "min_delay",
            "out_dir",
            "robots",
            "sitemap",
            "scan_out",
            "producer_build",
            "resume",
        ):
            value = getattr(args, flag, None)
            if value is not None:
                kw[flag] = value
        if getattr(args, "profile", None):
            if getattr(args, "config", None):
                raise ValueError("use --config or --profile, not both")
            from seohead.crawl import profiles

            kw["config"] = profiles.path_for(args.profile)
        from seohead.crawl import settings as crawl_config

        overrides: dict[str, Any] = {}
        rate = getattr(args, "max_urls_per_second", None)
        if rate is not None:
            overrides["speed.min_delay_seconds"] = crawl_config.delay_for_request_rate(rate)
        for assignment in getattr(args, "set_settings", None) or ():
            path, value = crawl_config.parse_setting_assignment(assignment)
            overrides[path] = value
        if overrides:
            kw["overrides"] = overrides
    elif cmd == "sitemap-crawl":
        if args.project is not None:
            kw["project"] = args.project
        if args.url:
            kw["url"] = args.url
        if args.concurrency is not None:
            kw["concurrency"] = args.concurrency
    elif cmd == "images-download":
        if args.urls:
            kw["urls"] = _split_list(args.urls)
        if args.output_dir:
            kw["output_dir"] = args.output_dir
    elif cmd == "images-optimize":
        if args.files:
            kw["files"] = _split_list(args.files)
        settings = dict(kw.get("settings") or {})
        if getattr(args, "output_dir", None):
            settings["out_dir"] = args.output_dir
        for key in ("format", "quality", "max_width", "max_height", "max_pixels"):
            value = getattr(args, key, None)
            if value is not None:
                settings[key] = value
        if getattr(args, "in_place", False):
            settings["in_place"] = True
        if getattr(args, "overwrite", False):
            settings["overwrite"] = True
        kw["settings"] = settings
    elif cmd == "keywords-cluster":
        pass  # keywords/algorithm come from --input JSON
    elif cmd == "duplicate-check":
        if getattr(args, "scan", None):
            kw["scan"] = args.scan
        if getattr(args, "threshold", None) is not None:
            kw["threshold"] = args.threshold
        if getattr(args, "fingerprints", False):
            kw["with_fingerprints"] = True
        if getattr(args, "all_pages", False):
            kw["only_indexable"] = False
        # items[] is intentionally accepted through --input JSON.
    elif cmd in {
        "schedule-show",
        "schedule-set",
        "schedule-run-due",
        "schedule-install",
        "schedule-uninstall",
    }:
        for name in ("directory", "expected_revision", "kind", "dest", "python"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    elif cmd in {"monitor-collect", "monitor-local-deliver"}:
        for name in ("directory", "expected_revision", "scan_id", "apply"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    elif cmd in {
        "workflow-start",
        "workflow-checkpoint",
        "workflow-status",
        "workflow-execute",
        "workflow-resume",
        "monitor-configure",
        "monitor-run",
        "monitor-status",
        "monitor-schedule",
    }:
        if getattr(args, "directory", None):
            kw["directory"] = args.directory
        context = kw.get("context")
        if (
            cmd == "workflow-start"
            and getattr(args, "goal_id", None)
            and (context is None or isinstance(context, dict))
        ):
            kw["context"] = {**(context or {}), "goal_id": args.goal_id}
    elif cmd in {
        "remediation-create",
        "remediation-ingest",
        "remediation-summary",
        "remediation-cases",
        "remediation-transition",
        "remediation-record-verification",
        "remediation-recheck",
        "remediation-report",
    }:
        for name in (
            "path",
            "project_dir",
            "producer_build",
            "scan",
            "ledger",
            "check",
            "url",
            "finding_key",
            "occurrence_key",
            "state",
            "actor",
            "reason",
            "expected_revision",
            "observation_id",
            "decided_at",
            "out_dir",
            "verification_path",
            "baseline",
            "after",
            "config",
            "task_id",
            "source_scan_id",
            "group_ref",
            "max_bytes",
        ):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        if cmd in {"remediation-cases", "remediation-report"}:
            kw["limit"] = args.limit
            kw["offset"] = args.offset
        if cmd in {"remediation-record-verification", "remediation-recheck"} and getattr(
            args, "occurrence_keys", None
        ):
            kw["occurrence_keys"] = _split_list(args.occurrence_keys)
    elif cmd in {
        "project-new",
        "project-open",
        "project-update",
        "project-delete",
        "project-settings",
        "project-settings-proposals",
        "project-settings-review",
        "project-status",
        "project-progress",
        "project-facts",
        "project-checklist-init",
        "project-checklist-update",
        "project-checklist-record",
        "project-priorities",
        "project-policy",
        "project-prepare",
        "project-start",
        "project-competitors-add",
        "project-competitors-list",
        "project-competitors-remove",
    }:
        for name in ("directory", "target", "label", "expected_site"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        if cmd == "project-progress":
            kw["limit"] = args.limit
            kw["offset"] = args.offset
        if cmd == "project-settings-proposals":
            kw["limit"] = args.limit
            kw["offset"] = args.offset
        if cmd == "project-settings-review":
            kw["proposal_id"] = args.proposal_id
            kw["decision"] = args.decision
        if getattr(args, "expected_revision", None) is not None:
            kw["expected_revision"] = args.expected_revision
        if cmd == "project-prepare" and args.max_urls is not None:
            kw["max_urls"] = args.max_urls
        if getattr(args, "item_id", None) is not None:
            kw["item_id"] = args.item_id
        if cmd in {"project-priorities", "project-policy", "project-facts"} and getattr(
            args, "apply", False
        ):
            kw["apply"] = True
        if getattr(args, "detect", False):
            kw["detect"] = True
        if getattr(args, "approve_large_crawl", False):
            kw["approve_large_crawl"] = True
        if getattr(args, "producer_build", None):
            kw["producer_build"] = args.producer_build
        if cmd == "project-competitors-remove":
            kw["url"] = args.url
    elif cmd.startswith("project-sources-"):
        for name in ("directory", "service", "resource", "label"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    elif cmd.startswith("project-inbox-"):
        for name in (
            "directory",
            "text",
            "kind",
            "consumer",
            "entry_id",
            "state",
            "author_role",
            "expected_revision",
        ):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        if getattr(args, "references", None):
            kw["references"] = _split_list(args.references)
        if getattr(args, "entry_ids", None):
            kw["entry_ids"] = _split_list(args.entry_ids)
        if getattr(args, "offset", None) is not None:
            kw["offset"] = args.offset
        if getattr(args, "limit", None) is not None:
            kw["limit"] = args.limit
        if getattr(args, "unacknowledged_only", False):
            kw["include_acknowledged"] = False
    elif cmd == "project-event-append":
        for name in ("directory", "source", "actor", "text"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd == "project-event-page":
        for name in (
            "directory",
            "source",
            "offset",
            "limit",
            "query",
            "since_revision",
            "follow",
            "wait_seconds",
        ):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd == "project-action-log":
        for name in ("directory", "offset", "limit", "client", "outcome", "query"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    elif cmd == "project-observe":
        for name in ("directory", "consumer", "scan_limit", "run_offset", "run_limit"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd in {"skill-show", "scenario-show"}:
        if getattr(args, "name", None) or getattr(args, "playbook_name", None):
            kw["name"] = getattr(args, "name", None) or args.playbook_name
    elif cmd == "provider-replay":
        for name in ("input_path", "evidence_file", "out_dir", "url_column"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
        if getattr(args, "review_external_only", False):
            kw["review_external_only"] = True
    elif cmd == "provider-auth":
        for name in ("provider", "action", "grant_file"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
        if getattr(args, "confirm", False):
            kw["confirm"] = True
    elif cmd in {"provider-verify", "provider-collect"}:
        for name in ("provider", "operation", "artifact_dir"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd == "evidence-normalize":
        for name in ("file", "mapping", "sheet", "site_origin", "out_dir"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd == "evidence-join":
        for name in (
            "scan",
            "audit",
            "pages",
            "evidence",
            "compare",
            "mapping",
            "compare_mapping",
            "policy",
            "sheet",
            "compare_sheet",
            "site_origin",
            "compare_site_origin",
            "out_dir",
        ):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
        for name in ("ignore_query", "ignore_scheme", "casefold_path"):
            if getattr(args, name, False):
                kw[name] = True
    elif cmd in {
        "project-activity",
        "project-checklist-page",
        "project-task-detail",
        "project-scans",
        "scan-navigation",
        "bi-filter",
    }:
        for name in (
            "directory",
            "item_id",
            "offset",
            "limit",
            "query",
            "kind",
            "state",
            "sort",
            "descending",
            "states",
            "input_path",
            "document_id",
            "package",
            "dataset",
            "out_dir",
            "max_rows_per_file",
            "max_bytes_per_file",
            "max_output_bytes",
            "xlsx_out",
            "xlsx_max_rows_per_sheet",
        ):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        if getattr(args, "where", None) is not None:
            kw["where"] = json.loads(args.where)
        if getattr(args, "columns", None) is not None:
            kw["columns"] = _split_list(args.columns)
    elif cmd == "bi-export":
        for name in (
            "scan",
            "audit",
            "out_dir",
            "max_rows_per_file",
            "max_bytes_per_file",
            "max_output_bytes",
            "max_scan_bytes",
            "search_metric",
            "xlsx_out",
            "xlsx_dataset",
            "xlsx_max_rows_per_sheet",
        ):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
        if getattr(args, "provider_join", None):
            kw["provider_joins"] = args.provider_join
    elif cmd in {"bi-sheets-plan", "bi-bigquery-plan", "bi-destination-apply"}:
        for name in (
            "package",
            "max_cells",
            "dataset",
            "target",
            "destination",
            "operation",
            "apply",
            "reconcile",
        ):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd in {"publication-cohorts", "gsc-progress"}:
        for name in ("file", "out_dir"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd in {
        "boilerplate-report",
        "semantic-inputs",
        "semantic-similarity",
        "meta-description-drafts",
    }:
        if getattr(args, "scan", None):
            kw["scan"] = args.scan
        if cmd == "semantic-similarity":
            for name in ("cache_path", "threshold", "max_candidate_comparisons"):
                value = getattr(args, name, None)
                if value is not None:
                    kw[name] = value
        if cmd == "meta-description-drafts":
            for name in ("checkpoint_path", "batch_size", "json_path", "csv_path"):
                value = getattr(args, name, None)
                if value is not None:
                    kw[name] = value
        # items[]/pages[] and content_area are intentionally accepted through --input JSON.
    elif cmd == "ai-column":
        if getattr(args, "scan", None):
            kw["scan"] = args.scan
        for name in ("prompt", "column", "max_pages", "csv_path"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        # items[], urls[] and rows[] are accepted through --input JSON, like the drafts command.
    elif cmd == "log-analyze":
        for name in ("path", "max_lines", "offset", "limit", "max_dns", "bot", "ip"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        if getattr(args, "verify_bots", False):
            kw["verify_bots"] = True
    elif cmd == "logs-import":
        for name in ("path", "project", "max_lines", "max_files"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    elif cmd == "logs-summary":
        for name in (
            "source",
            "view",
            "limit",
            "offset",
            "bot",
            "family",
            "ip",
            "date_from",
            "date_to",
            "status",
            "section",
        ):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    elif cmd == "logs-join":
        for name in (
            "source",
            "scan",
            "view",
            "base_url",
            "family",
            "bot",
            "date_from",
            "date_to",
            "limit",
            "offset",
        ):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    elif cmd == "logs-bots":
        for name in ("source", "limit", "offset", "bot", "family", "ip", "verdict", "max_dns"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        if getattr(args, "verify_bots", False):
            kw["verify_bots"] = True
    elif cmd == "site-audit":
        if args.url:
            kw["url"] = args.url
        if getattr(args, "urls", None):
            kw["urls"] = _split_list(args.urls)
        if getattr(args, "limit", None) is not None:
            kw["limit"] = args.limit
        if getattr(args, "concurrency", None) is not None:
            kw["concurrency"] = args.concurrency
        if getattr(args, "render", False):
            kw["render"] = True
        if getattr(args, "skip", None):
            kw["skip"] = _split_list(args.skip)
        if getattr(args, "crux_evidence", None):
            with Path(args.crux_evidence).open("rb") as stream:
                content = stream.read(MAX_CRUX_EVIDENCE_BYTES + 1)
            if len(content) > MAX_CRUX_EVIDENCE_BYTES:
                raise ValueError("CrUX evidence file exceeds the 2 MiB input limit")
            kw["crux_evidence"] = json.loads(content.decode("utf-8"))
        if getattr(args, "report", None):
            kw["_report"] = args.report
            kw["_out"] = getattr(args, "out", None)
    elif cmd == "report-build":
        if "format" in kw:
            # --input JSON uses the CLI spelling; the handler parameter is fmt.
            kw["fmt"] = kw.pop("format")
        if getattr(args, "pdf_policy", None) is not None:
            kw["pdf_policy"] = args.pdf_policy
        if getattr(args, "audit", None):
            kw["audit"] = args.audit
        if getattr(args, "format", None):
            kw["fmt"] = args.format
        if getattr(args, "out", None):
            kw["out"] = args.out
        if getattr(args, "project", None):
            kw["project"] = args.project
        lang = getattr(args, "lang", "en")
        if getattr(args, "format", None) == "pdf" or lang != "en":
            kw["lang"] = lang
        if getattr(args, "view", None):
            kw["view"] = args.view
        if getattr(args, "offset", None) is not None:
            kw["offset"] = args.offset
    elif cmd == "project-view-list":
        if getattr(args, "directory", None):
            kw["directory"] = args.directory
    elif cmd == "project-view-show":
        for name in ("directory", "name"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd == "project-view-save":
        for name in ("directory", "expected_revision"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd in {"project-view-delete", "project-view-rename"}:
        for name in ("directory", "name", "new_name", "expected_revision"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd == "findings-view":
        for name in ("directory", "name", "audit", "offset"):
            if getattr(args, name, None) is not None:
                kw[name] = getattr(args, name)
    elif cmd == "log-scan":
        if getattr(args, "run", None):
            kw["run"] = args.run
        if getattr(args, "images_dir", None):
            kw["images_dir"] = args.images_dir
    elif cmd == "crawl-diagnose":
        for name in ("scan", "run", "max_decisions"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    elif cmd == "crawl-diagnose-export":
        for name in ("scan", "run", "max_decisions", "export"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    elif cmd == "compare-crawls":
        if getattr(args, "compression", None) is not None:
            kw["compression"] = args.compression
        if getattr(args, "out_dir", None) is not None:
            kw["out_dir"] = args.out_dir
        if getattr(args, "before", None):
            kw["before"] = args.before
        if getattr(args, "after", None):
            kw["after"] = args.after
        if getattr(args, "correspondence", None):
            kw["correspondence"] = args.correspondence
        if getattr(args, "force", False):
            kw["force"] = True
    elif cmd == "verify-fixes":
        for name in ("baseline", "view", "urls_file", "after", "config", "out_dir"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        for flag, key in (("finding_ids", "finding_ids"), ("urls", "urls")):
            value = getattr(args, flag, None)
            if value:
                kw[key] = _split_list(value)
    elif cmd == "crawl-enrich":
        for name in (
            "audit",
            "external_csv",
            "url_column",
            "out_urls",
            "visits_column",
            "bounce_column",
        ):
            value = getattr(args, name, None)
            if value:
                kw[name] = value
        for name in ("ignore_query", "ignore_scheme", "casefold_path"):
            if getattr(args, name, False):
                kw[name] = True
    elif cmd == "crawl-import":
        if getattr(args, "manifest", None):
            kw["manifest_path"] = args.manifest
    elif cmd == "segment-diff":
        if getattr(args, "audit", None):
            kw["audit"] = args.audit
        if getattr(args, "source", None):
            kw["source"] = args.source
        if getattr(args, "target", None):
            kw["target"] = args.target
    elif cmd == "regions-check":
        if args.url:
            kw["url"] = args.url
        if getattr(args, "extra", None):
            kw["extra"] = _split_list(args.extra)
        if getattr(args, "limit", None) is not None:
            kw["limit"] = args.limit
        if getattr(args, "render", False):
            kw["render"] = True
    elif cmd == "domain-profile":
        if args.domain:
            kw["domain"] = args.domain
        if getattr(args, "no_tls", False):
            kw["with_tls"] = False
    elif cmd == "backlinks-check":
        if args.target:
            kw["target"] = args.target
        donors = _split_list(args.donors) or []
        if args.donors_file:
            donors += _read_donors(args.donors_file)
        if donors:
            kw["donors"] = donors
        if args.concurrency is not None:
            kw["concurrency"] = args.concurrency
    if cmd == "keywords-expand":
        if args.phrase:
            kw["phrase"] = args.phrase
        if args.limit is not None:
            kw["limit"] = args.limit
        if getattr(args, "regions", None):
            kw["regions"] = _split_list(args.regions)
    if cmd == "keywords-seasonality":
        for name in ("phrase", "from_date", "to_date", "period"):
            value = getattr(args, name, None)
            if value:
                kw[name] = value
        if getattr(args, "regions", None):
            kw["regions"] = _split_list(args.regions)
    if cmd == "keywords-exact":
        if getattr(args, "keywords", None):
            kw["keywords"] = _split_list(args.keywords)
        if getattr(args, "region", None) is not None:
            kw["region"] = args.region
        if getattr(args, "no_wait", False):
            kw["wait"] = False
    if cmd == "serp-fetch":
        if getattr(args, "query", None):
            kw["query"] = args.query
        if getattr(args, "queries", None):
            kw["queries"] = _split_list(args.queries)
        if getattr(args, "region", None):
            kw["region"] = str(args.region)
        if getattr(args, "top", None) is not None:
            kw["top"] = args.top
    if cmd in {"google-keywords", "google-serp"}:
        if getattr(args, "confirm_paid", False):
            kw["confirm_paid"] = True
        for name in ("env", "spend_ceiling_usd"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    if cmd == "dataforseo-estimate":
        if getattr(args, "refresh_prices", False):
            kw["refresh_prices"] = True
        for name in ("operation", "query_count", "row_limit", "cache_hits", "env", "query"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    if cmd == "backlinks-profile":
        for name in (
            "domain",
            "project",
            "db",
            "follow",
            "state",
            "target_prefix",
            "min_spam_score",
            "max_spam_score",
            "limit",
            "offset",
        ):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        if getattr(args, "history_only", False):
            kw["history_only"] = True
        if getattr(args, "one_per_domain", False):
            kw["one_per_domain"] = True
    if cmd == "backlinks-disavow":
        for name in ("domains", "urls"):
            value = getattr(args, name, None)
            if value:
                kw[name] = _split_list(value)
        if getattr(args, "output", None):
            kw["output"] = args.output
    if cmd == "google-keywords":
        if getattr(args, "keywords", None):
            kw["keywords"] = _split_list(args.keywords)
        for name in ("seed", "language", "country"):
            value = getattr(args, name, None)
            if value:
                kw[name] = value
        if getattr(args, "location_code", None) is not None:
            kw["location_code"] = args.location_code
        if getattr(args, "limit", None) is not None:
            kw["limit"] = args.limit
        if getattr(args, "difficulty", False):
            kw["difficulty"] = True
    if cmd == "google-serp":
        for name in ("query", "language", "country"):
            value = getattr(args, name, None)
            if value:
                kw[name] = value
        if getattr(args, "location_code", None) is not None:
            kw["location_code"] = args.location_code
        if getattr(args, "depth", None) is not None:
            kw["depth"] = args.depth
    if cmd == "wayback-history":
        if args.url:
            kw["url"] = args.url
        for name in ("limit", "from_date", "to_date"):
            value = getattr(args, name, None)
            if value:
                kw[name] = value
    if cmd == "crtsh-subdomains" and args.domain:
        kw["domain"] = args.domain
    if cmd == "cloudflare-traffic":
        for name in ("zone", "since", "until"):
            value = getattr(args, name, None)
            if value:
                kw[name] = value
    if cmd == "gsc-query":
        if args.site_url:
            kw["site_url"] = args.site_url
        for name in ("mode", "start_date", "end_date", "inspection_url"):
            value = getattr(args, name, None)
            if value:
                kw[name] = value
        if getattr(args, "dimensions", None):
            kw["dimensions"] = _split_list(args.dimensions)
        if getattr(args, "row_limit", None):
            kw["row_limit"] = args.row_limit
    if cmd == "webmaster-url-queries":
        for name in (
            "host_id",
            "url",
            "url_contains",
            "max_urls",
            "max_queries_per_url",
            "start_date",
            "end_date",
        ):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    if cmd == "gsc-archive":
        for name in (
            "database",
            "action",
            "site_url",
            "start_date",
            "end_date",
            "max_requests",
            "pause",
            "backup_path",
        ):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    if cmd == "crux-report":
        if getattr(args, "url", None):
            kw["url"] = args.url
        if getattr(args, "origin", None):
            kw["origin"] = args.origin
        if getattr(args, "urls", None):
            kw["urls"] = _split_list(args.urls)
        if getattr(args, "form_factor", None):
            kw["form_factor"] = args.form_factor
        if getattr(args, "metrics", None):
            kw["metrics"] = _split_list(args.metrics)
        for name in ("max_samples", "cache_dir", "cache_max_age_hours"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    if cmd == "indexnow-submit":
        if getattr(args, "urls", None):
            kw["urls"] = _split_list(args.urls)
        if getattr(args, "host", None):
            kw["host"] = args.host
        if getattr(args, "key_location", None):
            kw["key_location"] = args.key_location
    if cmd == "scan-list":
        if getattr(args, "project", None):
            kw["project"] = args.project
        if getattr(args, "directory", None):
            kw["directory"] = args.directory
        for name in ("offset", "limit"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    if cmd == "scan-inspect":
        if getattr(args, "input_path", None):
            kw["input_path"] = args.input_path
        for name in ("table", "offset", "limit", "max_bytes", "columns"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        if getattr(args, "total", False):
            kw["total"] = True
    if cmd == "scan-link-inspect":
        if getattr(args, "input_path", None):
            kw["input_path"] = args.input_path
        for name in (
            "view",
            "seed",
            "target",
            "representation",
            "cursor",
            "link_id",
            "document_id",
            "offset",
            "limit",
            "max_bytes",
            "max_body_bytes",
            "max_nodes",
            "max_edges",
            "max_depth",
            "timeout_seconds",
            "url",
            "direction",
            "link_type",
            "follow",
            "status_class",
            "contains",
            "sort",
        ):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    if cmd in {"scan-status", "scan-rendered-routes"} and getattr(args, "input_path", None):
        kw["input_path"] = args.input_path
    if cmd == "scan-snapshot":
        if getattr(args, "input_path", None):
            kw["input_path"] = args.input_path
        if getattr(args, "out", None):
            kw["out"] = args.out
    if cmd == "scan-export":
        for name in ("input_path", "out", "format", "records", "fields"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
    if cmd == "scan-pin":
        if getattr(args, "input_path", None):
            kw["input_path"] = args.input_path
        if getattr(args, "unpin", False):
            kw["pinned"] = False
    if cmd == "scan-prune":
        if getattr(args, "project", None):
            kw["project"] = args.project
        if getattr(args, "directory", None):
            kw["directory"] = args.directory
        for name in ("older_than_days", "keep_newest", "plan"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        if getattr(args, "apply", False):
            kw["apply"] = True
    if cmd == "scan-body-diff":
        for name in (
            "left",
            "right",
            "url",
            "variant_key",
            "representation",
            "max_bytes",
            "max_lines",
        ):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        if getattr(args, "text", False):
            kw["text"] = True
    if cmd == "metrika-setup" and getattr(args, "counter", None):
        kw["counter_id"] = args.counter
    if cmd == "metrika-report":
        if getattr(args, "counter", None):
            kw["counter_id"] = args.counter
        for name in ("metrics", "dimensions", "date1", "date2", "filters", "sort"):
            value = getattr(args, name, None)
            if value:
                kw[name] = value
        if getattr(args, "limit", None) is not None:
            kw["limit"] = args.limit
        if getattr(args, "paginate", False):
            kw["paginate"] = True
    if cmd == "metrika-traffic-pdf":
        if getattr(args, "counter", None):
            kw["counter_id"] = args.counter
        for name in (
            "document",
            "date1",
            "date2",
            "out_dir",
            "attribution",
            "traffic",
            "filters",
            "lang",
            "site_label",
            "brand",
            "gsc_site_url",
        ):
            value = getattr(args, name, None)
            if value:
                kw[name] = value
        for name in ("top", "timeout"):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        if getattr(args, "no_render", False):
            kw["render"] = False
        if getattr(args, "no_pdf", False):
            kw["pdf"] = False
        if getattr(args, "overwrite", False):
            kw["overwrite"] = True
    if cmd == "regions-tree" and getattr(args, "save_to", None):
        kw["save_to"] = args.save_to
    if cmd == "spend-report" and getattr(args, "since", None):
        kw["since"] = args.since
    if cmd == "spend-report" and getattr(args, "csv", None):
        kw["csv_path"] = args.csv
    if cmd in {"sources-sync", "sources-status", "sources-export"}:
        for name in (
            "source",
            "resource",
            "start_date",
            "end_date",
            "match",
            "limit",
            "out",
            "db",
            "project",
        ):
            value = getattr(args, name, None)
            if value is not None:
                kw[name] = value
        if getattr(args, "force", False):
            kw["force"] = True
    if cmd in URL_COMMANDS:
        if args.url:
            kw["url"] = args.url
        if cmd == "links-check" and getattr(args, "internal_only", False):
            kw["internal_only"] = True
        if cmd == "security-check" and getattr(args, "probe_paths", False):
            kw["probe_paths"] = True
        if cmd == "schema-build" and getattr(args, "type", None):
            kw["override_type"] = args.type
        if cmd == "render-check":
            if getattr(args, "viewport", None):
                kw["viewport"] = args.viewport
            if getattr(args, "wait", None):
                kw["wait"] = args.wait
            if getattr(args, "user_agent", None):
                kw["user_agent"] = args.user_agent
            if (
                getattr(args, "browser_transport", None)
                or getattr(args, "remote_endpoint_env", None)
                or getattr(args, "remote_playwright_version", None)
            ):
                kw["transport_config"] = {
                    "transport": args.browser_transport or "local",
                    "remote_protocol": "playwright",
                    "remote_endpoint_env": args.remote_endpoint_env or "",
                    "remote_playwright_version": args.remote_playwright_version or "",
                }
        if cmd == "llms-txt-check" and getattr(args, "brand", None):
            kw["brand"] = args.brand
    return handler_name, kw


def _print_config_help() -> None:
    """List every crawl-site config setting, generated from seohead.crawl.settings.

    One source of truth: this reads the same DEFAULTS/DESCRIPTIONS that the
    config file loader validates against, so a setting cannot be added to the
    module without becoming visible here.
    """
    from seohead.crawl import settings as crawl_settings

    print(
        "Crawler config settings (seohead/crawl/settings.py). Set these in a JSON file passed to "
        '--config, e.g. {"limits": {"max_urls": 50}}.'
    )
    print()
    for setting in crawl_settings.describe_settings():
        marker = "*" if setting["results_affecting"] else " "
        print(f"{marker} {setting['path']} ({setting['type']}, default {setting['default']!r})")
        print(f"      {setting['description']}")
    print()
    print("* changes what the audit finds; recorded in the run manifest.")


def _crawl_overrides(kwargs: dict[str, Any]) -> dict[str, Any]:
    """The overrides a crawl-site run will resolve, in the precedence the handler applies.

    Shared by everything here that has to know what a run's settings resolve to
    before the run happens: two copies of this precedence would be two chances
    for the number printed to describe a run that is not the one about to
    happen -- worse than printing nothing, because it is believed.
    """
    overrides = dict(kwargs.get("overrides") or {})
    # Only a named argument that was actually given wins. Updating with None
    # would erase a --set or --max-urls-per-second value and silently fall
    # back to the default, which is how a rate cap becomes a no-op.
    for path, value in (
        ("limits.max_urls", kwargs.get("max_urls")),
        ("limits.max_depth", kwargs.get("max_depth")),
        ("speed.min_delay_seconds", kwargs.get("min_delay")),
        ("speed.concurrency", kwargs.get("concurrency")),
        ("robots.policy", kwargs.get("robots")),
        ("output.dir", kwargs.get("out_dir")),
    ):
        if value is not None:
            overrides[path] = value
    if kwargs.get("user_agent") not in (None, ""):
        from seohead.crawl.user_agents import resolve as resolve_user_agent

        identity, token = resolve_user_agent(kwargs["user_agent"])
        overrides["http.user_agent"] = identity
        overrides.setdefault("robots.user_agent_token", token)
    return overrides


def _project_crawl_defaults(kwargs: dict[str, Any]) -> dict | None:
    if not kwargs.get("project"):
        return None
    from seohead.projects.runtime import project_policy

    return project_policy(kwargs["project"])["policy"]["crawl_overrides"]


def _print_effective_rate(kwargs: dict[str, Any]) -> None:
    """Print the worst-case requests/second a crawl-site run permits, before it runs.

    Politeness is a combination of settings, not one knob (#14): printing the
    derived number at startup, on stderr so the JSON result on stdout stays
    clean, is what lets an operator catch a dangerous combination before the
    crawl — rather than only from a killed process or a struggling site — and
    it is what `--config`'s values actually resolve to, not just what the file
    or flags said in isolation.
    """
    from seohead.crawl import settings as crawl_config

    try:
        # The same overrides the handler will resolve, in the same precedence, or
        # the printed rate describes a run that is not the one about to happen --
        # which is worse than printing nothing, because it is believed.
        resolved = crawl_config.load(
            kwargs.get("config"),
            overrides=_crawl_overrides(kwargs),
            base_overrides=_project_crawl_defaults(kwargs),
        )
    except crawl_config.ConfigError:
        return  # the handler call below reports the same error to the user
    rate = crawl_config.effective_request_rate(resolved)
    shown = "unbounded" if rate == float("inf") else f"{rate:.2f} req/s"
    print(f"crawl-site: effective worst-case request rate to one host: {shown}", file=sys.stderr)
    for message in crawl_config.rate_warnings(resolved):
        print(f"crawl-site: warning: {message}", file=sys.stderr)


# Stops the crawl chose because it was told to, not ones a resume can get past: the
# limit is part of the effective configuration a resume reads back from the artifact.
BUDGET_STOPS = {
    "url_limit": "The URL budget (limits.max_urls)",
    "duration_limit": "The crawl-time budget (limits.max_crawl_seconds)",
    "request_limit": "The total HTTP-attempt budget (limits.max_requests)",
}


def _print_crawl_outcome(result: Any) -> None:
    """Say on exit whether the crawl finished or stopped early, and why.

    A crawl that stopped at a limit, an error circuit or an interruption is not a
    shorter complete crawl: every count it produced describes part of a site. The
    JSON already carries that in ``partial``/``finish_reason``, but an operator
    watching a long run in a terminal reads the last line, not the document -- so
    this repeats it there, and names the command that continues the run rather
    than restarting it.

    On stderr, like the request-rate line above it, so the JSON document on stdout
    stays exactly what a pipeline parses.
    """
    if not isinstance(result, dict) or "finish_reason" not in result:
        return  # --config-help, an error already reported, or a non-crawl result
    fetched = result.get("urls_collected")
    scan = result.get("scan")
    if result.get("partial"):
        reason = result.get("stopped_reason") or result.get("finish_reason") or "reason unrecorded"
        line = f"crawl-site: stopped early ({reason}); {fetched} URLs fetched"
        budget = BUDGET_STOPS.get(result.get("finish_reason"))
        if budget:
            # A resume applies the artifact's own recorded settings, so it cannot
            # get past a budget those settings set: pointing at --resume here would
            # be advice to run the same no-op again.
            line += f". {budget} was reached, and a resume continues under it: "
            line += "raise it and crawl to a new scan to go further"
        elif scan:
            line += f". Continue it with: seohead crawl-site --resume {scan}"
    else:
        line = f"crawl-site: finished; {fetched} URLs fetched"
        if result.get("resumed"):
            line += " (this run continued an earlier one)"
    if result.get("audit_available") is False:
        line += f"; audit unavailable: {result.get('audit_reason') or 'reason unrecorded'}"
    print(line, file=sys.stderr)
    _print_body_retention(result)


def _print_body_retention(result: Any) -> None:
    """Say how many fetched HTML bodies and rendered DOMs the run threw away, and why.

    A run that keeps a fifth of the bodies reports its pages and links exactly like
    one that kept them all; the only trace was ``html_bodies`` (or, in the rendering
    lane, ``rendered_bodies``) turning ``partial`` in the capabilities map, which
    reads as an ordinary caveat and says nothing about how much is gone (#647, #656).
    Everything derived from stored HTML or a stored DOM -- duplicate detection, DOM
    shape, boilerplate share, offline reanalysis -- then covers the retained pages
    while every page-level percentage beside it covers all of them, so the two
    populations have to be named where an operator will read them.

    Silent when nothing was discarded: a line on every run is a line nobody reads.
    A crawl that escalated to rendering carries both counts, so both are checked.
    """
    if not isinstance(result, dict):
        return
    _print_retention_line(result.get("html_bodies"), "fetched HTML page bodies", "stored HTML")
    _print_retention_line(result.get("rendered_bodies"), "rendered DOMs", "a stored rendered DOM")


def _print_retention_line(retention: Any, unit: str, stored_as: str) -> None:
    if not isinstance(retention, dict) or not retention.get("omitted"):
        return
    omitted = retention["omitted"]
    total, retained = retention["total"], retention["retained"]
    reasons = ", ".join(f"{reason} {count}" for reason, count in sorted(omitted.items()))
    print(
        f"crawl-site: {total - retained} of {total} {unit} were not retained "
        f"({reasons}). Anything computed from {stored_as} covers "
        f"{retained} of {total} pages",
        file=sys.stderr,
    )


def _crawl_progress(kwargs: dict[str, Any]) -> CrawlProgress | None:
    """Build the live progress line for a crawl-site run, or None when it has no place to go.

    The budget it prints against is the one this run will actually apply, read
    back from the same resolved settings ``_print_effective_rate`` uses -- a
    percentage computed against the default 200 on a run configured for 40 000
    would be a number the operator has no way to know is wrong.

    A configuration this run cannot load returns None rather than raising: the
    handler call that follows reports that error properly, and a progress line
    is never worth turning a clear message into a traceback.
    """
    import sqlite3

    from seohead.crawl import settings as crawl_config
    from seohead.crawl.progress import CrawlProgress

    resume = kwargs.get("resume")
    try:
        if resume:
            # A resume runs under the settings stored in its artifact, so the budget
            # to measure against is the stored one; the flags that would have set a
            # different one are refused by the handler rather than applied.
            from seohead.mcp.scan_handlers import resume_inputs

            resolved = resume_inputs(resume)["settings"]
        else:
            resolved = crawl_config.load(
                kwargs.get("config"),
                overrides=_crawl_overrides(kwargs),
                base_overrides=_project_crawl_defaults(kwargs),
            )
    except (crawl_config.ConfigError, OSError, ValueError, sqlite3.Error):
        return None
    stream = sys.stderr
    return CrawlProgress(
        stream,
        budget=resolved["limits"]["max_urls"],
        # A pipe or a log file gets periodic whole lines instead of redraws.
        # getattr, because a captured or replaced stderr need not be a real file
        # object at all, and a missing isatty means "assume not a terminal".
        tty=bool(getattr(stream, "isatty", lambda: False)()),
        artifact_path=resume or kwargs.get("scan_out"),
    )


def _read_donors(path: str) -> list[str]:
    """Read one donor URL per line, ignoring blank lines and ``#`` comments."""
    with open(path, "r", encoding="utf-8") as fh:  # noqa: UP015 - explicit read-only contract
        return [line.strip() for line in fh if line.strip() and not line.lstrip().startswith("#")]


def _add_flags(sub: argparse.ArgumentParser, cmd: str) -> None:
    sub.add_argument("--input", help="JSON object mapped onto the handler arguments")
    if cmd == "scan-reanalyze":
        _source_flag(sub, "--source", dest="input_path", help="retained SQLite scan to read")
        sub.add_argument("--out", help="new derived SQLite file; never overwrites an existing file")
        sub.add_argument("--producer-build", metavar="SHA", help="current analyzer source build")
        sub.add_argument(
            "--step",
            choices=("all", "links.position"),
            help="full reanalysis or resumable position-only backfill",
        )
        sub.add_argument(
            "--max-documents",
            type=int,
            help="position documents per derived batch; only with --step links.position",
        )
    if cmd in {"scan-graph", "scan-sitemap-generate"}:
        _source_flag(
            sub, "--scan", "--source", dest="input_path", help="retained SQLite scan to read"
        )
    if cmd == "scan-sitemap-generate":
        sub.add_argument("--out-dir", help="fresh output directory for sitemap XML files")
        sub.add_argument(
            "--sitemap-base-url",
            help="absolute public directory URL where the files will be deployed",
        )
        sub.add_argument(
            "--include-images",
            action="store_true",
            help="include images from complete retained HTML evidence",
        )
        sub.add_argument(
            "--exclude", action="append", help="URL exclusion glob; repeat for several patterns"
        )
    if cmd == "scan-graph":
        for name in ("positions", "section", "around", "compare-to"):
            sub.add_argument("--" + name)
        sub.add_argument("--aggregate", choices=("none", "sections"))
        sub.add_argument(
            "--representation",
            choices=("all", "static", "rendered", "legacy_fragment", "legacy_unknown"),
        )
        for name in (
            "depth",
            "node-offset",
            "edge-offset",
            "node-limit",
            "edge-limit",
            "max-nodes",
            "max-edges",
            "max-bytes",
        ):
            sub.add_argument("--" + name, type=int)
        sub.add_argument("--timeout-seconds", type=float)
    if cmd in URL_COMMANDS:
        _source_flag(sub, "--url", help="target URL")
    if cmd == "links-check":
        sub.add_argument("--internal-only", action="store_true", help="check internal links only")
    if cmd in ("log-analyze", "logs-import"):
        _source_flag(sub, "--path", help="access-log file or import directory; gzip allowed")
        sub.add_argument("--max-lines", type=int, help="explicit input line budget")
    if cmd == "logs-import":
        sub.add_argument("--project", help="existing local project workspace")
        sub.add_argument("--max-files", type=int, help="directory file budget")
    if cmd in ("logs-summary", "logs-join", "logs-bots"):
        sub.add_argument("--source", help="retained logs.v1 SQLite artifact")
        sub.add_argument("--family", help="exact bot family filter")
    if cmd == "logs-join":
        sub.add_argument("--scan", help="validated retained native scan")
        sub.add_argument("--base-url", help="explicit site origin for relative log paths")
    if cmd in ("logs-summary", "logs-join"):
        sub.add_argument("--view", help="registered evidence view")
        sub.add_argument("--date-from", help="UTC date inclusive, YYYY-MM-DD")
        sub.add_argument("--date-to", help="UTC date inclusive, YYYY-MM-DD")
    if cmd == "logs-summary":
        sub.add_argument("--status", type=int, help="exact HTTP status")
        sub.add_argument("--section", help="exact first path segment")
    if cmd in ("log-analyze", "logs-summary", "logs-join", "logs-bots"):
        sub.add_argument("--limit", type=int, help="page size, 1..500")
        sub.add_argument("--offset", type=int, help="stable page offset")
        sub.add_argument("--bot", help="exact bot name filter")
    if cmd in ("log-analyze", "logs-summary", "logs-bots"):
        sub.add_argument("--ip", help="IP address or subnet filter")
    if cmd in ("log-analyze", "logs-bots"):
        sub.add_argument(
            "--verify-bots", action="store_true", help="explicit bounded DNS verification"
        )
        sub.add_argument("--max-dns", type=int, help="maximum DNS checks, 0..100; plus one canary")
    if cmd == "logs-bots":
        sub.add_argument(
            "--verdict", choices=("verified", "fake", "unchecked"), help="cached verdict filter"
        )
    if cmd == "webmaster-url-queries":
        _source_flag(sub, "--host-id", dest="host_id", help="verified Yandex Webmaster host ID")
        _source_flag(sub, "--url", help="one exact page URL")
        _source_flag(sub, "--url-contains", dest="url_contains", help="page URL substring")
        sub.add_argument("--max-urls", dest="max_urls", type=int, help="URL cap, 1..500")
        sub.add_argument(
            "--max-queries-per-url",
            dest="max_queries_per_url",
            type=int,
            help="query cap per URL, 1..500",
        )
        sub.add_argument("--start-date", dest="start_date", help="local ISO date filter")
        sub.add_argument("--end-date", dest="end_date", help="local ISO date filter")
    if cmd in {
        "scan-evidence",
        "scan-content-search",
        "scan-extract",
        "scan-fragment-links",
        "scan-structured-blocks",
        "scan-requeue",
        "scan-import-urls",
    }:
        _source_flag(sub, "--scan", dest="input_path", help="existing SQLite artifact")
    if cmd == "scan-fragment-links":
        sub.add_argument(
            "--state",
            choices=("resolved", "missing", "skipped"),
            help="occurrence state filter",
        )
        sub.add_argument(
            "--representation",
            choices=("static", "rendered", "legacy_fragment"),
            help="source representation filter",
        )
        sub.add_argument("--offset", type=int)
        sub.add_argument("--limit", type=int)
    if cmd == "scan-evidence":
        sub.add_argument(
            "--section",
            choices=(
                "capabilities",
                "corpus",
                "structured",
                "routes",
                "resources",
                "timeline",
                "relations",
                "browser",
                "extraction",
            ),
        )
        sub.add_argument("--limit", type=int)
        sub.add_argument("--offset", type=int)
    if cmd == "scan-content-search":
        sub.add_argument("--query", help="nonempty marker; a literal string unless --kind regex")
        sub.add_argument("--out-dir", help="new local content-search package directory")
        sub.add_argument(
            "--scope",
            choices=("raw_html", "head_markup", "body_text", "selector_markup"),
            help="retained representation to search",
        )
        sub.add_argument("--mode", choices=("contains", "not_contains"))
        sub.add_argument(
            "--kind",
            choices=("literal", "regex"),
            help="query is a literal string (default) or a Python regular expression",
        )
        sub.add_argument("--representation", choices=("static", "rendered"))
        sub.add_argument("--selector", help="CSS selector required only by selector_markup")
        sub.add_argument("--case-sensitive", action="store_true")
        sub.add_argument("--include-snippets", action="store_true")
    if cmd == "scan-content-search-page":
        _source_flag(sub, "--package", help="completed local content-search package directory")
        sub.add_argument("--offset", type=int, help="zero-based derived record offset")
        sub.add_argument("--limit", type=int, help="records per page, 1..100")
        sub.add_argument(
            "--status",
            choices=("matched", "not_matched", "unavailable"),
            help="return only records with this status; offset then counts matching records",
        )
        sub.add_argument(
            "--status-code", type=int, help="return only records with this HTTP status, 100..599"
        )
    if cmd == "scan-structured-blocks":
        _source_flag(sub, "--url", help="exact retained logical URL")
        sub.add_argument("--representation", choices=("static", "rendered", "legacy_fragment"))
    if cmd == "scan-extract":
        _source_flag(sub, "--url", help="optional exact logical URL")
        sub.add_argument("--representation", choices=("static", "rendered", "legacy_fragment"))
        sub.add_argument("--limit", type=int)
    if cmd in {"scan-requeue", "scan-import-urls"}:
        _source_flag(sub, "--backup", dest="backup_path", help="new mandatory verified backup path")
    if cmd == "scan-requeue":
        _source_flag(sub, "--where", help="restricted predicate over saved URL/page fields")
        _source_flag(sub, "--from-scan", help="optional alternate SQLite selection source")
    if cmd == "scan-import-urls":
        _source_flag(sub, "--urls-file", help="explicit external TXT/CSV/XLSX/XML URL list")
    if cmd == "inspect-url":
        _source_flag(sub, "--url", help="one page URL")
    if cmd == "audit-workflow":
        _source_flag(sub, "--directory", help="project workspace")
        sub.add_argument("--action", choices=("status", "start", "prepare", "report"))
        _source_flag(sub, "--target", help="target for a new project")
        sub.add_argument("--out")
        sub.add_argument(
            "--format", dest="fmt", choices=("md", "csv", "xlsx", "docx", "json", "pdf")
        )
    if cmd == "tool-catalog":
        sub.add_argument("--query")
        sub.add_argument("--limit", type=int)
        sub.add_argument("--include-arguments", action="store_true")
    if cmd == "crawl-site":
        sub.add_argument(
            "--approve-large-crawl",
            action="store_true",
            help="approve exceeding project budgets",
        )
        sub.add_argument(
            "--user-agent",
            help="user agent preset or custom string",
        )
        sub.add_argument(
            "--observer-run-id",
            help=argparse.SUPPRESS,
        )
        _source_flag(
            sub,
            "--project",
            help="project directory: default target and scan location",
        )
        _source_flag(
            sub,
            "--urls",
            help="comma-separated URL list: list mode, no discovery",
        )
        _source_flag(
            sub,
            "--urls-file",
            help="list mode: TXT/CSV/XLSX/XML URL file",
        )
        sub.add_argument(
            "--max-urls",
            type=int,
            help="URL cap: 1..1,000,000; 0=full SQLite (default 200)",
        )
        sub.add_argument(
            "--out-dir",
            help="legacy directory output; disables default scan",
        )
        sub.add_argument(
            "--scan-out",
            metavar="FILE",
            help="SQLite output: site crawl or URL list",
        )
        _source_flag(
            sub,
            "--resume",
            metavar="FILE",
            help="resume stored URL/settings; no crawl overrides",
        )
        sub.add_argument(
            "--producer-build", metavar="SHA", help="original source build for SQLite capture"
        )
        sub.add_argument("--config", help="path to a crawler config file (JSON)")
        sub.add_argument(
            "--profile",
            metavar="NAME",
            help="crawl with a saved profile instead of --config; --set still applies",
        )
        sub.add_argument(
            "--save-profile",
            metavar="NAME",
            help="store the --config file as profile NAME after validating it; does not crawl",
        )
        sub.add_argument(
            "--robots",
            choices=["respect", "report_only", "ignore"],
            help="obey, report-only, or skip robots.txt",
        )
        sub.add_argument(
            "--sitemap",
            help="seed/reconcile XML URLs; discovery: --config",
        )
        sub.add_argument(
            "--sitemap-only",
            action="store_true",
            help="SQLite: explicit --sitemap members only",
        )
        sub.add_argument(
            "--config-help",
            action="store_true",
            help="list every crawler configuration setting",
        )
        sub.add_argument(
            "-q",
            "--quiet",
            action="store_true",
            help="no crawl lines on stderr; stdout unchanged",
        )
        sub.add_argument(
            "--max-urls-per-second",
            type=float,
            metavar="N",
            help="requests/second per host (sets speed.min_delay_seconds)",
        )
        sub.add_argument(
            "--set",
            action="append",
            dest="set_settings",
            metavar="PATH=VALUE",
            help="set PATH=VALUE after --config; repeatable; paths: --config-help",
        )
        # Kept working for scripts written before --config existed, but no longer advertised in
        # --help: depth and delay are exactly the kind of setting #13's config file exists for, and
        # every flag shown here is one more line standing between a new setting and --config.
        # --set is the answer to that tension rather than an exception to it: one flag reaches
        # every setting, and a setting added tomorrow is reachable with no CLI change at all.
        sub.add_argument("--max-depth", type=int, help=argparse.SUPPRESS)
        sub.add_argument("--min-delay", type=float, help=argparse.SUPPRESS)
    if cmd == "compare-crawls":
        sub.add_argument(
            "--compression",
            choices=("none", "gzip"),
            help="explicit NDJSON compression; gzip requires --out-dir",
        )
        _source_flag(
            sub,
            "--out-dir",
            help="new local compare.v2 package; required for large retained audits",
        )
        sub.add_argument(
            "--correspondence",
            help="url-correspondence.v1 JSON file declaring origin and URL pairs",
        )
        sub.add_argument(
            "--force",
            action="store_true",
            help="compare known-different effective crawl settings",
        )
    if cmd == "crawl-enrich":
        _source_flag(sub, "--audit", help="crawl audit JSON or SQLite scan")
        _source_flag(sub, "--external-csv", help="URL-keyed traffic or search CSV")
        sub.add_argument(
            "--url-column", default="url", help="external CSV URL column (default: url)"
        )
        sub.add_argument(
            "--ignore-query", action="store_true", help="join URLs without query strings"
        )
        sub.add_argument("--ignore-scheme", action="store_true", help="join HTTP and HTTPS URLs")
        sub.add_argument(
            "--casefold-path", action="store_true", help="case-fold URL paths for the join"
        )
        sub.add_argument(
            "--out-urls",
            help="write reliable external-only URLs as a list-mode input file",
        )
        sub.add_argument(
            "--visits-column", help="CSV column with visit counts (analytics findings)"
        )
        sub.add_argument(
            "--bounce-column",
            help="CSV column with bounce rate, as a percentage or fraction (analytics findings)",
        )
    if cmd == "crawl-import":
        _source_flag(
            sub,
            "--manifest",
            help="versioned third-party crawl manifest JSON (CSV files stay beside it)",
        )
    if cmd == "site-audit":
        _source_flag(sub, "--url", help="site home page")
        _source_flag(
            sub,
            "--urls",
            help="explicit comma-separated page list (otherwise discovered from the sitemap)",
        )
        sub.add_argument("--limit", type=int, help="maximum pages to inspect (default 25)")
        sub.add_argument("--concurrency", type=int, help="maximum concurrent requests (default 5)")
        sub.add_argument(
            "--render",
            action="store_true",
            help="inspect the rendered DOM for regional selectors (requires Playwright)",
        )
        sub.add_argument("--skip", help="comma-separated tools to skip")
        sub.add_argument(
            "--crux-evidence",
            help="local JSON output from crux-report or restricted provider artifact; no Google call",
        )
        sub.add_argument(
            "--report",
            choices=("xlsx", "docx", "csv", "md", "json", "pdf"),
            help="build a report in this format after the audit",
        )
        sub.add_argument("--out", help="report output path")
    if cmd == "keywords-expand":
        _source_flag(sub, "--phrase", help="seed phrase")
        sub.add_argument("--limit", type=int, help="maximum refinements to return (default 300)")
        sub.add_argument("--regions", help="comma-separated Yandex region IDs (225 is Russia)")
    if cmd == "keywords-seasonality":
        _source_flag(sub, "--phrase", help="query phrase")
        sub.add_argument(
            "--from-date",
            dest="from_date",
            help="period start in RFC3339 form, e.g. 2026-01-01T00:00:00Z",
        )
        sub.add_argument("--to-date", dest="to_date", help="period end in RFC3339 form")
        sub.add_argument("--period", help="PERIOD_MONTHLY | PERIOD_WEEKLY | PERIOD_DAILY")
        sub.add_argument("--regions", help="comma-separated Yandex region IDs")
    if cmd == "keywords-exact":
        _source_flag(sub, "--keywords", help="comma-separated phrases")
        sub.add_argument("--region", type=int, help="Yandex region ID (default 225)")
        sub.add_argument(
            "--no-wait",
            action="store_true",
            help="create the paid task and exit; retrieve its result later by task_id",
        )
    if cmd == "serp-fetch":
        _source_flag(sub, "--query", help="single search query")
        _source_flag(sub, "--queries", help="comma-separated batch of search queries")
        sub.add_argument("--region", help="Yandex region ID (default 225)")
        sub.add_argument("--top", type=int, help="number of result positions (default 10)")
    if cmd in {"google-keywords", "google-serp"}:
        sub.add_argument(
            "--confirm-paid", action="store_true", help="explicitly confirm production charges"
        )
        sub.add_argument(
            "--env", choices=("sandbox", "prod"), help="provider environment (default sandbox)"
        )
        sub.add_argument(
            "--spend-ceiling-usd",
            type=float,
            help="pre-call ceiling; supply current prices through --input",
        )
    if cmd == "dataforseo-estimate":
        _source_flag(sub, "--operation", help="DataForSEO operation to quote offline")
        sub.add_argument(
            "--refresh-prices",
            action="store_true",
            help="explicit free account appendix/user_data price lookup",
        )
        sub.add_argument("--query", help="SERP query, including priced search operators")
        sub.add_argument("--query-count", type=int, help="number of proposed requests")
        sub.add_argument("--row-limit", type=int, help="maximum billed result rows per request")
        sub.add_argument("--cache-hits", type=int, help="caller-declared requests already cached")
        sub.add_argument("--env", choices=("sandbox", "prod"))
    if cmd == "backlinks-profile":
        _source_flag(
            sub, "--domain", help="target hostname; imports/evidence/sources through --input JSON"
        )
        sub.add_argument("--project", help="retain private profile history in this local project")
        sub.add_argument("--db", help="explicit private backlink history SQLite file")
        sub.add_argument("--state", choices=("live", "lost", "unknown"))
        sub.add_argument("--follow", choices=("normal", "nofollow", "sponsored", "ugc", "unknown"))
        sub.add_argument("--target-prefix", help="filter targets under this URL prefix")
        sub.add_argument("--min-spam-score", type=float)
        sub.add_argument("--max-spam-score", type=float)
        sub.add_argument(
            "--history",
            dest="history_only",
            action="store_true",
            help="read retained history metadata without collecting or writing",
        )
        sub.add_argument("--one-per-domain", action="store_true")
        sub.add_argument("--limit", type=int)
        sub.add_argument("--offset", type=int)
    if cmd == "backlinks-disavow":
        _source_flag(sub, "--domains", help="comma-separated reviewed donor domains")
        _source_flag(sub, "--urls", help="comma-separated reviewed donor URLs")
        sub.add_argument("--output", help="new local text file; never overwrites or submits")
    if cmd == "google-keywords":
        _source_flag(sub, "--keywords", help="comma-separated phrases for search-volume lookup")
        _source_flag(sub, "--seed", help="seed phrase for keyword expansion")
        sub.add_argument(
            "--location-code",
            dest="location_code",
            type=int,
            help="DataForSEO location code (2840 is the United States)",
        )
        sub.add_argument("--language", help="language code (default en)")
        sub.add_argument(
            "--country",
            help="country used by the coverage guard; Russia and Belarus are unsupported",
        )
        sub.add_argument("--limit", type=int, help="maximum keyword ideas (default 100)")
        sub.add_argument(
            "--difficulty",
            action="store_true",
            help="return keyword difficulty instead of search volume",
        )
    if cmd == "google-serp":
        _source_flag(sub, "--query", help="search query")
        sub.add_argument(
            "--location-code",
            dest="location_code",
            type=int,
            help="DataForSEO location code (2840 is the United States)",
        )
        sub.add_argument("--language", help="language code (default en)")
        sub.add_argument("--depth", type=int, help="number of result positions (default 10)")
        sub.add_argument("--country", help="country used by the provider coverage guard")
    if cmd == "wayback-history":
        _source_flag(sub, "--url", help="URL to look up in the Wayback Machine")
        sub.add_argument("--limit", type=int, help="maximum snapshots to return")
        sub.add_argument("--from-date", dest="from_date", help="earliest timestamp, e.g. 2024")
        sub.add_argument("--to-date", dest="to_date", help="latest timestamp, e.g. 20260101")
    if cmd == "cloudflare-traffic":
        _source_flag(sub, "--zone", help="Cloudflare zone name, e.g. example.com")
        sub.add_argument(
            "--since", help="first UTC day, YYYY-MM-DD (default: 6 days before --until)"
        )
        sub.add_argument("--until", help="last UTC day, YYYY-MM-DD (default: today)")
    if cmd == "crtsh-subdomains":
        _source_flag(sub, "--domain", help="domain to search Certificate Transparency logs for")
    if cmd == "gsc-query":
        _source_flag(sub, "--site-url", dest="site_url", help="verified Search Console property")
        sub.add_argument(
            "--mode",
            choices=("search_analytics", "inspect_url"),
            help="search_analytics (default) or inspect_url",
        )
        sub.add_argument("--start-date", dest="start_date", help="period start (default 28daysAgo)")
        sub.add_argument("--end-date", dest="end_date", help="period end (default today)")
        sub.add_argument("--dimensions", help="comma-separated dimensions, e.g. query,page")
        sub.add_argument("--row-limit", dest="row_limit", type=int, help="rows to return")
        sub.add_argument("--inspection-url", dest="inspection_url", help="URL for mode=inspect_url")
    if cmd == "gsc-archive":
        _source_flag(sub, "--database", help="explicit local SQLite archive file")
        sub.add_argument(
            "--action",
            choices=("status", "prepare", "run", "backup"),
            help="offline status (default), prepare, bounded run, or backup",
        )
        sub.add_argument(
            "--site-url", dest="site_url", help="verified property; required for prepare"
        )
        sub.add_argument(
            "--start-date", dest="start_date", help="inclusive YYYY-MM-DD; required for prepare"
        )
        sub.add_argument(
            "--end-date", dest="end_date", help="inclusive YYYY-MM-DD; required for prepare"
        )
        sub.add_argument(
            "--max-requests",
            dest="max_requests",
            type=int,
            help="API requests per run, 1..1000 (default 1)",
        )
        sub.add_argument(
            "--pause", type=float, help="seconds between API requests, 0..60 (default 1)"
        )
        sub.add_argument(
            "--backup-path", dest="backup_path", help="new snapshot file; required for backup"
        )
    if cmd == "crux-report":
        _source_flag(sub, "--url", help="page URL to report on")
        _source_flag(sub, "--origin", help="origin to report on, instead of a single URL")
        _source_flag(sub, "--urls", help="explicit comma-separated URLs for bounded field samples")
        sub.add_argument(
            "--form-factor", dest="form_factor", choices=("PHONE", "DESKTOP", "TABLET")
        )
        sub.add_argument("--metrics", help="comma-separated CrUX metric names")
        sub.add_argument("--max-samples", type=int, help="maximum sampled URLs, 1..25 (default 25)")
        sub.add_argument("--cache-dir", help="explicit private local CrUX cache directory")
        sub.add_argument(
            "--cache-max-age-hours", type=float, help="cache freshness limit (default 24 hours)"
        )
    if cmd == "indexnow-submit":
        _source_flag(sub, "--urls", help="comma-separated URLs to submit")
        sub.add_argument("--host", help="host the submitted URLs and key belong to")
        sub.add_argument("--key-location", dest="key_location", help="key file URL, if non-default")
    if cmd == "metrika-setup":
        _source_flag(sub, "--counter", help="Yandex Metrika counter ID")
    if cmd == "metrika-report":
        _source_flag(sub, "--counter", help="Yandex Metrika counter ID")
        sub.add_argument(
            "--metrics", help="comma-separated API metrics, e.g. ym:s:visits,ym:s:users"
        )
        sub.add_argument("--dimensions", help="comma-separated API dimensions, e.g. ym:s:startURL")
        sub.add_argument("--date1", help="period start (a date or 30daysAgo)")
        sub.add_argument("--date2", help="period end (a date or today)")
        sub.add_argument("--filters", help="filter expression in Metrika API notation")
        sub.add_argument("--sort", help="sort expression, e.g. -ym:s:visits")
        sub.add_argument("--limit", type=int, help="rows per API page (default 100)")
        sub.add_argument(
            "--paginate", action="store_true", help="collect all API pages, capped at 100,000 rows"
        )
    if cmd == "metrika-traffic-pdf":
        _source_flag(sub, "--counter", help="Yandex Metrika counter ID(s), comma-separated")
        _source_flag(
            sub, "--document", help="existing traffic document JSON to render without network"
        )
        sub.add_argument("--date1", help="period start, YYYY-MM-DD")
        sub.add_argument("--date2", help="period end, YYYY-MM-DD")
        sub.add_argument("--out-dir", dest="out_dir", help="directory that receives the files")
        sub.add_argument("--attribution", choices=("last_significant", "last_click"))
        sub.add_argument("--traffic", choices=("organic", "all"), help="default organic")
        sub.add_argument("--filters", help="extra Metrika filter expression, ANDed")
        sub.add_argument("--lang", choices=("en", "ru"), help="labels and API names language")
        sub.add_argument("--top", type=int, help="rows in landing-page and phrase tables")
        sub.add_argument("--site-label", dest="site_label", help="site name shown in the report")
        sub.add_argument("--brand", help="brand JSON file or inline JSON object")
        sub.add_argument(
            "--gsc-site-url", dest="gsc_site_url", help="also add Search Console queries"
        )
        sub.add_argument("--no-render", action="store_true", help="collect the document only")
        sub.add_argument("--no-pdf", action="store_true", help="write HTML without the PDF")
        sub.add_argument("--overwrite", action="store_true", help="replace existing files")
        sub.add_argument("--timeout", type=float, help="browser timeout in seconds")
    if cmd == "regions-tree":
        sub.add_argument("--save-to", dest="save_to", help="save a flat {name: id} mapping as JSON")
    if cmd == "spend-report":
        sub.add_argument("--since", help="include charges on or after YYYY-MM-DD")
        sub.add_argument("--csv", help="write one CSV row per journal entry to this path")
    if cmd in {"sources-sync", "sources-status", "sources-export"}:
        _source_flag(sub, "--db", help="sources SQLite database path")
        _source_flag(sub, "--project", help="project directory; uses its sources.sqlite")
    if cmd in {"sources-sync", "sources-export"}:
        sub.add_argument("--source", help="gsc, ga4, metrika, webmaster, or webmaster_history")
        sub.add_argument(
            "--resource", help="GSC property, GA4 property, Metrika counter, or Webmaster host ID"
        )
        sub.add_argument("--start-date", dest="start_date", help="YYYY-MM-DD")
        sub.add_argument("--end-date", dest="end_date", help="YYYY-MM-DD")
    if cmd == "sources-sync":
        sub.add_argument("--force", action="store_true", help="re-fetch days already stored")
    if cmd == "sources-export":
        sub.add_argument("--match", help="substring of any dimension (query, page, ...)")
        sub.add_argument("--limit", type=int, help="maximum JSON rows (default 1000)")
        sub.add_argument(
            "--out", help="new CSV with at most --limit matching rows; never overwrites"
        )
    if cmd == "report-build":
        sub.add_argument(
            "--pdf-policy",
            choices=("overview-v1",),
            help="explicit audit.v2 PDF overview with complete JSON/CSV companions",
        )
        _source_flag(
            sub, "--audit", help="path to an audit JSON document or scan.v1 SQLite artifact"
        )
        sub.add_argument(
            "--format",
            choices=("xlsx", "docx", "csv", "md", "json", "pdf"),
            help="report format (default xlsx)",
        )
        sub.add_argument("--out", help="output file path")
        _source_flag(sub, "--project", help="validated local project workspace")
        sub.add_argument("--view", help="saved finding view to apply from the project")
        sub.add_argument("--offset", type=int, help="finding-view page offset")
        sub.add_argument("--lang", choices=("en", "ru"), default="en", help="PDF language")
    if cmd == "log-scan":
        # Not `required=True`: that would reject a JSON-only `--input '{"run": ...}'` call before
        # _build_kwargs ever runs, since argparse enforces required flags ahead of dispatch. The
        # handler already raises a clear error when `run` is missing from both sources (#218).
        _source_flag(
            sub,
            "--run",
            help="native scan path, or run directory with scan.sqlite, audit.json or pages.jsonl",
        )
        sub.add_argument(
            "--images-dir",
            help="an images-download output directory, so a recorded size can be compared "
            "against the file on disk",
        )
    if cmd in ("crawl-diagnose", "crawl-diagnose-export"):
        _source_flag(sub, "--scan", help="saved native scan.v1/v2 SQLite artifact")
        _source_flag(sub, "--run", help="legacy crawl output directory with audit.json")
        sub.add_argument(
            "--max-decisions", type=int, default=20, help="decision sample size (1..20)"
        )
        if cmd == "crawl-diagnose-export":
            sub.add_argument(
                "--export", help="write a new redacted JSON diagnostic file (never overwrite)"
            )
    if cmd == "compare-crawls":
        # See the log-scan comment above: `required=True` here would reject a JSON-only
        # `--input '{"before": ..., "after": ...}'` call the same way (#218). compare_crawls
        # already raises a clear error when either side is missing.
        _source_flag(
            sub, "--before", help="path to the earlier audit.json or scan.v1 SQLite artifact"
        )
        _source_flag(sub, "--after", help="path to the later audit.json or scan.v1 SQLite artifact")
    if cmd == "verify-fixes":
        _source_flag(sub, "--baseline", help="saved baseline audit.json or SQLite scan")
        sub.add_argument("--finding-ids", help="comma-separated baseline finding IDs")
        _source_flag(sub, "--view", help="saved verification_view.v1 JSON selection")
        _source_flag(sub, "--urls", help="comma-separated affected baseline URLs")
        _source_flag(sub, "--urls-file", help="TXT/CSV/XLSX/XML affected URL list")
        _source_flag(sub, "--after", help="existing after audit/scan for offline verification")
        sub.add_argument(
            "--config", help="original crawler config when the baseline redacted secrets"
        )
        sub.add_argument("--out-dir", help="new directory for recrawl and immutable verification")
    if cmd == "segment-diff":
        # See the log-scan comment above: `required=True` here would reject a JSON-only
        # `--input '{"audit": ..., "source": ..., "target": ...}'` call the same way (#218).
        # segment_diff already raises a clear error when any of the three is missing.
        _source_flag(
            sub, "--audit", help="path to the audit.json or scan.v1 SQLite artifact to diff"
        )
        sub.add_argument("--source", help="segment name that should have a counterpart")
        sub.add_argument("--target", help="segment name to look for the counterpart in")

    if cmd == "scan-list":
        _source_flag(
            sub, "--directory", required=False, help="directory containing scan SQLite files"
        )
        sub.add_argument("--offset", type=int)
        sub.add_argument("--limit", type=int)
        _source_flag(sub, "--project", help="project directory whose scans/ directory is listed")
    if cmd in _SCAN_PATH_COMMANDS:
        _source_flag(sub, "--scan", dest="input_path", required=False, help="scan SQLite file")
    if cmd == "scan-inspect":
        sub.add_argument("--table")
        sub.add_argument("--offset", type=int)
        sub.add_argument("--limit", type=int)
        sub.add_argument("--max-bytes", dest="max_bytes", type=int)
        sub.add_argument("--columns", type=lambda v: v.split(","), help="comma-separated columns")
        sub.add_argument("--total", action="store_true", help="include the table row count")
    if cmd == "scan-url-query":
        sub.add_argument("--filters", type=_json_list, help="JSON list of {column, op, value}")
        sub.add_argument("--sort")
        sub.add_argument("--direction", choices=("asc", "desc"))
        sub.add_argument("--columns", type=lambda v: v.split(","), help="comma-separated columns")
        sub.add_argument("--offset", type=int)
        sub.add_argument("--limit", type=int)
        sub.add_argument("--count-timeout-seconds", dest="count_timeout_seconds", type=float)
        sub.add_argument("--max-bytes", dest="max_bytes", type=int)
        sub.add_argument(
            "--facets",
            type=lambda v: v if v == "all" else v.split(","),
            help="comma-separated facet groups, or 'all': counts per group over the same filters",
        )
        from seohead.storage.url_query import PRESETS

        sub.add_argument(
            "--preset",
            help="ready-made filter set, AND-combined with --filters: "
            + ", ".join(sorted(PRESETS)),
        )
        sub.add_argument(
            "--export",
            metavar="PATH",
            help="write every matching row to this new file instead of printing a page",
        )
        sub.add_argument("--export-format", dest="export_format", choices=("csv", "xlsx"))
        sub.add_argument("--export-max-rows", dest="export_max_rows", type=int)
        sub.add_argument(
            "--issue-check",
            dest="issue_check",
            action="append",
            help="audit check id to filter by (repeatable, 1..50)",
        )
        sub.add_argument(
            "--issue-severity",
            dest="issue_severity",
            choices=("critical", "warning", "notice"),
        )
    if cmd == "scan-url-history":
        _source_flag(sub, "--project", help="project directory whose scans/ directory is read")
        _source_flag(sub, "--url", help="exact retained URL text")
        sub.add_argument("--limit", type=int, help="newest scans to read (1..500, default 50)")
    if cmd == "scan-url-detail":
        _source_flag(sub, "--url", help="exact retained logical URL")
        sub.add_argument("--response-offset", type=int)
        sub.add_argument("--response-limit", type=int)
        sub.add_argument("--form-offset", type=int)
        sub.add_argument("--form-limit", type=int)
        sub.add_argument("--max-bytes", dest="max_bytes", type=int)
    if cmd == "scan-link-inspect":
        sub.add_argument("--view", choices=("path", "inlinks", "context", "links"))
        sub.add_argument("--url", help="URL whose links to page (view=links)")
        sub.add_argument("--direction", choices=("out", "in"))
        sub.add_argument("--link-type", dest="link_type", choices=("all", "internal", "external"))
        sub.add_argument("--follow", choices=("all", "follow", "nofollow", "sponsored", "ugc"))
        sub.add_argument(
            "--status-class",
            dest="status_class",
            choices=("all", "2xx", "3xx", "4xx", "5xx", "broken", "error", "unscanned"),
        )
        sub.add_argument("--contains", help="substring of the other URL or the anchor")
        sub.add_argument("--sort", choices=("order", "url", "status"))
        sub.add_argument("--seed")
        sub.add_argument("--target")
        sub.add_argument(
            "--representation",
            choices=("all", "static", "rendered", "legacy_fragment", "legacy_unknown"),
        )
        sub.add_argument("--cursor")
        sub.add_argument("--link-id", dest="link_id", type=int)
        sub.add_argument("--document-id", dest="document_id", type=int)
        sub.add_argument("--offset", type=int)
        sub.add_argument("--limit", type=int)
        sub.add_argument("--max-bytes", dest="max_bytes", type=int)
        sub.add_argument("--max-body-bytes", dest="max_body_bytes", type=int)
        sub.add_argument("--max-nodes", dest="max_nodes", type=int)
        sub.add_argument("--max-edges", dest="max_edges", type=int)
        sub.add_argument("--max-depth", dest="max_depth", type=int)
        sub.add_argument("--timeout-seconds", dest="timeout_seconds", type=float)
    if cmd == "scan-snapshot":
        _source_flag(sub, "--out", help="new snapshot SQLite file")
    if cmd == "scan-export":
        _source_flag(
            sub,
            "--scan",
            dest="input_path",
            metavar="FILE",
            help="scan.v1 SQLite artifact or SF Analyzer audit.json to export",
        )
        sub.add_argument(
            "--out",
            metavar="PATH",
            help="output file; CSV mode treats it as the base for per-entity files",
        )
        sub.add_argument("--format", choices=("csv", "xlsx", "json", "xml"), default="json")
        sub.add_argument(
            "--records",
            metavar="TYPES",
            help="comma-separated record types: pages, links, findings (default: all available)",
        )
        sub.add_argument(
            "--fields",
            action="append",
            metavar="TYPE=F1,F2",
            help="record field selection, repeatable, e.g. --fields pages=url,title",
        )
    if cmd == "scan-pin":
        sub.add_argument("--unpin", action="store_true")
    if cmd == "scan-prune":
        _source_flag(
            sub, "--directory", required=False, help="directory containing scan SQLite files"
        )
        sub.add_argument("--older-than-days", dest="older_than_days", type=int)
        sub.add_argument("--keep-newest", dest="keep_newest", type=int)
        _source_flag(sub, "--plan", help="reviewed prune-plan JSON file")
        sub.add_argument("--apply", action="store_true")
        _source_flag(sub, "--project", help="project directory whose scans/ directory is pruned")
    if cmd == "project-new":
        _source_flag(sub, "--directory", help="new project directory")
        _source_flag(sub, "--target", help="primary site URL")
        sub.add_argument("--label", help="human project label")
    if cmd in {"project-open", "project-status", "project-progress"}:
        _source_flag(sub, "--directory", help="project directory")
    if cmd.startswith("project-sources-"):
        _source_flag(sub, "--directory", help="validated local project workspace")
        if cmd != "project-sources-list":
            sub.add_argument(
                "--service", help="gsc, ga4, gtm, metrika, webmaster, bing or topvisor"
            )
            sub.add_argument("--resource", help="provider resource ID, e.g. a GSC property")
        if cmd == "project-sources-link":
            sub.add_argument("--label", help="human label for the resource")
    if cmd == "project-observe":
        _source_flag(sub, "--directory", help="validated local project workspace")
        sub.add_argument("--consumer", help="stable local agent/session consumer id")
        sub.add_argument("--scan-limit", type=int, help="retained scans per site (default: 20)")
        sub.add_argument("--run-offset", type=int, help="terminal run offset per site (default: 0)")
        sub.add_argument(
            "--run-limit", type=int, help="terminal runs per site, 1..100 (default: 20)"
        )
    if cmd.startswith("project-inbox-"):
        _source_flag(sub, "--directory", help="validated local project workspace")
    if cmd.startswith("project-event-"):
        _source_flag(sub, "--directory", help="validated local project workspace")
        sub.add_argument("--source", choices=("agent", "user", "scans", "app"), help="event source")
    if cmd == "project-event-append":
        sub.add_argument("--actor", choices=("user", "agent", "schedule"), help="event actor")
        sub.add_argument("--text", help="event text, 1..2000 characters")
    if cmd == "project-event-page":
        sub.add_argument("--offset", type=int, default=0)
        sub.add_argument("--limit", type=int, default=50, help="events per page, 1..200")
        sub.add_argument("--query", default="", help="case-insensitive text substring")
        sub.add_argument("--since", dest="since_revision", type=int, help="event revision cursor")
        sub.add_argument("--follow", action="store_true", help="wait for a newer event")
        sub.add_argument(
            "--wait-seconds", type=int, default=30, help="maximum follow wait, 0..30 (default: 30)"
        )
    if cmd in {
        "project-update",
        "project-delete",
        "project-settings",
        "project-settings-proposals",
        "project-settings-review",
        "project-action-log",
    }:
        _source_flag(sub, "--directory", help="validated local project workspace")
    if cmd == "project-update":
        sub.add_argument("--label", required=True, help="new project label, up to 128 characters")
        sub.add_argument("--expected-revision", type=int, required=True)
    if cmd == "project-settings-proposals":
        sub.add_argument("--offset", type=int, default=0)
        sub.add_argument("--limit", type=int, default=50)
    if cmd == "project-settings-review":
        sub.add_argument("--proposal-id", required=True)
        sub.add_argument("--decision", required=True, choices=("approve", "deny"))
        sub.add_argument("--expected-revision", type=int, required=True)
    if cmd == "project-action-log":
        sub.add_argument("--offset", type=int, default=0)
        sub.add_argument("--limit", type=int, default=50)
        sub.add_argument("--client", choices=("cli", "mcp", "system"))
        sub.add_argument(
            "--outcome",
            choices=("pending", "applied", "approved", "denied", "stale", "refused", "deleted"),
        )
        sub.add_argument("--query", default="")
    if cmd == "project-inbox-submit":
        _source_flag(sub, "--text", help="specialist note, proposed goal or question text")
        sub.add_argument(
            "--kind",
            choices=("note", "proposed_goal", "question"),
            help="entry kind (default: note)",
        )
        sub.add_argument(
            "--references", help="comma-separated goal/task/scan/finding/section references"
        )
        sub.add_argument(
            "--author-role",
            choices=("specialist", "agent"),
            help="entry author role (default: specialist)",
        )
        sub.add_argument("--expected-revision", type=int)
    if cmd in {"project-inbox-list", "project-inbox-unread"}:
        sub.add_argument("--consumer", required=True, help="stable local agent/session consumer id")
        sub.add_argument("--limit", type=int, default=20 if cmd.endswith("list") else 10)
    if cmd == "project-inbox-list":
        sub.add_argument("--offset", type=int, default=0)
        sub.add_argument("--unacknowledged-only", action="store_true")
    if cmd in {"project-inbox-read", "project-inbox-acknowledge"}:
        sub.add_argument("--consumer", required=True, help="stable local agent/session consumer id")
        sub.add_argument("--entry-ids", required=True, help="comma-separated inbox entry ids")
        sub.add_argument("--expected-revision", type=int)
    if cmd == "project-inbox-goal":
        sub.add_argument("--entry-id", required=True)
        sub.add_argument("--state", required=True, choices=("accepted", "completed"))
        sub.add_argument("--expected-revision", type=int)
    if cmd == "project-inbox-triage":
        sub.add_argument("--entry-id", help="specialist note id")
        sub.add_argument("--actor", help="stable controller identity")
        sub.add_argument("--expected-revision", type=int)
    if cmd in {
        "workflow-start",
        "workflow-checkpoint",
        "workflow-status",
        "workflow-execute",
        "workflow-resume",
    }:
        _source_flag(sub, "--directory", help="project directory")
    if cmd == "workflow-start":
        sub.add_argument(
            "--goal-id",
            dest="goal_id",
            help="accepted owner goal id (inbox:...); the prompt and goal task are derived",
        )
    if cmd in {
        "monitor-configure",
        "monitor-run",
        "monitor-status",
        "monitor-schedule",
        "monitor-collect",
        "monitor-local-deliver",
    }:
        _source_flag(sub, "--directory", help="project directory")
    if cmd in {"monitor-collect", "monitor-local-deliver"}:
        sub.add_argument("--expected-revision", type=int, help="current monitoring revision")
    if cmd in {
        "schedule-show",
        "schedule-set",
        "schedule-run-due",
        "schedule-install",
        "schedule-uninstall",
    }:
        _source_flag(sub, "--directory", help="project directory")
    if cmd == "schedule-set":
        sub.add_argument("--expected-revision", type=int, help="current schedule revision")
    if cmd in {"schedule-install", "schedule-uninstall"}:
        sub.add_argument("--kind", choices=["launchd", "cron", "windows"], required=True)
        sub.add_argument("--dest", help="file to write (launchd default: ~/Library/LaunchAgents)")
    if cmd == "schedule-install":
        sub.add_argument("--python", help="interpreter for the trigger (default: this one)")
    if cmd == "monitor-collect":
        sub.add_argument(
            "--apply",
            action="store_true",
            default=None,
            help="explicitly fetch the existing claimed plan; preview by default",
        )
    if cmd == "monitor-local-deliver":
        sub.add_argument("--scan-id", help="retained monitor run identifier for a local receipt")
    if cmd == "project-progress":
        sub.add_argument("--limit", type=int, default=20, help="items per page (1..100)")
        sub.add_argument("--offset", type=int, default=0, help="zero-based item offset")
    if cmd == "remediation-create":
        sub.add_argument(
            "--path", required=True, help="new ledger.v1 SQLite path; never overwrites"
        )
        _source_flag(
            sub,
            "--project-dir",
            dest="project_dir",
            required=True,
            help="validated project directory",
        )
        sub.add_argument(
            "--producer-build",
            dest="producer_build",
            required=True,
            help="full lowercase 40-character Git SHA of the ledger-writing build",
        )
    if cmd == "remediation-ingest":
        _source_flag(sub, "--scan", help="validated saved scan.v1 SQLite artifact")
    if cmd in {
        "remediation-ingest",
        "remediation-summary",
        "remediation-cases",
        "remediation-transition",
        "remediation-record-verification",
        "remediation-recheck",
        "remediation-report",
    }:
        _source_flag(sub, "--ledger", help="validated local ledger.v1 SQLite artifact")
    if cmd == "remediation-cases":
        sub.add_argument("--check", help="exact registry check identifier")
        sub.add_argument("--url", help="exact affected URL")
        sub.add_argument("--finding-key", dest="finding_key", help="exact finding SHA-256 key")
        sub.add_argument("--limit", type=int, default=100, help="findings per page (1..1000)")
        sub.add_argument("--offset", type=int, default=0, help="zero-based finding offset")
        sub.add_argument(
            "--source-scan-id", type=int, help="source revision for a group member page"
        )
        sub.add_argument(
            "--group-ref", help="exact source group reference; requires --source-scan-id"
        )
        sub.add_argument("--max-bytes", type=int, help="group member page byte bound (up to 8 MiB)")
    if cmd == "remediation-transition":
        sub.add_argument("--occurrence-key", dest="occurrence_key", help="case SHA-256 key")
        sub.add_argument("--state", help="next lifecycle state")
        sub.add_argument("--actor", help="decision actor")
        sub.add_argument("--reason", help="bounded decision rationale")
        sub.add_argument("--expected-revision", dest="expected_revision", type=int)
        sub.add_argument("--observation-id", dest="observation_id", type=int)
        sub.add_argument("--decided-at", dest="decided_at", help="UTC ISO-8601 decision time")
    if cmd == "remediation-record-verification":
        _source_flag(
            sub,
            "--verification-path",
            dest="verification_path",
            help="retained verification.v1 JSON",
        )
        sub.add_argument("--actor", help="recheck actor")
        sub.add_argument("--expected-revision", dest="expected_revision", type=int)
        sub.add_argument("--occurrence-keys", help="comma-separated exact ledger case keys")
        sub.add_argument(
            "--task-id",
            dest="task_id",
            default="unassigned",
            help="local remediation task identifier",
        )
    if cmd == "remediation-recheck":
        _source_flag(
            sub, "--baseline", help="retained baseline audit.json used to create the ledger cases"
        )
        sub.add_argument("--occurrence-keys", help="comma-separated exact pending ledger case keys")
        _source_flag(sub, "--after", help="retained later audit for offline bounded verification")
        sub.add_argument(
            "--config", help="original crawler config when the baseline redacted secrets"
        )
        sub.add_argument("--actor", help="bounded recheck actor")
        sub.add_argument("--expected-revision", dest="expected_revision", type=int)
        sub.add_argument(
            "--task-id",
            dest="task_id",
            default="unassigned",
            help="local remediation task identifier",
        )
        sub.add_argument(
            "--out-dir", dest="out_dir", help="new immutable verification evidence directory"
        )
    if cmd == "remediation-report":
        sub.add_argument("--out-dir", dest="out_dir", help="new directory for JSON and Markdown")
        sub.add_argument(
            "--limit", type=int, default=100, help="findings per report page (1..1000)"
        )
        sub.add_argument("--offset", type=int, default=0, help="zero-based finding offset")
    if cmd == "project-open":
        sub.add_argument("--expected-site", help="expected target host")
    if cmd in {
        "project-checklist-init",
        "project-checklist-update",
        "project-checklist-record",
        "project-priorities",
    }:
        _source_flag(sub, "--directory", help="project directory")
        sub.add_argument(
            "--expected-revision",
            dest="expected_revision",
            type=int,
            help="current checklist revision required before a write",
        )
    if cmd in {
        "project-view-list",
        "project-view-show",
        "project-view-save",
        "project-view-delete",
        "project-view-rename",
    }:
        _source_flag(sub, "--directory", help="validated local project workspace")
    if cmd in {"project-view-show", "project-view-delete", "project-view-rename"}:
        sub.add_argument("--name", help="saved finding view name")
    if cmd == "project-view-rename":
        sub.add_argument("--new-name", dest="new_name", help="new finding view name")
    if cmd in {"project-view-delete", "project-view-rename"}:
        sub.add_argument(
            "--expected-revision",
            dest="expected_revision",
            type=int,
            help="current project view config revision",
        )
    if cmd == "project-view-save":
        sub.add_argument(
            "--expected-revision",
            dest="expected_revision",
            type=int,
            help="current project view config revision; use 0 for the first save",
        )
    if cmd == "findings-view":
        _source_flag(sub, "--directory", help="validated local project workspace")
        sub.add_argument("--name", help="saved finding view name")
        _source_flag(sub, "--audit", help="audit JSON document or validated scan.v1 SQLite file")
        sub.add_argument("--offset", type=int, help="finding-view page offset")
    if cmd == "project-facts":
        _source_flag(sub, "--directory", help="project directory")
        sub.add_argument(
            "--detect",
            action="store_true",
            help="fetch the project target once to detect its stack (robots.txt first)",
        )
        sub.add_argument(
            "--apply", action="store_true", help="record the previewed facts in project.json"
        )
    if cmd in {"project-priorities", "project-policy"}:
        sub.add_argument(
            "--apply",
            action="store_true",
            help="apply the previewed policy with an expected revision",
        )
    if cmd in {
        "project-policy",
        "project-prepare",
        "project-start",
        "project-competitors-add",
        "project-competitors-list",
        "project-competitors-remove",
    }:
        _source_flag(sub, "--directory", help="project directory")
    if cmd == "project-competitors-remove":
        sub.add_argument("--url", required=True, help="recorded competitor URL to remove")
    if cmd == "project-policy":
        sub.add_argument(
            "--expected-revision", type=int, help="current policy revision, zero for a new policy"
        )
    if cmd == "project-prepare":
        sub.add_argument(
            "--max-urls",
            type=int,
            help="URL cap for a new preparation crawl: 1..1,000,000; reuses a suitable saved scan first",
        )
    if cmd in {"project-prepare", "project-start"}:
        sub.add_argument("--approve-large-crawl", action="store_true")
        sub.add_argument(
            "--producer-build", help="explicit producer revision when working outside a clean build"
        )
    if cmd == "project-start":
        _source_flag(sub, "--target", help="site URL for the new project")
    if cmd in {"skill-show", "scenario-show"}:
        _source_flag(sub, "--name", help="full playbook identifier or unambiguous name")
    if cmd == "provider-replay":
        _source_flag(sub, "--scan", dest="input_path", help="saved scan")
        _source_flag(sub, "--evidence-file", help="private saved provider collection")
        _source_flag(sub, "--out-dir", help="private local join output directory")
        sub.add_argument("--url-column")
        sub.add_argument("--review-external-only", action="store_true")
    if cmd == "provider-auth":
        _source_flag(sub, "--provider", help="OAuth provider (gsc)")
        sub.add_argument(
            "--action", choices=("status", "connect", "refresh", "cancel", "disconnect", "revoke")
        )
        _source_flag(
            sub, "--grant-file", help="private bounded JSON grant obtained through provider consent"
        )
        sub.add_argument("--confirm", action="store_true")
    if cmd in {"provider-verify", "provider-collect"}:
        _source_flag(sub, "--provider", help="provider registry identifier")
    if cmd == "provider-collect":
        _source_flag(sub, "--operation", help="declared read-only provider operation")
        _source_flag(sub, "--artifact-dir", help="restricted local raw-evidence directory")
    if cmd == "evidence-normalize":
        _source_flag(
            sub,
            "--file",
            help="supplied CSV/XLSX/JSON rows or a saved provider-evidence envelope",
        )
        sub.add_argument("--mapping", help="seohead.evidence-mapping.v1 JSON or path")
        sub.add_argument("--sheet", help="XLSX sheet name")
        sub.add_argument("--site-origin", help="explicit origin binding for relative URL keys")
        _source_flag(sub, "--out-dir", help="restricted normalized artifact directory")
    if cmd == "evidence-join":
        _source_flag(sub, "--scan", help="saved scan SQLite artifact")
        _source_flag(sub, "--audit", help="crawl audit document")
        _source_flag(sub, "--pages", help="inline JSON page list or JSON file path")
        _source_flag(
            sub,
            "--evidence",
            help="evidence file or inline normalized document/rows object",
        )
        _source_flag(sub, "--compare", help="second evidence source for compatibility")
        sub.add_argument("--mapping", help="evidence mapping manifest JSON or path")
        sub.add_argument("--compare-mapping", help="mapping manifest for --compare")
        sub.add_argument("--policy", help="comparison policy JSON or path")
        sub.add_argument("--sheet", help="XLSX sheet for --evidence")
        sub.add_argument("--compare-sheet", help="XLSX sheet for --compare")
        sub.add_argument("--site-origin", help="origin binding for --evidence")
        sub.add_argument("--compare-site-origin", help="origin binding for --compare")
        sub.add_argument("--ignore-query", action="store_true")
        sub.add_argument("--ignore-scheme", action="store_true")
        sub.add_argument("--casefold-path", action="store_true")
        _source_flag(sub, "--out-dir", help="private local join output directory")
    if cmd in {
        "project-activity",
        "project-checklist-page",
        "project-task-detail",
        "project-scans",
    }:
        _source_flag(sub, "--directory", help="validated local project workspace")
    if cmd == "project-task-detail":
        sub.add_argument("--item-id", help="exact checklist item identifier")
    if cmd in {"project-checklist-page", "project-scans", "scan-navigation"}:
        sub.add_argument("--limit", type=int, help="bounded page size")
        sub.add_argument("--offset", type=int, help="zero-based item offset")
    if cmd == "project-checklist-page":
        sub.add_argument("--query", help="case-insensitive checklist search")
        sub.add_argument(
            "--kind", choices=("method", "schema", "check", "skill", "scenario", "custom")
        )
        sub.add_argument("--state", help="exact displayed checklist state")
        sub.add_argument(
            "--sort", choices=("id", "priority", "state", "updated"), help="sort field (default id)"
        )
        sub.add_argument("--desc", dest="descending", action="store_true", help="reverse order")
        sub.add_argument(
            "--states", type=_split_list, help="comma-separated displayed states (up to 8)"
        )
    if cmd == "scan-navigation":
        _source_flag(sub, "--scan", dest="input_path", help="retained local scan artifact")
        sub.add_argument("--document-id", type=int, help="exact retained document identifier")
    if cmd == "bi-filter":
        _source_flag(sub, "--package", help="verified local BI package directory")
        sub.add_argument("--dataset", help="declared BI dataset")
        _source_flag(sub, "--out-dir", help="new local filtered package directory")
        sub.add_argument(
            "--where", help='exact equality JSON object, e.g. {"severity":["warning"]}'
        )
        sub.add_argument("--columns", help="comma-separated declared fields")
        sub.add_argument("--max-rows-per-file", type=int)
        sub.add_argument("--max-bytes-per-file", type=int)
        sub.add_argument("--max-output-bytes", type=int)
        _source_flag(sub, "--xlsx-out", help="optional new split XLSX output")
        sub.add_argument("--xlsx-max-rows-per-sheet", type=int)
    if cmd == "bi-export":
        _source_flag(sub, "--scan", help="validated scan.v1 SQLite artifact")
        _source_flag(sub, "--audit", help="supported saved audit JSON document")
        _source_flag(
            sub,
            "--provider-join",
            action="append",
            dest="provider_join",
            help="saved issue #781 evidence-join artifact or normalized evidence JSON; repeatable",
        )
        _source_flag(sub, "--out-dir", help="new local BI package directory (never overwritten)")
        sub.add_argument("--max-rows-per-file", type=int, help="CSV partition row bound")
        sub.add_argument("--max-bytes-per-file", type=int, help="CSV partition byte bound")
        sub.add_argument("--max-output-bytes", type=int, help="hard total package byte bound")
        sub.add_argument(
            "--max-scan-bytes",
            type=int,
            help="streamed native scan hash/read budget (default 8 GiB; maximum 32 GiB)",
        )
        sub.add_argument(
            "--search-metric",
            choices=("clicks", "impressions"),
            help="explicit Search Console axis for complete compatible GA sessions quadrants",
        )
        _source_flag(sub, "--xlsx-out", help="optional new split XLSX consumer output")
        sub.add_argument(
            "--xlsx-dataset",
            help="declared BI dataset to write to split XLSX worksheets",
        )
        sub.add_argument(
            "--xlsx-max-rows-per-sheet",
            type=int,
            help="data rows per XLSX worksheet, below Excel's row limit",
        )
    if cmd == "bi-sheets-plan":
        _source_flag(sub, "--package", help="complete local BI package directory")
        sub.add_argument(
            "--max-cells", type=int, help="declared Sheets cell capacity (maximum 10000000)"
        )
    if cmd == "bi-bigquery-plan":
        _source_flag(sub, "--package", help="complete local BI package directory")
        sub.add_argument(
            "--dataset", required=True, help="planned BigQuery dataset name; no cloud write occurs"
        )
        sub.add_argument("--operation", choices=("replace", "append"), default="replace")
    if cmd == "bi-destination-apply":
        _source_flag(sub, "--package", help="complete local BI package directory")
        sub.add_argument("--target", required=True, help="explicit authorized destination target")
        sub.add_argument("--destination", choices=("sheets", "bigquery"), required=True)
        sub.add_argument("--operation", choices=("replace", "append"), default="replace")
        sub.add_argument(
            "--apply", action="store_true", help="request an injected host-authorized write"
        )
        sub.add_argument(
            "--reconcile",
            action="store_true",
            help="explicitly read/reconcile a prior uncertain destination request before retrying it",
        )
    if cmd in {"publication-cohorts", "gsc-progress"}:
        _source_flag(sub, "--file", help="versioned offline cohort input JSON")
        _source_flag(
            sub, "--out-dir", help="new local cohort package directory (never overwritten)"
        )
    if cmd == "project-checklist-record":
        _source_flag(sub, "--item-id", help="checklist item identifier to record")
    if cmd == "scan-body-diff":
        _source_flag(sub, "--left", help="earlier scan SQLite file")
        _source_flag(sub, "--right", help="later scan SQLite file")
        _source_flag(sub, "--url", help="exact logical URL")
        sub.add_argument("--variant-key")
        sub.add_argument("--representation", choices=("static", "rendered", "legacy_fragment"))
        sub.add_argument("--text", action="store_true")
        sub.add_argument("--max-bytes", dest="max_bytes", type=int)
        sub.add_argument("--max-lines", dest="max_lines", type=int)
    if cmd == "regions-check":
        _source_flag(sub, "--url", help="any site page, usually the home page")
        sub.add_argument(
            "--extra",
            help="comma-separated additional regional URLs; "
            "satellite domains cannot be discovered from the page",
        )
        sub.add_argument("--limit", type=int, help="maximum regional pages to fetch (default 12)")
        sub.add_argument(
            "--render",
            action="store_true",
            help="find JavaScript-rendered city selectors (requires Playwright)",
        )
    if cmd == "render-check":
        sub.add_argument(
            "--browser-transport",
            choices=("local", "remote"),
            help="local browser launch (default) or explicit remote Playwright connection",
        )
        sub.add_argument(
            "--remote-endpoint-env",
            help="environment variable containing a remote Playwright ws/wss endpoint",
        )
        sub.add_argument(
            "--remote-playwright-version",
            help="operator-declared remote Playwright version; major/minor must match the client",
        )
        sub.add_argument(
            "--viewport",
            choices=("desktop", "mobile"),
            help="desktop, or the built-in smartphone diagnostic identity and viewport",
        )
        sub.add_argument(
            "--user-agent",
            help="explicit identity for both raw and browser requests; must be one header line",
        )
        sub.add_argument(
            "--wait",
            choices=("load", "domcontentloaded", "networkidle"),
            help="DOM capture milestone (default load; networkidle may never "
            "occur on sites with persistent connections)",
        )
    if cmd == "domain-profile":
        _source_flag(sub, "--domain", help="domain name or URL")
        sub.add_argument("--no-tls", action="store_true", help="skip TLS certificate inspection")
    if cmd == "security-check":
        sub.add_argument(
            "--probe-paths",
            action="store_true",
            help="also probe for exposed service paths such as .git and .env",
        )
    if cmd == "schema-build":
        sub.add_argument(
            "--type",
            help="explicit Schema.org type (Product, Service, Article, etc.) "
            "when the classifier has low confidence",
        )
    if cmd == "backlinks-check":
        _source_flag(sub, "--target", help="target domain or exact URL")
        _source_flag(sub, "--donors", help="comma-separated donor-page URLs")
        _source_flag(sub, "--donors-file", help="file containing one donor URL per line")
        sub.add_argument("--concurrency", type=int, help="maximum concurrent requests (default 3)")
    if cmd in ("parse", "images-download"):
        _source_flag(sub, "--urls", help="comma-separated URL list")
    if cmd == "redirects-generate":
        sub.add_argument("--format", help="apache-rewrite-rule|apache-redirect|nginx|csv|custom")
    if cmd == "sitemap-crawl":
        sub.add_argument("--concurrency", type=int, help="parallel fetches (default 3)")
        _source_flag(sub, "--project", help="explicit local project for sitemap observation")
    if cmd == "images-download":
        sub.add_argument("--output-dir", help="download target directory")
    if cmd == "images-optimize":
        _source_flag(sub, "--files", help="comma-separated image paths/dirs")
        sub.add_argument("--output-dir", help="safe output directory (recommended)")
        sub.add_argument(
            "--in-place",
            action="store_true",
            help="explicitly allow source mutation; backups are enabled by default",
        )
        sub.add_argument(
            "--overwrite", action="store_true", help="replace an existing destination file"
        )
        sub.add_argument("--format", choices=("keep", "jpeg", "png", "webp", "avif"))
        sub.add_argument("--quality", type=int, help="lossy quality, clamped to 10..100")
        sub.add_argument("--max-width", type=int, help="maximum output width; never upscales")
        sub.add_argument("--max-height", type=int, help="maximum output height; never upscales")
        sub.add_argument("--max-pixels", type=int, help="input pixel safety limit")
    if cmd == "duplicate-check":
        _source_flag(sub, "--scan", help="validated scan.v1 SQLite artifact to read offline")
        sub.add_argument(
            "--threshold", type=float, help="similarity threshold from 0 to 1 (default 0.92)"
        )
        sub.add_argument(
            "--fingerprints",
            action="store_true",
            help="include every page fingerprint in output; this can make stdout "
            "very large for substantial datasets",
        )
        sub.add_argument(
            "--all-pages",
            action="store_true",
            help="also compare non-indexable items; by default only items with "
            "indexable=true (or no indexable flag) are compared, since a "
            "canonicalised twin is not a defect",
        )
    if cmd == "boilerplate-report":
        _source_flag(sub, "--scan", help="validated scan.v1 SQLite artifact to read offline")
    if cmd == "semantic-inputs":
        _source_flag(sub, "--scan", help="validated scan.v1 SQLite artifact to read offline")
    if cmd == "semantic-similarity":
        _source_flag(sub, "--scan", help="validated scan.v1 SQLite artifact to read offline")
        sub.add_argument("--cache-path", help="local SQLite embedding cache (created if absent)")
        sub.add_argument(
            "--threshold", type=float, help="cosine candidate threshold (default 0.82)"
        )
        sub.add_argument(
            "--max-candidate-comparisons", type=int, help="finite pairwise comparison budget"
        )
    if cmd == "meta-description-drafts":
        _source_flag(sub, "--scan", help="validated scan.v1 SQLite artifact to read offline")
        sub.add_argument("--checkpoint-path", help="local SQLite draft checkpoint")
        sub.add_argument("--batch-size", type=int, help="bounded supplied-draft batch size")
        sub.add_argument("--json-path", help="local JSON review artifact (requires --csv-path)")
        sub.add_argument(
            "--csv-path", help="local formula-safe CSV review artifact (requires --json-path)"
        )
    if cmd == "ai-column":
        _source_flag(sub, "--scan", help="validated scan.v1 SQLite artifact to read offline")
        sub.add_argument(
            "--prompt", help="instruction applied to each selected page (max 2000 chars)"
        )
        sub.add_argument("--column", help="custom column name (default ai_column)")
        sub.add_argument("--max-pages", type=int, help="selection limit (default 100, max 500)")
        sub.add_argument("--csv-path", help="local formula-safe CSV of the column values")
    if cmd == "llms-txt-check":
        sub.add_argument("--brand", help="brand name that llms.txt should mention")


# crawl-site keeps only its most-used settings as direct flags; everything the crawler build-out
# (#13 onward) has added or will add lives in --config instead, so --help stays short as the
# surface grows. This note is the pointer from one to the other.
CRAWL_SITE_HELP_NOTE = "More crawler settings: seohead crawl-site --config-help."
SCAN_FRAGMENT_LINKS_HELP_NOTE = (
    "Measures only retained complete HTML/DOM from the saved scan; missing, "
    "truncated or otherwise incomplete evidence stays skipped/unavailable and "
    "is never reported as a broken fragment. No network access."
)


WORKFLOW_START_HELP_NOTE = """\
--input takes {"scenario_id": "scenario:full-audit", "steps": [...], "context": {...}}.
Only the accepted goal is required in context; the rest is derived:
  goal_id          required: the accepted proposed goal (inbox:...). --goal-id sets it.
  prompt_reference optional: defaults to skill:workflow/full-audit-v1 (the registered contract).
  task_ids         optional: defaults to the checklist task the accepted goal created
                   (custom:goal-<entry id>); it must be current and incomplete.
  competitors      optional: unique HTTP(S) URLs, at most 20.
  phase            optional: a short label stored with the run.
Owner-side steps that come first (nothing starts without them):
  seohead project inbox-goal --directory DIR --entry-id ENTRY --state accepted
  seohead project inbox-list --directory DIR --consumer ID   (find the entry id)
Refused runs name the missing link and the command that creates it.
"""

CHECKLIST_RECORD_HELP_NOTE = """\
--input takes {"record": {...}} with the checklist item's directory, --item-id and
--expected-revision. Accepted record shapes (all are evidence only; nothing is executed):
  automatic check:   {"status": "succeeded", "reason": "...", "artifact": "scans/<saved>.sqlite"}
                     the saved scan (scan.v1 or streamed audit.v2) must prove the check ran
  attempt:           {"status": "failed" | "unavailable" | "running", "reason": "..."}
  manual signoff:    {"status": "succeeded", "reason": "...", "reviewer": "...", "signoff": true}
  reviewed artifact: {"status": "succeeded", "reason": "...", "reviewer": "...",
                      "review": "approved", "artifact": "reports/<file>"}
                     (manual and deliverable items; deliverables take no signoff)
  reviewed exclusion: {"status": "not_applicable", "reason": "...", "reviewer": "...",
                      "artifact": "reports/<file>"} or "evidence": "..." instead of artifact
artifact paths are project-relative and must lie under scans/ or reports/.
"""


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="seohead", description="Headless Python SEO toolkit.")
    p.add_argument("--version", action="version", version=f"seohead {__version__}")
    subs = p.add_subparsers(dest="command", metavar="<command>")
    version = subs.add_parser("version", help="core version, formats and commands")
    version.add_argument("--json", action="store_true", help="print machine-readable core info")
    for cmd in COMMANDS:
        epilog = (
            CRAWL_SITE_HELP_NOTE
            if cmd == "crawl-site"
            else SCAN_FRAGMENT_LINKS_HELP_NOTE
            if cmd == "scan-fragment-links"
            else None
        )
        if cmd == "workflow-start":
            epilog = WORKFLOW_START_HELP_NOTE
        sp = subs.add_parser(
            cmd,
            help=f"run the {cmd} tool",
            epilog=epilog,
            formatter_class=argparse.RawDescriptionHelpFormatter
            if cmd == "workflow-start"
            else argparse.HelpFormatter,
        )
        _add_flags(sp, cmd)
    scan = subs.add_parser("scan", help="saved SQLite scan history")
    scan_subs = scan.add_subparsers(dest="scan_command", metavar="<action>", required=True)
    for action in (
        "list",
        "inspect",
        "url-query",
        "url-detail",
        "link-inspect",
        "graph",
        "sitemap-generate",
        "status",
        "rendered-routes",
        "content-search",
        "content-search-page",
        "navigation",
        "snapshot",
        "export",
        "pin",
        "prune",
        "body-diff",
        "evidence",
        "extract",
        "fragment-links",
        "requeue",
        "import-urls",
    ):
        cmd = "scan-" + action
        sp = scan_subs.add_parser(action, help=f"run {cmd}")
        _add_flags(sp, cmd)
    logs = subs.add_parser("logs", help="local retained access-log evidence")
    logs_subs = logs.add_subparsers(dest="logs_command", metavar="<action>", required=True)
    for action in ("import", "summary", "join", "bots"):
        sp = logs_subs.add_parser(action, help=f"run logs-{action}")
        _add_flags(sp, "logs-" + action)
    project = subs.add_parser("project", help="local project workspace")
    project_subs = project.add_subparsers(dest="project_command", metavar="<action>", required=True)
    for action in (
        "new",
        "open",
        "update",
        "delete",
        "settings",
        "settings-proposals",
        "settings-review",
        "action-log",
        "status",
        "progress",
        "observe",
        "activity",
        "checklist-page",
        "task-detail",
        "scans",
        "inbox-submit",
        "inbox-list",
        "inbox-read",
        "inbox-acknowledge",
        "inbox-goal",
        "inbox-unread",
        "facts",
        "checklist-init",
        "checklist-update",
        "checklist-record",
        "priorities",
        "view-list",
        "view-show",
        "view-save",
        "view-delete",
        "view-rename",
        "policy",
        "prepare",
        "start",
        "sources-link",
        "sources-unlink",
        "sources-list",
    ):
        cmd = "project-" + action
        sp = project_subs.add_parser(
            action,
            help=f"run {cmd}",
            epilog=CHECKLIST_RECORD_HELP_NOTE if cmd == "project-checklist-record" else None,
            formatter_class=argparse.RawDescriptionHelpFormatter,
        )
        _add_flags(sp, cmd)
    project_archive = project_subs.add_parser(
        "archive", help="write a portable project archive zip (no credentials)"
    )
    project_archive.add_argument("--project", dest="project_dir", required=True)
    project_archive.add_argument("--out", required=True, help="new archive file path")
    project_archive.add_argument(
        "--dry-run", action="store_true", help="report files and size estimate; write nothing"
    )
    project_restore = project_subs.add_parser(
        "restore", help="restore a project archive into a new directory"
    )
    project_restore.add_argument("archive_file", metavar="ARCHIVE")
    project_restore.add_argument("--to", dest="to_dir", required=True, help="new project path")
    skill = subs.add_parser("skill", help="packaged method playbooks")
    skill_actions = skill.add_subparsers(dest="skill_command", required=True)
    for action in ("list", "show"):
        leaf = skill_actions.add_parser(action)
        _add_flags(leaf, "skill-" + action)
        if action == "show":
            leaf.add_argument("playbook_name", nargs="?")
    scenario = subs.add_parser("scenario", help="packaged workflow scenarios")
    scenario_actions = scenario.add_subparsers(dest="scenario_command", required=True)
    leaf = scenario_actions.add_parser("show")
    _add_flags(leaf, "scenario-show")
    leaf.add_argument("playbook_name", nargs="?")
    sf = subs.add_parser("sf", help="Screaming Frog crawl audit (run | tasks | doctor)")
    sf.add_argument(
        "sf_args", nargs=argparse.REMAINDER, help="arguments forwarded to the sf-analyzer CLI"
    )
    semantics = subs.add_parser(
        "semantics",
        help="accumulating semantic core (seohead semantics <stage> --project DIR)",
    )
    semantics.add_argument(
        "semantics_args", nargs=argparse.REMAINDER, help="stage and its arguments"
    )
    reanalyze = scan_subs.add_parser("reanalyze", help="reanalyze retained inputs without network")
    _add_flags(reanalyze, "scan-reanalyze")
    crawl_profile = subs.add_parser("crawl-profile", help="list or delete saved crawl profiles")
    crawl_profile_subs = crawl_profile.add_subparsers(
        dest="crawl_profile_command", metavar="<action>", required=True
    )
    crawl_profile_subs.add_parser("list", help="list saved crawl profile names")
    crawl_profile_subs.add_parser("delete", help="delete a saved crawl profile").add_argument(
        "name", metavar="NAME"
    )
    mcp = subs.add_parser("mcp", help="run the MCP server (stdio)")
    mcp.add_argument(
        "--profile", choices=("full", "audit", "infra", "quick-check", "router"), default=None
    )
    mcp.add_argument(
        "--no-progress", action="store_true", help="disable optional MCP progress notifications"
    )
    mcp.add_argument(
        "action",
        nargs="?",
        choices=("status", "enable", "disable", "install", "uninstall", "backups"),
    )
    mcp.add_argument(
        "--json", action="store_true", help="return only SEOHEAD state or its registration plan"
    )
    mcp.add_argument("--actor", choices=("CLI", "SEOHEAD Desktop"), default="CLI")
    mcp.add_argument("--client", choices=("claude-code", "claude-desktop", "codex", "cursor"))
    mcp.add_argument("--backup", help="adjacent backup path from the reviewed plan")
    mcp.add_argument("--dry-run", action="store_true", help="preview registration without writes")
    mcp.add_argument(
        "--yes", action="store_true", help="explicitly allow client configuration mutation"
    )
    mcp.add_argument(
        "--command", dest="install_command", help="absolute core executable for client registration"
    )
    mcp.add_argument(
        "--expected-sha256", help="refuse installation if configuration changed since preview"
    )
    tui = subs.add_parser("tui", help="interactive terminal shell (needs the optional 'tui' extra)")
    tui.add_argument("--no-color", action="store_true", help="force the plain, unstyled shell")
    watch = subs.add_parser("watch", help="observe a local project beside an AI chat")
    watch.add_argument("--project", required=True, help="validated local project workspace")
    watch.add_argument("--no-color", action="store_true", help="force the plain, unstyled shell")
    for terminal in (tui, watch):
        terminal.add_argument(
            "--lang", choices=("ru", "en"), help="label language (default: locale)"
        )
        terminal.add_argument("--scan", help="follow one retained scan/run UUID or id")
        terminal.add_argument(
            "--compact",
            action="store_true",
            help="one-line read-only status for tmux; prints once when piped",
        )
    tui.add_argument("--project", help="local project workspace")
    return p


def main(argv: list[str] | None = None) -> int:
    _configure_windows_streams()
    runlog.set_interface("cli")
    raw = sys.argv[1:] if argv is None else list(argv)
    if raw[:1] == ["semantics"]:
        # The semantic-core pipeline owns its parser, including its own --help.
        from seohead.semantics.cli import main as semantics_main

        return semantics_main(raw[1:])
    argv, warnings = _rewrite_deprecated_scan_flags(argv)
    args = build_parser().parse_args(argv)
    for warning in warnings:
        print(f"warning: {warning}", file=sys.stderr)
    cmd = args.command
    if cmd == "scan":
        cmd = "scan-" + args.scan_command
    if cmd == "project":
        cmd = "project-" + args.project_command
    if cmd == "logs":
        cmd = "logs-" + args.logs_command
    if cmd == "skill":
        cmd = "skill-" + args.skill_command
    if cmd == "scenario":
        cmd = "scenario-" + args.scenario_command
    if not cmd:
        build_parser().print_help()
        return 0
    if cmd == "version":
        from seohead.core.core_info import core_info

        info = core_info(COMMANDS)
        if args.json:
            print(json.dumps(info, ensure_ascii=False, indent=2))
        else:
            rev = f" ({info['revision'][:7]})" if info["revision"] else ""
            print(f"seohead {info['package_version']}{rev}")
        return 0
    if cmd == "sf":
        # The crawl-audit subsystem owns its parser; preserve and forward its argument tail.
        from seohead.sf.cli import main as sf_main

        return sf_main(args.sf_args)
    if cmd == "mcp":
        if args.action:
            from seohead.mcp import mcp_control

            try:
                if args.action == "status":
                    result = mcp_control.status()
                elif args.action == "backups":
                    result = mcp_control.backups(args.client)
                elif args.action in {"enable", "disable"}:
                    mcp_control.set_state(
                        args.action == "enable", profile=args.profile, actor=args.actor
                    )
                    result = mcp_control.status()
                elif not args.client:
                    raise ValueError("--client is required")
                elif args.action == "install":
                    result = mcp_control.install(
                        args.client,
                        dry_run=args.dry_run,
                        yes=args.yes,
                        command=args.install_command,
                        expected_sha256=args.expected_sha256,
                        backup_path=args.backup,
                    )
                else:
                    result = mcp_control.uninstall(args.client, yes=args.yes)
            except (OSError, ValueError) as exc:
                result = {
                    "ok": False,
                    "error": str(exc)
                    if isinstance(exc, ValueError)
                    else "configuration I/O failed",
                }
            if args.json or args.action in {"install", "uninstall"} or not result["ok"]:
                print(json.dumps(result, ensure_ascii=False, indent=2))
            elif args.action == "backups":
                for item in result["backups"]:
                    print(
                        f"{item['client']} | {item['created_at']} | {item['reason']} | "
                        f"{item['sha256'][:12]}{'' if item['verified'] else ' (hash mismatch)'} | {item['path']}"
                    )
                if not result["backups"]:
                    print("No SEOHEAD configuration backups")
            else:
                print(
                    f"MCP: {'enabled' if result['enabled'] else 'disabled'} | profile {result['profile']} | {result['tools']} tools"
                )
                print(f"Changed by: {result['by_label']}")
                print(
                    "Clients: "
                    + ", ".join(c for c, info in result["clients"].items() if info["registered"])
                )
            return 0 if result["ok"] else 1
        from seohead.mcp.mcp_server import main as mcp_main

        # mcp_main() itself catches a missing optional SDK and returns 1 after a stderr
        # diagnostic (#366), so the direct `python -m seohead.mcp.mcp_server` entry
        # point advertised in that module's docstring gives the same outcome as this one.
        if args.profile is None and not args.no_progress:
            return mcp_main()
        return mcp_main(profile=args.profile, progress_notifications=not args.no_progress)
    if cmd == "project-archive":
        from seohead.projects.archive import archive_project

        try:
            result = archive_project(args.project_dir, args.out, dry_run=args.dry_run)
        except ValueError as exc:
            result = {"ok": False, "error": str(exc)}
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0 if result["ok"] else 1
    if cmd == "project-restore":
        from seohead.projects.archive import restore_project

        try:
            result = restore_project(args.archive_file, args.to_dir)
        except (ValueError, OSError, zipfile.BadZipFile, KeyError) as exc:
            result = {"ok": False, "error": str(exc)}
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0 if result["ok"] else 1
    if cmd in INTERACTIVE_COMMANDS:
        try:
            from seohead.tui.app import run as tui_run
        except ImportError:
            print(
                "seohead tui needs the optional 'tui' extra: pip install 'seohead-seotools[tui]'",
                file=sys.stderr,
            )
            return 1
        return tui_run(
            no_color=args.no_color,
            project=getattr(args, "project", None),
            commands=COMMANDS,
            lang=args.lang,
            scan=args.scan,
            compact=args.compact,
        )
    from seohead.cli.terminal_progress import show_banner

    show_banner(cmd, quiet=getattr(args, "quiet", False))
    if cmd == "crawl-profile":
        from seohead.crawl import profiles

        try:
            if args.crawl_profile_command == "list":
                print(json.dumps({"profiles": profiles.saved_names()}, ensure_ascii=False))
            else:
                profiles.delete(args.name)
                print(json.dumps({"profile": args.name, "deleted": True}, ensure_ascii=False))
        except profiles.ProfileError as exc:
            print(f"error: {exc}", file=sys.stderr)
            return 1
        return 0
    if cmd == "crawl-site" and getattr(args, "config_help", False):
        _print_config_help()
        return 0
    if cmd == "crawl-site" and getattr(args, "save_profile", None):
        from seohead.crawl import profiles

        if not getattr(args, "config", None):
            print("error: --save-profile needs --config FILE", file=sys.stderr)
            return 1
        try:
            stored = profiles.save(args.save_profile, args.config)
        except profiles.ProfileError as exc:
            print(f"error: {exc}", file=sys.stderr)
            return 1
        print(json.dumps({"profile": args.save_profile, "path": stored}, ensure_ascii=False))
        return 0
    try:
        handler_name, kwargs = _build_kwargs(cmd, args)
        report_fmt = kwargs.pop("_report", None)
        report_out = kwargs.pop("_out", None)
        quiet = getattr(args, "quiet", False)
        progress = None
        if cmd == "crawl-site" and not quiet:
            if not kwargs.get("resume"):
                # A resume applies the artifact's own recorded settings, not this
                # command line's: printing a rate derived from flags that are refused
                # here would describe a run that is not the one about to happen. The
                # progress line reads that same artifact rather than these flags.
                _print_effective_rate(kwargs)
            progress = _crawl_progress(kwargs)
            kwargs["progress"] = progress
        try:
            result = handlers.HANDLERS[handler_name](**kwargs)
        finally:
            # Closed whatever happened: an interrupted or failed crawl must not
            # leave the operator's shell prompt printed over half a progress
            # line, and the error message below is worth reading.
            if progress is not None:
                progress.close()
        if cmd == "crawl-site" and not quiet:
            # Under -q the crawl says nothing on stderr at all: the outcome this line
            # states is in the JSON on stdout as ``partial``/``finish_reason``, and a
            # caller that asked for silence is a pipeline reading that, not a terminal.
            _print_crawl_outcome(result)
        if cmd in ("crawl-diagnose", "crawl-diagnose-export") and not quiet:
            observed = result["observed"]
            pages = observed["page_records"]
            shown = f"{pages} URL records" if pages is not None else "unknown URL record count"
            elapsed = observed["elapsed_seconds"]
            duration = (
                f"{elapsed:.1f} s" if isinstance(elapsed, (int, float)) else "unknown duration"
            )
            print(
                f"crawl-diagnose: {shown}; site total unknown; "
                f"elapsed={duration}; finish={result['source']['finish_reason'] or 'unknown'}",
                file=sys.stderr,
            )
            for finding in result["diagnoses"]:
                print(
                    f"  {finding['code']}: {finding['conclusion']} Next: {finding['next_step']}",
                    file=sys.stderr,
                )
            for decision in result["decisions"]["sample"]:
                print(
                    f"  decision #{decision['decision_id']}: {decision['reason']} "
                    f"url={decision['url']!r} depth={decision['depth']}",
                    file=sys.stderr,
                )
        if report_fmt and isinstance(result, dict) and result.get("ok"):
            # Build an optional report from the in-memory audit result. This keeps the structured
            # document identical while avoiding a manual JSON handoff between two commands.
            result["report"] = handlers.report_build(result, fmt=report_fmt, out=report_out)
    except Exception as exc:  # CLI boundary: report a concise error and exit non-zero.
        print(f"error: {exc}", file=sys.stderr)
        return 1
    try:
        json.dump(result, sys.stdout, ensure_ascii=False, indent=2, default=str)
        sys.stdout.write("\n")
    except KeyboardInterrupt:
        # Collection may already be finished. Interrupting its JSON output must
        # neither roll back retained evidence nor label a truncated stream successful.
        print("interrupted while writing result; retained artifacts are unchanged", file=sys.stderr)
        return 130
    if cmd == "log-scan" and isinstance(result, dict) and result.get("anomaly_count"):
        # A contradiction is a gate, not a report: a pipeline that produced numbers which
        # disagree with each other should stop rather than publish them. 2, not 1, so a
        # caller can tell "the run contradicts itself" from "the command failed".
        return 2
    if (
        cmd == "scan-content-search"
        and isinstance(result, dict)
        and result.get("status") in {"partial", "incomplete"}
    ):
        # A staged package exists and is usable, but incomplete source coverage
        # cannot be treated as a clean whole-site presence/absence result.
        return 2
    if isinstance(result, dict) and result.get("audit_available") is False:
        # A scan whose collection finished but whose audit did not (a budget it exceeded,
        # evidence it could not reconstruct, or an unexpected exception -- #627) is neither
        # a clean run nor a failed one: the artifact is real and re-analysable, so exiting 0
        # would read as "audit ran clean" and exiting 1 would read as "nothing was produced".
        # Same 2 as the log-scan contradiction above -- a caller gating on `$?` still needs a
        # third answer besides those two.
        return 2
    if handlers.handler_failed(result):
        # The handler could not complete its check (bad input, an unreachable host, a
        # missing dependency) and said so in the JSON rather than raising. A pipeline
        # gating on `$?` needs that reflected in the exit code too, or `ok: false` is
        # indistinguishable from success to anything reading only the exit status.
        return 1
    if isinstance(result, dict) and handlers.handler_failed(result.get("report")):
        # An explicitly requested `--report` is a deliverable in its own right: the
        # outer audit can be `ok: true` while the report it was asked to also produce
        # never got written. Keep the nested error on stdout (it already is) and only
        # change the exit status, so `report-build` run directly still gates on its
        # own top-level `ok` untouched by this branch.
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
