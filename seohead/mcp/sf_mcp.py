"""MCP server (stdio) exposing the audit core through five agent-callable tools.

Requires the optional ``mcp`` dependency::

    pip install "seohead-seotools[mcp]"

Run it directly (``python -m seohead.mcp.sf_mcp``) or register it as a local
stdio connector. Large payloads are returned as file paths, never dumped inline.
Network side effects, such as sitemap
live-rechecks, remain opt-in.
"""

from __future__ import annotations

import contextlib
import os
import threading
from hashlib import sha256
from json import dumps
from pathlib import Path
from typing import Any

from seohead.sf.core.audit import INPUT_MODES, run_audit
from seohead.sf.core.loader import EXPORT_MATCHERS, discover_exports
from seohead.sf.core.runner import terminate_live_crawls
from seohead.sf.reporters import write_json, write_markdown

try:
    from mcp.server.fastmcp import FastMCP
except ImportError:  # pragma: no cover - SDK optional at import time
    FastMCP = None  # type: ignore


# Imported from the audit core so this boundary cannot drift from it: adding
# a live mode there makes it valid here without a second, easily-forgotten edit.
VALID_MODES = INPUT_MODES
VALID_PROFILES = {"lite", "full", "custom"}

# --- cancellable live crawls (#369) -----------------------------------------
#
# FastMCP calls a plain ``def`` tool synchronously on the server's single asyncio
# event loop (mcp.server.fastmcp.tools.base -- there is no thread hop for a sync
# function), so a live crawl mode's blocking subprocess wait used to freeze the
# whole stdio server: no other request could be served, and a client's
# CancelledNotification had no running task to interrupt.
#
# Making the tool an ``async def`` that awaits ``anyio.to_thread.run_sync``
# fixes the first half for free: the event loop is idle while the crawl runs,
# so ``sf_list_exports`` and friends stay responsive. The second half --
# actually stopping the child once the request is cancelled -- is the runner's
# to answer, because the runner is where the process is created. It publishes
# the crawlers it currently has running and stops them through the same
# ``_terminate_tree`` its own timeout uses, so there is one way to kill a crawl
# rather than two that must be kept in step.
#
# Replacing ``subprocess.Popen`` process-wide for the duration of a crawl would
# also have reached it, and would have reached far more: ``subprocess.run``
# resolves that module global at call time, so every unrelated child started
# anywhere in the process during the crawl would have been collected too, and
# cancelling the crawl would have sent SIGTERM to its process group.
#
# ``_RUN_LOCK`` serializes live crawls one at a time. Non-crawl sf_* tools
# (sf_list_exports, sf_audit_summary, ...) never touch it and stay answerable
# while a crawl is running.
_RUN_LOCK = threading.Lock()


