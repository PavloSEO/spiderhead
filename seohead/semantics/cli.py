"""``seohead semantics <stage> --project DIR``: argument mapping onto the shared runner."""

from __future__ import annotations

import argparse
import contextlib
import io
import json
import sqlite3
import sys
from pathlib import Path

from seohead.semantics import PAID_STAGES, STAGES

_HELP = {
    "init": "create project.yaml, seeds.txt and the SQLite database",
    "import": "preview or import mapped CSV/XLSX phrase data",
    "collect": "PAID: expand seeds through Yandex Wordstat",
    "clean": "free: statuses, routes and lemma groups from project filters",
    "graph": "free: word graph, homonyms and candidate clusters (anomalies.md)",
    "exact": "PAID: exact !W frequency through Arsenkin, once, at the end",
    "cluster": "PAID unless cached: SERP clustering (Yandex Search API)",
    "competitors": "free: competitor domains and title candidates from cached SERP",
    "synonyms": "PAID: check synonyms.txt candidates in Wordstat",
    "report": "free: report.md and clusters.csv",
    "excel": "free: final XLSX workbook",
    "mine": "web: candidate phrases from competitor page headings",
    "sitematch": "free: send phrases without a catalog.txt term to review",
    "relevance": "free: flag phrases whose SERP has no vendors.txt peer",
    "status": "read-only counters, stage state, provider units and the next stage",
    "export": "write pool.csv and pool.json",
    "clusters": "read paginated cluster aggregates",
    "cluster-show": "read a cluster, candidate landings and evidence",
    "cluster-queries": "read paginated phrases for a cluster",
    "set-landing": "set or clear a landing URL and retain an audit record",
    "insights": "read period-scoped rules and source evidence without provider calls",
    "insights-tasks": "preview or create selected semantic insight checklist tasks",
}


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="seohead semantics",
        description="Accumulating semantic core: one SQLite database per project.",
    )
    sub = parser.add_subparsers(dest="stage", required=True, metavar="<stage>")
    for name in STAGES:
        command = sub.add_parser(name, help=_HELP[name])
        command.add_argument(
            "--project",
            required=True,
            help="semantic project directory, or a SEOHEAD project (uses its semantics/)",
        )
        if name in {
            "status",
            "import",
            "export",
            "excel",
            *PAID_STAGES,
            "clusters",
            "cluster-show",
            "cluster-queries",
            "insights",
            "insights-tasks",
            "set-landing",
        }:
            command.add_argument("--json", action="store_true")
        if name in {"import", "export", "excel", "insights-tasks", *PAID_STAGES}:
            command.add_argument("--dry-run", action="store_true")
        if name in ("export", "graph", "competitors", "report", "excel", "mine"):
            command.add_argument("--out", type=Path, help="output directory")
        if name == "import":
            command.add_argument("--file", type=Path, required=True, help="CSV or XLSX file")
            command.add_argument("--sheet", help="worksheet name for XLSX")
            command.add_argument(
                "--map", dest="mapping", action="append", default=[], metavar="FIELD=COLUMN"
            )
            command.add_argument("--merge", choices=("add", "update", "replace"), default="add")
        if name in {"export", "excel"}:
            command.add_argument("--scope", choices=("all", "no-landing"), default="all")
            command.add_argument("--cluster", help="scope output to a cluster by exact name")
        if name == "collect":
            command.add_argument("--resume", action="store_true")
            command.add_argument("--max-seeds", type=int, default=0)
            command.add_argument("--max-phrases", type=int, default=100000)
        if name in ("exact", "cluster"):
            command.add_argument("--limit", type=int, default=0, help="debugging only")
        if name == "exact":
            command.add_argument(
                "--yes", action="store_true", help="allow the configured batch size"
            )
        if name == "cluster":
            command.add_argument("--method", choices=("serp", "louvain"), default="serp")
        if name in PAID_STAGES:
            command.add_argument(
                "--confirm-paid",
                action="store_true",
                help="confirm the paid estimate shown by a prior --dry-run --json",
            )
        if name in {"clusters", "cluster-queries"}:
            command.add_argument("--offset", type=int, default=0)
            command.add_argument("--limit", type=int, default=50)
        if name in {"cluster-show", "cluster-queries", "set-landing"}:
            command.add_argument("--cluster", required=True, help="cluster id or exact name")
        if name == "set-landing":
            command.add_argument(
                "--url", required=True, help="landing URL, or empty string to clear"
            )
        if name in {"insights", "insights-tasks"}:
            command.add_argument(
                "--period", required=True, help="inclusive YYYY-MM-DD..YYYY-MM-DD period"
            )
            command.add_argument("--sources", default="gsc,metrika,webmaster")
        if name == "insights-tasks":
            command.add_argument(
                "--select",
                dest="selections",
                action="append",
                required=True,
                metavar="RULE_ID:CLUSTER_ID",
            )
            command.add_argument("--expected-revision", type=int)
            command.add_argument(
                "--confirm",
                action="store_true",
                help="write only the explicitly selected matched rule/cluster pairs",
            )
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    for field in ("limit", "max_seeds", "max_phrases", "offset"):
        if getattr(args, field, 0) < 0:
            parser.error(f"--{field.replace('_', '-')} must be non-negative")
    options = vars(args)
    stage, project = options.pop("stage"), options.pop("project")
    json_output = (
        options.get("json", False)
        or options.get("dry_run", False)
        or (stage in PAID_STAGES and not options.get("confirm_paid"))
        or stage
        in {
            "clusters",
            "cluster-show",
            "cluster-queries",
            "set-landing",
            "insights",
            "insights-tasks",
        }
    )
    if "mapping" in options:
        parsed = {}
        for item in options.pop("mapping"):
            if "=" not in item:
                parser.error("--map must use FIELD=COLUMN")
            key, value = item.split("=", 1)
            if not key or not value:
                parser.error("--map must use FIELD=COLUMN")
            parsed[key] = value
        options["mapping"] = parsed
    if stage == "insights-tasks":
        selections = []
        for item in options.pop("selections"):
            if ":" not in item:
                parser.error("--select must use RULE_ID:CLUSTER_ID")
            rule_id, cluster_id = item.rsplit(":", 1)
            if not rule_id or not cluster_id.isdecimal():
                parser.error("--select must use RULE_ID:CLUSTER_ID")
            selections.append({"rule_id": rule_id, "cluster_id": int(cluster_id)})
        options["selections"] = selections
    if stage in {"export", "excel"} and options.get("cluster"):
        if options.get("scope") != "all":
            parser.error("--cluster cannot be combined with a non-default --scope")
        options["scope"] = f"cluster:{options.pop('cluster')}"
    try:
        from seohead.semantics.runner import execute

        if json_output:
            with contextlib.redirect_stdout(io.StringIO()) as logs:
                result = execute(stage, project, **options)
            result = {**result, "logs": logs.getvalue()}
            print(json.dumps(result, ensure_ascii=False))
        else:
            result = execute(stage, project, **options)
    except ModuleNotFoundError as error:
        print(
            f"seohead semantics: missing optional dependency {error.name!r}; "
            "install it with: pip install 'seohead-seotools[semantics]'",
            file=sys.stderr,
        )
        return 1
    except (OSError, ValueError, RuntimeError, sqlite3.Error) as error:
        print(f"seohead semantics: {error}", file=sys.stderr)
        return 1
    if stage in PAID_STAGES and options.get("confirm_paid"):
        print("provider charges: seohead spend-report", file=sys.stderr)
    return 0
