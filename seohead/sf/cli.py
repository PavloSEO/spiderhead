"""Console interface for crawl audits and task generation.

This module deliberately remains a thin argument-mapping layer over
``core.run_audit`` so CLI concerns cannot leak into the audit engine.
"""

from __future__ import annotations

import argparse
import contextlib
import os
import sys
import time

from . import __version__
from .config import load_config
from .core.audit import run_audit
from .reporters import write_json, write_markdown
from .reporters.jsonfile import to_dict
from .tasks import build_tasks, write_tasks


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="sf-analyzer",
        description=(
            "Generate a machine-readable SEO audit from a Screaming Frog crawl "
            "(audit.json + audit.md)."
        ),
    )
    parser.add_argument("--version", action="version", version=f"sf-analyzer {__version__}")
    sub = parser.add_subparsers(dest="command", required=True)

    run = sub.add_parser("run", help="Run an audit")
    src = run.add_mutually_exclusive_group(required=True)
    src.add_argument(
        "--load-crawl",
        metavar="FILE.seospider",
        help="Mode A: open a saved crawl through the licensed SF CLI",
    )
    src.add_argument(
        "--crawl",
        metavar="URL",
        help="Mode A: crawl a site from scratch through the licensed SF CLI",
    )
    src.add_argument(
        "--crawl-list", metavar="FILE", help="Mode A: crawl a URL list through the licensed SF CLI"
    )
    src.add_argument(
        "--exports-dir",
        metavar="DIR",
        help="Mode B: read an existing directory of SF CSV/XLSX exports",
    )

    run.add_argument("--out", default="report", help="Report output directory (default: ./report)")
    run.add_argument(
        "--project",
        default=None,
        help="validated local project workspace for read-only live observation",
    )
    run.add_argument(
        "--gsc-inspect",
        action="store_true",
        help="explicitly inspect up to 50 URLs with the bound Search Console property",
    )
    run.add_argument(
        "--profile",
        choices=["lite", "full", "custom"],
        default=None,
        help=(
            "Audit/export profile: full (default; verifies the supported export manifest), "
            "lite, or custom"
        ),
    )
    run.add_argument("--config", default="config.json", help="Path to config.json")
    run.add_argument(
        "--auth-config",
        default=None,
        help=(
            "SF form-authentication profile (.seospiderauthconfig) for protected "
            "staging or production environments"
        ),
    )
    run.add_argument(
        "--auth",
        default=None,
        metavar="USER:PASS",
        help=(
            "HTTP Basic credentials in USER:PASS form; may be visible in shell history/process "
            "lists, so prefer an isolated transient session"
        ),
    )
    run.add_argument(
        "--sf-cli", default=None, help="Explicit path to the Screaming Frog CLI/launcher executable"
    )
    run.add_argument(
        "--max-urls-per-second",
        type=float,
        default=None,
        metavar="N",
        help=(
            "Limit the Mode A crawl request rate (0.1..1000 URLs/s; 1-2 is a polite "
            "starting point). Requires a readable base .seospiderconfig; the tool "
            "writes and verifies a derived config without changing the base."
        ),
    )
    run.add_argument(
        "--sitemap",
        default=None,
        help="Explicit sitemap.xml URL (otherwise discover it from robots.txt)",
    )
    run.add_argument(
        "--fetch-all-inlinks",
        action="store_true",
        help="Fetch the large All Inlinks export in Mode A",
    )
    run.add_argument(
        "--live-recheck",
        dest="live_recheck",
        action="store_true",
        help="Enable opt-in network rechecks for sitemap and robots data",
    )
    run.add_argument("--no-live-recheck", dest="live_recheck", action="store_false")
    run.set_defaults(live_recheck=None)
    run.add_argument(
        "--format", default="json,md", help="Comma-separated report formats to generate: json,md"
    )
    run.add_argument(
        "--tasks",
        action="store_true",
        help="Also generate the prioritized tasks.json + tasks.md backlog",
    )
    run.add_argument(
        "--fail-on",
        choices=["critical", "warning", "none"],
        default="none",
        help="Return a non-zero exit code when this severity is present (for CI)",
    )
    run.add_argument("-v", "--verbose", action="store_true")
    run.add_argument("-q", "--quiet", action="store_true")

    # `tasks` builds a backlog from an existing audit.json without rerunning the audit.
    tasks = sub.add_parser("tasks", help="Build a prioritized task backlog from audit.json")
    tasks.add_argument(
        "--json",
        dest="audit_json",
        required=True,
        help="Path to audit.json or a validated scan.v1 SQLite artifact",
    )
    tasks.add_argument(
        "--out", default="report", help="Output directory for tasks.json and tasks.md"
    )
    tasks.add_argument(
        "--config",
        default="config.json",
        help="Path to config.json containing the task-pipeline settings",
    )
    tasks.add_argument("-q", "--quiet", action="store_true")

    # `doctor` reports SF discovery and optional dependency availability.
    doc = sub.add_parser("doctor", help="Diagnose SF CLI discovery and dependency availability")
    doc.add_argument("--config", default="config.json", help="Path to config.json")
    doc.add_argument(
        "--sf-cli", default=None, help="Explicit SF CLI path to validate before searching elsewhere"
    )

    # `save-config` materialises a base config so the GUI step is a one-time
    # convenience rather than a hard dependency of every crawl.
    save = sub.add_parser(
        "save-config",
        help="Copy the most recent Screaming Frog crawl configuration to a base config file",
    )
    save.add_argument(
        "--out",
        default="audit.seospiderconfig",
        help="Where to write the base config (default: audit.seospiderconfig)",
    )
    save.add_argument(
        "--force", action="store_true", help="Overwrite the destination when it already exists"
    )

    snippets = sub.add_parser(
        "snippet-edits", help="Import snippet edits from CSV and export review tasks"
    )
    snippets.add_argument("--csv", required=True, help="CSV with url and title/description columns")
    snippets.add_argument(
        "--out", required=True, help="New JSON task file; existing files are kept"
    )
    snippets.add_argument(
        "--metrics-json",
        help="Optional JSON map of named local font profiles for measured pixel widths",
    )
    snippets.add_argument(
        "--scan", help="Optional saved scan.v1 artifact for retained-page context"
    )
    snippets.add_argument("--query", help="Explicit search query for a retained-page text excerpt")
    snippets.add_argument(
        "--representation",
        choices=("static", "rendered", "legacy_fragment"),
        default="static",
    )
    snippets.add_argument("--variant-key", help="Optional exact retained browser variant")
    snippets.add_argument("--content-area-json", help="Optional content-area configuration JSON")
    snippets.add_argument("--max-body-bytes", type=int, default=8 * 1024 * 1024)
    return parser