def _do_run(
    mode: str,
    source: str,
    profile: str = "full",
    out: str = "report",
    config: str | None = None,
    sitemap: str | None = None,
    project: str | None = None,
) -> dict[str, Any]:
    if mode not in VALID_MODES:
        raise ValueError(f"mode must be one of {sorted(VALID_MODES)}, got {mode!r}")
    if profile not in VALID_PROFILES:
        raise ValueError(f"profile must be one of {sorted(VALID_PROFILES)}, got {profile!r}")
    if not source:
        raise ValueError("`input` (exports dir / .seospider / url / list) is required")
    observed: tuple[str, str] | None = None
    observed_phase: str | None = None
    reporter = None

    def record_phase(name: str) -> None:
        nonlocal observed_phase
        if observed is None or observed_phase == name:
            return
        from seohead.projects.run_observation import phase

        try:
            phase(observed[0], observed[1], name)
        except (OSError, ValueError):
            return
        observed_phase = name

    if project is not None:
        from seohead.projects.run_observation import NativeRunReporter, start
        from seohead.projects.workspace import open_project
        from seohead.sf.config import load_config

        opened = open_project(project)
        root = Path(opened["path"])
        try:
            artifact = Path(out).resolve().relative_to(root.resolve())
        except (OSError, ValueError):
            artifact = None
        resolved_config = load_config(config or "config.json")
        run = start(
            root,
            kind="screaming_frog",
            mode="sf_live" if mode != "parse-exports" else "sf_exports",
            max_urls=0,
            config_fingerprint=sha256(
                dumps(resolved_config, sort_keys=True, separators=(",", ":")).encode("utf-8")
            ).hexdigest(),
            artifact=root / artifact if artifact is not None else None,
            counters={"fetched": None, "queued": None, "inflight": None, "excluded": None},
        )
        observed = (str(root), run["id"])
        reporter = NativeRunReporter(root, run["id"], source="sf")
        record_phase("collection" if mode != "parse-exports" else "analysis")

    def log(message: str) -> None:
        if message.startswith("[audit] exports written") or message.startswith(
            "[audit] loaded exports"
        ):
            record_phase("analysis")
        elif message.startswith("[runner]"):
            record_phase("collection")

    def collector_started(pid: int) -> None:
        if observed is None:
            return
        from seohead.projects.run_observation import collector_started as record_collector

        try:
            record_collector(observed[0], observed[1], pid)
        except (OSError, ValueError):
            return

    try:
        observer_kwargs = (
            {"on_started": collector_started, "on_progress": reporter.observe_counts}
            if reporter is not None
            else {}
        )
        if mode == "parse-exports":
            result = run_audit(
                input_mode=mode,
                exports_dir=source,
                profile=profile,
                config_path=config or "config.json",
                sitemap_url=sitemap,
                log=log,
                **observer_kwargs,
            )
        else:
            result = run_audit(
                input_mode=mode,
                source=source,
                profile=profile,
                config_path=config or "config.json",
                sitemap_url=sitemap,
                output_dir=os.path.join(out, "exports"),
                log=log,
                **observer_kwargs,
            )
        os.makedirs(out, exist_ok=True)
        json_path = write_json(result, os.path.join(out, "audit.json"))
        md_path = write_markdown(result, os.path.join(out, "audit.md"))
    except Exception as exc:
        if observed is not None:
            from seohead.projects.run_observation import finish

            with contextlib.suppress(OSError, ValueError):
                finish(observed[0], observed[1], state="failed", reason=type(exc).__name__)
        raise
    if observed is not None:
        from seohead.projects.run_observation import finish

        with contextlib.suppress(OSError, ValueError):
            finish(observed[0], observed[1], state="finished", reason="finished")
    return {
        "summary": result.summary,
        "json_path": os.path.abspath(json_path),
        "md_path": os.path.abspath(md_path),
    }


async def _do_run_cancellable(
    mode: str,
    source: str,
    profile: str = "full",
    out: str = "report",
    config: str | None = None,
    sitemap: str | None = None,
    project: str | None = None,
) -> dict[str, Any]:
    """Run ``_do_run`` off the event loop and stop its child on cancellation.

    ``anyio.to_thread.run_sync(..., abandon_on_cancel=True)`` gives the event
    loop back to the server the moment a cancellation notification arrives
    instead of waiting for the worker thread, which is what keeps
    ``sf_list_exports`` and the other sf_* tools answerable during a live
    crawl. Abandoning the thread does not touch the OS process it started, so
    the ``except`` below does that part explicitly, the same way the runner's
    own timeout does for a deadline instead of a cancellation.

    A crawl that dies here mid-run never reaches ``write_json``/
    ``write_markdown`` (see ``_do_run`` above), so a cancelled run cannot
    leave behind an ``audit.json``/``audit.md`` that would read as completed;
    the caller distinguishes completed (a result), failed (``ToolError``) and
    cancelled (this request never gets a response, by the MCP protocol's own
    rules for a notification it sent) by that existing dict-or-error contract,
    and recovers a cancelled run's outcome the same way as a lost response to
    any other tool: rerun ``sf_list_exports``/``sf_audit_summary`` against the
    known ``out`` path.
    """
    import anyio

    def blocking() -> dict[str, Any]:
        with _RUN_LOCK:
            return _do_run(
                mode,
                source,
                profile=profile,
                out=out,
                config=config,
                sitemap=sitemap,
                project=project,
            )

    try:
        return await anyio.to_thread.run_sync(blocking, abandon_on_cancel=True)
    except anyio.get_cancelled_exc_class():
        terminate_live_crawls()
        raise