# Labels for the module switches, in the order an operator reads them.
_MODULE_LABELS: tuple[tuple[str, str], ...] = (
    ("structured_data_json_ld", "structured data: JSON-LD"),
    ("structured_data_microdata", "structured data: Microdata"),
    ("structured_data_rdfa", "structured data: RDFa"),
    ("structured_data_google_validation", "structured data: Google validation"),
    ("structured_data_schema_org_validation", "structured data: Schema.org validation"),
    ("spelling", "spelling check"),
    ("grammar", "grammar check"),
    ("store_html", "store HTML"),
    ("store_rendered_html", "store rendered HTML"),
    ("crawl_linked_xml_sitemaps", "crawl linked XML sitemaps"),
    ("auto_discover_sitemaps", "auto-discover XML sitemaps"),
    ("near_duplicates", "near-duplicate checking"),
    ("auto_crawl_analysis", "auto crawl analysis"),
)


# Which registry checks each module switch unlocks. The mapping is by exact
# check source, because a whole export tab can mix modules: SF:Content holds
# both spelling and grammar, and each has its own switch.
_MODULE_GATES: tuple[tuple[str, str, str], ...] = (
    ("structured_data_json_ld", "SF:Structured Data:", "structured data extraction"),
    ("spelling", "SF:Content:Spelling", "spelling check"),
    ("grammar", "SF:Content:Grammar", "grammar check"),
    ("near_duplicates", "SF:Content:Near Duplicates", "near-duplicate checking"),
)


def _gated_checks(source_prefix: str) -> list[str]:
    """Registry check ids whose SF source starts with this prefix."""
    from .core.registry import CHECKS

    return sorted(
        check_id
        for check_id, meta in CHECKS.items()
        if str(meta.get("source", "")).startswith(source_prefix)
    )


def preflight_warnings(config_path: str | None) -> list[str]:
    """Say which checks cannot run before the crawl starts, not an hour after.

    Returns human-readable lines. An unreadable or absent config yields one line
    saying so: silence would read as "everything is fine".
    """
    from .core.spiderconfig import find_base_config, read_module_flags

    resolved = find_base_config(config_path or None)
    if not resolved:
        return [
            "no Screaming Frog base config found — SF will use its own defaults "
            "and module-dependent checks will be skipped (see: seohead sf save-config)"
        ]
    try:
        with open(resolved, "rb") as fh:
            flags = read_module_flags(fh.read())
    except OSError as err:
        return [f"base config {resolved} could not be read ({err}) — module state unknown"]

    lines: list[str] = []
    for key, prefix, label in _MODULE_GATES:
        # True means on; None means unknown, and unknown is not a warning.
        if flags.get(key) is not False:
            continue
        gated = _gated_checks(prefix)
        if not gated:
            continue
        lines.append(f"{label} is off — {len(gated)} check(s) will be skipped: {', '.join(gated)}")
    return lines


def _report_base_config(cfg: dict) -> None:
    """Report which base config a crawl would use and what it switches on.

    Without this the operator only learns that module-dependent checks were
    skipped after a full crawl, by reading ``checks_skipped`` in audit.json.
    """
    import os

    from .core.spiderconfig import find_base_config, read_module_flags, read_speed

    configured = (cfg.get("sf_cli") or {}).get("seospiderconfig") or ""
    print("\nBase Screaming Frog config:")
    if configured:
        state = "exists" if os.path.isfile(configured) else "MISSING"
        print(f"  sf_cli.seospiderconfig: {configured} ({state})")
    else:
        print("  sf_cli.seospiderconfig: not set")

    resolved = find_base_config(configured or None)
    if not resolved:
        print("  resolved: NO BASE CONFIG FOUND")
        print("    request-rate limit: NOT VERIFIED; no reusable SF config was found")
        print("    → In SF, open Config → Speed, set a limit such as 1-2 URLs/s, then")
        print("      use Config → File → Config → Save As or `seohead sf save-config`.")
        print("      Set that file as sf_cli.seospiderconfig before requesting a limit.")
        return

    mtime = time.strftime("%Y-%m-%d %H:%M", time.localtime(os.path.getmtime(resolved)))
    origin = "configured" if resolved == configured else "latest crawl config"
    print(f"  resolved: {resolved}")
    print(f"    source: {origin}, modified {mtime}")

    try:
        with open(resolved, "rb") as fh:
            blob = fh.read()
            flags = read_module_flags(blob)
    except OSError as err:
        print(f"    modules: unreadable ({err})")
        print("    request-rate limit: unknown")
        return

    print("    modules (unknown = could not be read, not off):")
    for key, label in _MODULE_LABELS:
        value = flags.get(key)
        mark = "unknown" if value is None else ("on" if value else "off")
        print(f"      [{mark:>7}] {label}")
    try:
        limited, rate = read_speed(blob)
        state = f"enabled at {rate:g} URLs/s" if limited else "disabled in the saved config"
        print(f"    request-rate limit: {state} (read back from config)")
    except ValueError:
        print("    request-rate limit: unknown (speed block could not be read safely)")
    requested = (cfg.get("sf_cli") or {}).get("max_urls_per_second")
    if requested is not None:
        print(
            f"    per-run limit requested: {requested} URLs/s; Mode A will verify a derived config"
        )


def _run_doctor(args) -> int:
    from .core.runner import SF_GLOBS, find_sf_cli

    cfg = load_config(args.config)
    sf = find_sf_cli(cfg, args.sf_cli)
    print("Screaming Frog CLI:", sf or "NOT FOUND")
    if not sf:
        print("  searched: --sf-cli, $SF_CLI, config sf_cli.path, PATH, and:")
        for g in SF_GLOBS:
            print("   -", g)
        print("  → set sf_cli.path in config.json, set $SF_CLI, or pass --sf-cli;")
        print("    otherwise use Mode B: sf-analyzer run --exports-dir ./exports")
    _report_base_config(cfg)

    print("\nDependencies:")
    for mod, what in (
        ("pandas", "core"),
        ("lxml", "core"),
        ("jsonschema", "schema"),
        ("openpyxl", "xlsx"),
        ("advertools", "sitemap (optional)"),
        ("mcp", "MCP server (optional)"),
    ):
        try:
            __import__(mod)
            print(f"  [ok] {mod} ({what})")
        except ImportError:
            print(f"  [--] {mod} ({what})")
    return 0 if sf else 1