def _load(json_path: str, diagnostics: list[dict[str, str]] | None = None) -> dict[str, Any]:
    from seohead.storage.inputs import resolve_audit_input

    if not os.path.isfile(json_path):
        raise FileNotFoundError(f"audit json not found: {json_path}")
    document, notices = resolve_audit_input(json_path)
    if diagnostics is not None:
        diagnostics.extend(notices)
    elif notices:
        import warnings

        for notice in notices:
            warnings.warn(f"{notice['code']}: {notice['message']}", RuntimeWarning, stacklevel=2)
    return document


def register(mcp):  # pragma: no cover - needs the SDK
    """Register the crawl-audit tools on an existing FastMCP server.

    Registration is separate from :func:`build_server` so the unified
    ``seohead`` server can expose both live URL tools and crawl-derived audit
    tools through one local connector. The standalone server remains available
    only for focused debugging.
    """

    from mcp.types import ToolAnnotations

    create_files = ToolAnnotations(
        readOnlyHint=False, destructiveHint=False, idempotentHint=False, openWorldHint=True
    )
    create_local_files = ToolAnnotations(
        readOnlyHint=False, destructiveHint=False, idempotentHint=False, openWorldHint=False
    )
    read_files = ToolAnnotations(
        readOnlyHint=True, destructiveHint=False, idempotentHint=True, openWorldHint=False
    )

    @mcp.tool(annotations=create_files, structured_output=True)
    async def sf_audit_run(
        mode: str,
        input: str,
        profile: str = "full",
        out: str = "report",
        config: str | None = None,
        sitemap: str | None = None,
        project: str | None = None,
    ) -> dict[str, Any]:
        """Run an SF audit and write audit.json plus audit.md.

        Use ``parse-exports`` with a directory of existing CSV/XLSX exports;
        this mode does not require Screaming Frog to be installed. The
        ``load-crawl``, ``crawl``, and ``crawl-list`` modes invoke the separately
        installed, licensed SF CLI. ``input`` is respectively an exports
        directory, .seospider path, start URL, or URL-list file. Returns a compact
        summary and absolute file paths instead of embedding the large reports.

        ``config`` may explicitly select ``semantic_similarity.adapter`` and
        caller-supplied body/title/H1 vectors. No embedding model or provider
        is selected by default; absent bodies or adapters remain unavailable.

        A live crawl can run for a long time; this call runs off the server's
        event loop, so other sf_* calls stay answerable while it is in
        progress, and cancelling the request (an MCP CancelledNotification)
        stops the underlying Screaming Frog process instead of leaving it
        running. A cancelled or failed run never leaves a completed-looking
        ``audit.json``/``audit.md`` behind; on either outcome, or a lost
        response, recover by pointing sf_list_exports/sf_audit_summary at the
        same ``out`` path.
        """
        return await _do_run_cancellable(
            mode, input, profile=profile, out=out, config=config, sitemap=sitemap, project=project
        )

    @mcp.tool(annotations=read_files, structured_output=True)
    def sf_audit_summary(json_path: str) -> dict[str, Any]:
        """Read a compact health summary from audit.json or a scan.v1 SQLite artifact.

        Returns the project, health score, severity counts, the first 15 ranked
        checks, and sitemap statistics without loading the complete issue list
        into the agent context.
        """
        diagnostics: list[dict[str, str]] = []
        data = _load(json_path, diagnostics)
        s = data["summary"]
        result = {
            "project": data["run"].get("project"),
            "health_score": s.get("health_score"),
            "by_severity": s.get("by_severity"),
            "top_checks": dict(list(s.get("by_check", {}).items())[:15]),
            "sitemap": s.get("sitemap"),
        }
        if s.get("health_score") is None:
            for key in ("health_score_reason", "health_score_basis", "health_score_scope"):
                if key in s:
                    result[key] = s[key]
        if diagnostics:
            result["input_diagnostics"] = diagnostics
        return result

    @mcp.tool(annotations=read_files, structured_output=True)
    def sf_audit_issues(
        json_path: str, check: str | None = None, severity: str | None = None, limit: int = 50
    ) -> list[dict[str, Any]]:
        """Return filtered issues from audit.json or a validated scan.v1 SQLite artifact.

        The requested limit is clamped to 1..1000 to keep MCP payloads bounded.
        Omit both filters to inspect the first issues in report order.
        """
        data = _load(json_path)
        limit = max(1, min(int(limit), 1000))  # clamp to a sane range
        out: list[dict[str, Any]] = []
        for issue in data["issues"]:
            if check and issue["check"] != check:
                continue
            if severity and issue["severity"] != severity:
                continue
            out.append(issue)
            if len(out) >= limit:
                break
        return out

    @mcp.tool(annotations=read_files, structured_output=True)
    def sf_list_exports(exports_dir: str) -> dict[str, Any]:
        """Discover recognized SF exports in a directory.

        Returns the matched files by logical export name and the logical exports
        that are absent, allowing an agent to explain which checks may be skipped
        before running the audit.
        """
        found = discover_exports(exports_dir)
        missing = [k for k in EXPORT_MATCHERS if k not in found]
        return {"found": found, "missing": missing}

    @mcp.tool(annotations=create_local_files, structured_output=True)
    def sf_audit_tasks(
        json_path: str, out: str = "report", config: str | None = None
    ) -> dict[str, Any]:
        """Build tasks.json and tasks.md from audit.json or a scan.v1 SQLite artifact.

        Priority, severity inclusion, grouping, effort estimates, and URL caps
        come from the configured ``tasks_pipeline``. ``check_assignment`` uses
        only declared template/component URLs or validated audit segments and
        preserves grouping provenance and candidate/declared/confirmed state.
        Returns a compact summary and absolute paths to both backlog files.
        """
        from seohead.sf.config import load_config
        from seohead.sf.tasks import build_tasks, build_tasks_from_audit_v2, write_tasks
        from seohead.storage.audit_v2 import audit_v2_path

        diagnostics: list[dict[str, str]] = []
        resolved_config = load_config(config or "config.json")
        if audit_v2_path(json_path).exists():
            diagnostics.append(
                {"code": "audit_v2_stream", "message": "tasks read the audit.v2 findings stream"}
            )
            backlog = build_tasks_from_audit_v2(json_path, resolved_config)
        else:
            backlog = build_tasks(_load(json_path, diagnostics), resolved_config)
        os.makedirs(out, exist_ok=True)
        jp, mp = write_tasks(
            backlog, os.path.join(out, "tasks.json"), os.path.join(out, "tasks.md")
        )
        result = {
            "summary": backlog["summary"],
            "tasks_json": os.path.abspath(jp),
            "tasks_md": os.path.abspath(mp),
        }
        if diagnostics:
            result["input_diagnostics"] = diagnostics
        return result

    return mcp


def build_server():  # pragma: no cover - needs the SDK
    """Build the audit-only FastMCP server used for focused local debugging."""
    if FastMCP is None:
        raise RuntimeError('MCP SDK not installed. Run: pip install "seohead-seotools[mcp]"')
    return register(FastMCP("sf-analyzer"))


def main() -> None:  # pragma: no cover
    build_server().run()


if __name__ == "__main__":  # pragma: no cover
    main()