def _run_save_config(args) -> int:
    """Copy the latest crawl configuration into a reusable base config."""
    import os
    import shutil

    from .core.spiderconfig import find_base_config, read_module_flags, read_speed

    if os.path.exists(args.out) and not args.force:
        print(f"{args.out} already exists — pass --force to overwrite")
        return 1
    source = find_base_config(None)
    if not source:
        print("No Screaming Frog crawl configuration found to copy.")
        print("  Run one crawl in the SF GUI with the modules you need, then repeat this command.")
        return 1

    shutil.copyfile(source, args.out)
    print(f"Copied {source}\n     -> {args.out}")
    try:
        with open(args.out, "rb") as fh:
            blob = fh.read()
            flags = read_module_flags(blob)
    except OSError as err:
        print(f"  written, but could not be read back: {err}")
        return 0
    try:
        limited, rate = read_speed(blob)
        print(
            "  request-rate limit: "
            + (f"enabled at {rate:g} URLs/s" if limited else "disabled in this config")
        )
    except ValueError:
        print("  request-rate limit: unknown (speed block could not be read safely)")
    on = [label for key, label in _MODULE_LABELS if flags.get(key)]
    print("  modules on: " + (", ".join(on) if on else "none"))
    if not on:
        print("  → nothing module-dependent will run; enable the modules in the SF GUI and repeat.")
    return 0


def _resolve_input(args: argparse.Namespace) -> tuple[str, str | None, str | None]:
    if args.load_crawl:
        return "load-crawl", args.load_crawl, None
    if args.crawl:
        return "crawl", args.crawl, None
    if args.crawl_list:
        return "crawl-list", args.crawl_list, None
    return "parse-exports", None, args.exports_dir


def _run_tasks(args) -> int:
    from seohead.sf.tasks import build_tasks_from_audit_v2
    from seohead.storage.audit_v2 import audit_v2_path
    from seohead.storage.inputs import resolve_audit_input

    try:
        if audit_v2_path(args.audit_json).exists():
            audit, diagnostics = (
                None,
                [{"code": "audit_v2_stream", "message": "tasks read the audit.v2 findings stream"}],
            )
        else:
            audit, diagnostics = resolve_audit_input(args.audit_json)
    except (OSError, ValueError) as err:
        print(f"error: cannot read audit json: {err}", file=sys.stderr)
        return 1
    for notice in diagnostics:
        print(f"[{notice['code']}] {notice['message']}", file=sys.stderr)
    cfg = load_config(args.config)
    backlog = (
        build_tasks_from_audit_v2(args.audit_json, cfg)
        if audit is None
        else build_tasks(audit, cfg)
    )
    os.makedirs(args.out, exist_ok=True)
    jp, mp = write_tasks(
        backlog, os.path.join(args.out, "tasks.json"), os.path.join(args.out, "tasks.md")
    )
    if not args.quiet:
        s = backlog["summary"]
        print(
            f"tasks={s['tasks_total']} "
            f"{' '.join(f'{k}={v}' for k, v in sorted(s['by_priority'].items()))}"
        )
        print(f"[out] {jp}\n[out] {mp}", file=sys.stderr)
    return 0


def _run_snippet_edits(args) -> int:
    import json
    from pathlib import Path

    from .snippet_edits import (
        attach_retained_context,
        attach_retained_measurements,
        build_snippet_tasks,
        measure_snippet_edits,
        parse_snippet_csv,
        write_snippet_tasks,
    )

    try:
        text = Path(args.csv).read_text(encoding="utf-8-sig")
        parsed = parse_snippet_csv(text)
        tasks = build_snippet_tasks(parsed)
        if args.query and not args.scan:
            raise ValueError("--query requires --scan")
        metrics = (
            json.loads(Path(args.metrics_json).read_text(encoding="utf-8"))
            if args.metrics_json
            else None
        )
        if metrics is not None:
            tasks["measurements"] = measure_snippet_edits(parsed["edits"], metrics)
        if args.scan:
            content_area = (
                json.loads(Path(args.content_area_json).read_text(encoding="utf-8"))
                if args.content_area_json
                else None
            )
            tasks = attach_retained_context(
                tasks,
                args.scan,
                representation=args.representation,
                variant_key=args.variant_key,
                query=args.query,
                content_area=content_area,
                max_decoded_bytes=args.max_body_bytes,
            )
            if metrics is not None:
                tasks = attach_retained_measurements(tasks, metrics)
        path = write_snippet_tasks(tasks, args.out)
    except (OSError, ValueError, json.JSONDecodeError) as err:
        print(f"error: cannot export snippet tasks: {err}", file=sys.stderr)
        return 1
    print(f"wrote {tasks['summary']['tasks']} snippet task(s) to {path}")
    if tasks["summary"]["rejected_rows"]:
        print(f"rejected rows: {tasks['summary']['rejected_rows']}", file=sys.stderr)
    return 0


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.command == "tasks":
        return _run_tasks(args)
    if args.command == "doctor":
        return _run_doctor(args)
    if args.command == "save-config":
        return _run_save_config(args)
    if args.command == "snippet-edits":
        return _run_snippet_edits(args)
    if args.command != "run":
        return 1

    input_mode, source, exports_dir = _resolve_input(args)
    observed: tuple[str, str] | None = None
    observed_phase: str | None = None

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

    if args.project is not None:
        import hashlib
        import json
        from pathlib import Path

        from seohead.projects.run_observation import start
        from seohead.projects.workspace import open_project

        project = open_project(args.project)
        root = Path(project["path"])
        try:
            artifact = Path(args.out).resolve().relative_to(root.resolve())
        except (OSError, ValueError):
            artifact = None
        config = load_config(args.config)
        fingerprint = hashlib.sha256(
            json.dumps(config, sort_keys=True, separators=(",", ":")).encode("utf-8")
        ).hexdigest()
        run = start(
            root,
            kind="screaming_frog",
            mode="sf_live" if input_mode in {"crawl", "crawl-list", "load-crawl"} else "sf_exports",
            max_urls=0,
            config_fingerprint=fingerprint,
            artifact=root / artifact if artifact is not None else None,
            counters={"fetched": None, "queued": None, "inflight": None, "excluded": None},
        )
        observed = (str(root), run["id"])
        from seohead.projects.run_observation import NativeRunReporter

        run_reporter = NativeRunReporter(root, run["id"], source="sf")
        record_phase(
            "collection" if input_mode in {"crawl", "crawl-list", "load-crawl"} else "analysis"
        )

    def log(msg: str) -> None:
        if msg.startswith("[audit] exports written") or msg.startswith("[audit] loaded exports"):
            record_phase("analysis")
        elif msg.startswith("[runner]"):
            record_phase("collection")
        if args.verbose and not args.quiet:  # progress only when asked
            print(msg, file=sys.stderr)

    def collector_started(pid: int) -> None:
        if observed is None:
            return
        from seohead.projects.run_observation import collector_started as record_collector

        try:
            record_collector(observed[0], observed[1], pid)
        except (OSError, ValueError):
            return

    if input_mode in ("crawl", "crawl-list") and not args.quiet:
        # Mode B already has the exports; only a fresh crawl can still be fixed.
        for warning in preflight_warnings(
            (load_config(args.config).get("sf_cli") or {}).get("seospiderconfig")
        ):
            print(f"[preflight] {warning}", file=sys.stderr)
    try:
        # Protected environments support two distinct authentication mechanisms.
        # Form login remains an SF-owned profile file; Basic authentication is
        # implemented at runtime without persisting credentials in the config.
        cfg_overrides: dict = {}
        if getattr(args, "auth_config", None):
            cfg_overrides.setdefault("sf_cli", {})["auth_config"] = args.auth_config
        if getattr(args, "max_urls_per_second", None) is not None:
            cfg_overrides.setdefault("sf_cli", {})["max_urls_per_second"] = args.max_urls_per_second
        # SF does not reliably accept Basic credentials in the URL or through a
        # CLI flag. A loopback proxy therefore injects the Authorization header;
        # exported URLs are rewritten back to the original origin afterwards.
        auth_proxy = None
        if getattr(args, "auth", None) and source and source.startswith("http"):
            from .core.auth_proxy import AuthProxy, parse_auth

            try:
                user, password = parse_auth(args.auth)
            except ValueError as error:
                raise SystemExit(str(error)) from error

            # rewrite_exports only understands CSV; an XLSX export behind the
            # proxy would reach the analyzer full of 127.0.0.1 URLs (#217).
            # Refuse before a single request goes out rather than produce a
            # report that looks complete but points at a dead loopback port.
            export_format = (load_config(args.config).get("sf_cli") or {}).get(
                "export_format", "csv"
            )
            if str(export_format).strip().lower() != "csv":
                raise SystemExit(
                    f"--auth does not support sf_cli.export_format={export_format!r}: "
                    "the loopback proxy's URL rewrite only covers CSV exports. "
                    "Set sf_cli.export_format to csv, or drop --auth."
                )

            auth_proxy = AuthProxy(source, user, password)
            proxy_base = auth_proxy.start()
            source = source.replace(auth_proxy.origin, proxy_base)
            cfg_overrides.setdefault("sf_cli", {})["_trusted_loopback_proxy"] = proxy_base
            log(f"[cli] protected site: crawling through local auth proxy {proxy_base}")

        try:
            from seohead.cli.terminal_progress import elapsed_progress, show_banner

            licensed_run = input_mode in {"crawl", "crawl-list", "load-crawl"}
            observer_kwargs = (
                {"on_started": collector_started, "on_progress": run_reporter.observe_counts}
                if observed is not None
                else {}
            )
            if licensed_run:
                show_banner(
                    "Screaming Frog run started; measured counters appear in the project observer when supported.",
                    quiet=args.quiet,
                )
            with elapsed_progress("Screaming Frog run", enabled=licensed_run and not args.quiet):
                result = run_audit(
                    input_mode=input_mode,
                    source=source,
                    exports_dir=exports_dir,
                    config_path=args.config,
                    config_overrides=cfg_overrides or None,
                    sf_cli=args.sf_cli,
                    sitemap_url=args.sitemap,
                    profile=args.profile,
                    fetch_all_inlinks=args.fetch_all_inlinks or None,
                    live_recheck=args.live_recheck,
                    output_dir=os.path.join(args.out, "exports"),
                    project_dir=args.project,
                    inspect_gsc=args.gsc_inspect,
                    url_rewrite=(auth_proxy.base_url, auth_proxy.origin) if auth_proxy else None,
                    log=log,
                    **observer_kwargs,
                )
        finally:
            # Bound to the crawl attempt, not the happy path: a failure here must
            # not leave a credentialed proxy bound to a loopback port (#263).
            if auth_proxy:
                auth_proxy.stop()
    # The CLI converts any core failure into a concise user-facing error; verbose
    # mode still exposes the traceback for diagnosis.
    except Exception as err:
        if observed is not None:
            from seohead.projects.run_observation import finish

            with contextlib.suppress(OSError, ValueError):
                finish(observed[0], observed[1], state="failed", reason=type(err).__name__)
        print(f"error: {err}", file=sys.stderr)
        if args.verbose:
            import traceback

            traceback.print_exc()
        return 1

    formats = {f.strip().lower() for f in args.format.split(",") if f.strip()}
    os.makedirs(args.out, exist_ok=True)
    if "json" in formats:
        path = write_json(result, os.path.join(args.out, "audit.json"))
        log(f"[out] {path}")
    if "md" in formats:
        path = write_markdown(result, os.path.join(args.out, "audit.md"))
        log(f"[out] {path}")
    if args.tasks:
        backlog = build_tasks(to_dict(result), load_config(args.config))
        jp, mp = write_tasks(
            backlog, os.path.join(args.out, "tasks.json"), os.path.join(args.out, "tasks.md")
        )
        log(f"[out] {jp}")
        log(f"[out] {mp}")

    crawl_valid = result.run.get("crawl_valid") is not False
    if observed is not None:
        from seohead.projects.run_observation import finish

        with contextlib.suppress(OSError, ValueError):
            finish(
                observed[0],
                observed[1],
                state="finished" if crawl_valid else "failed",
                reason="finished" if crawl_valid else "crawl_invalid",
            )
    if not args.quiet:
        sev = result.summary["by_severity"]
        health = result.summary["health_score"]
        health_text = "n/a" if health is None else str(health)
        print(
            f"health={health_text} "
            f"critical={sev['critical']} warning={sev['warning']} notice={sev['notice']} "
            f"issues={result.summary['totals']['issues_total']}"
        )
        if not crawl_valid:
            reason = result.run.get("crawl_invalid_reason") or "the crawl produced no usable data"
            print(f"crawl failed: {reason} — no health score", file=sys.stderr)

    # A run that never crawled anything must not record a success, whatever
    # --fail-on says about severities: there were no severities to judge.
    if not crawl_valid:
        return 2

    if args.fail_on == "critical" and result.summary["by_severity"]["critical"] > 0:
        return 2
    if args.fail_on == "warning" and (
        result.summary["by_severity"]["critical"] > 0
        or result.summary["by_severity"]["warning"] > 0
    ):
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
