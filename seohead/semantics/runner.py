"""Shared stage dispatch for the CLI and the MCP tools."""

from __future__ import annotations

import contextlib
import importlib
import io
import json
import math
import sqlite3
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from seohead.data_sources import spend
from seohead.semantics import PAID_STAGES, STAGES, config
from seohead.semantics.store import Store

# Stages whose phrase table changes and is exported to pool.csv/pool.json afterwards.
_EXPORTING = frozenset(
    {
        "import",
        "collect",
        "clean",
        "graph",
        "exact",
        "cluster",
        "synonyms",
        "sitematch",
        "relevance",
    }
)
_MODULES = {"import": "import_phrases"}


def _store(project, create=False, readonly=False):
    db = config.db_path(project)
    if not create and not db.is_file():
        raise FileNotFoundError(
            f"semantic database not found: {db}\n"
            f"Initialize it with: seohead semantics init --project {project}"
        )
    return Store(db, readonly=readonly)


def _status(store, project):
    counts = store.counts()
    following = (
        "collect"
        if counts["_total"] == 0
        else "clean"
        if counts.get("new", 0)
        else "graph"
        if counts.get("kept", 0) and not counts.get("review", 0)
        else "exact"
        if counts["_exact"] < counts.get("kept", 0)
        else "cluster"
    )
    has_journal = store.db.execute(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='semantic_stage_runs'"
    ).fetchone()
    states = store.stage_states() if has_journal else None
    return {
        "contract": "seohead.semantics.operation.v1",
        "mode": "read",
        "state": "complete",
        "project": str(project),
        "database": str(config.db_path(project)),
        "counts": counts,
        "spend": store.spend(),
        "next_stage": following,
        "stages": states,
        "stages_unavailable_reason": None
        if states is not None
        else "stage journal is absent from this legacy database",
    }


def paid_estimate(stage: str, project: str | Path, **options: Any) -> dict[str, Any]:
    """Calculate a local preflight without provider construction or network access."""
    directory = config.data_dir(project)
    cfg = config.load(project)
    blocked_items = 0
    blocked_reason = None
    with _store(project, readonly=True) as store:
        if stage == "collect":
            seed_file = directory / cfg.get("seeds_file", "seeds.txt")
            seeds = [line.strip() for line in seed_file.read_text(encoding="utf-8").splitlines() if line.strip()] if seed_file.exists() else []
            expanded = {r[0] for r in store.db.execute("SELECT seed_norm FROM expansions")}
            has_attempts = store.db.execute(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='collect_attempts'"
            ).fetchone()
            attempts = store.db.execute("SELECT seed,state,depth FROM collect_attempts").fetchall() if has_attempts else []
            blocked = {
                r[0] for r in store.db.execute(
                    "SELECT seed FROM collect_attempts WHERE state IN ('submitting','unknown')"
                )
            } if has_attempts else set()
            from seohead.semantics.norm import Filters, normalize

            candidates = {normalize(seed) for seed in seeds}
            if options.get("resume"):
                candidates.update(normalize(row["seed"]) for row in attempts if row["state"] == "retryable")
                filters = Filters.from_config(cfg)
                floor, max_depth = cfg.get("reexpand_floor", 40), cfg.get("max_depth", 3)
                for row in store.db.execute(
                    "SELECT norm,depth FROM phrases WHERE base>=? AND status IN ('new','kept')",
                    (floor,),
                ):
                    phrase, depth = row["norm"], row["depth"] or 0
                    if depth <= max_depth and not filters.is_stop(phrase) and filters.has_intent(phrase) and len(phrase.split()) >= 2:
                        candidates.add(normalize(phrase))
            initial = candidates - expanded - blocked - {""}
            count = len(initial)
            limit = int(options.get("max_seeds", 0) or 0)
            if limit:
                count = min(count, limit)
            max_phrases = int(options.get("max_phrases", 100000))
            total_phrases = store.counts()["_total"]
            maximum = (
                0
                if total_phrases >= max_phrases
                else min(limit, max_phrases) if limit else max_phrases
            )
            count = min(count, maximum)
            provider, unit, tasks = "yandex_wordstat", "requests", count
        elif stage == "synonyms":
            from seohead.semantics.norm import normalize

            path = directory / "synonyms.txt"
            candidates = {normalize(line) for line in path.read_text(encoding="utf-8").splitlines()} if path.exists() else set()
            known = {r[0] for r in store.db.execute("SELECT norm FROM phrases")}
            count = len({item for item in candidates if item and item not in known})
            provider, unit, tasks = "yandex_wordstat", "requests", count
        elif stage == "cluster":
            rows = store.db.execute("SELECT COUNT(*) FROM phrases WHERE status='kept'").fetchone()[0]
            region = str((cfg.get("regions") or ["225"])[0])
            receipts = store.db.execute(
                "SELECT f.phrase_norm,f.state FROM serp_fetches f JOIN phrases p ON p.norm=f.phrase_norm "
                "WHERE f.region=? AND f.top=10 AND p.status='kept'",
                (region,),
            ).fetchall() if store.db.execute(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='serp_fetches'"
            ).fetchone() else []
            complete = {r[0] for r in receipts if r[1] == "complete"}
            blocked_items = sum(r[1] != "complete" for r in receipts)
            if blocked_items:
                blocked_reason = "unresolved SERP receipts must be reconciled before another paid cluster run"
            all_receipts = {
                r[0] for r in store.db.execute(
                    "SELECT DISTINCT f.phrase_norm FROM serp_fetches f JOIN phrases p ON p.norm=f.phrase_norm "
                    "WHERE p.status='kept'"
                )
            } if store.db.execute(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='serp_fetches'"
            ).fetchone() else set()
            legacy = {
                r[0] for r in store.db.execute(
                    "SELECT DISTINCT s.phrase_norm FROM serp s JOIN phrases p ON p.norm=s.phrase_norm "
                    "WHERE p.status='kept'"
                )
            } - all_receipts
            count = max(0, rows - len(complete | legacy))
            if blocked_items:
                count = 0
            limit = int(options.get("limit", 0) or 0)
            if limit:
                count = min(count, limit)
            provider, unit, tasks = "yandex_search", "requests", count
        elif stage == "exact":
            rows = store.db.execute(
                "SELECT lemma_group,norm FROM phrases WHERE status='kept' AND exact IS NULL"
            ).fetchall()
            groups: dict[object, list[str]] = {}
            for row in rows:
                key = row["lemma_group"] if row["lemma_group"] is not None else f"solo:{row['norm']}"
                groups.setdefault(key, []).append(row["norm"])
            claimed = set()
            has_tasks = store.db.execute(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='provider_tasks'"
            ).fetchone()
            if has_tasks:
                from seohead.semantics.norm import normalize

                region_id = int(str((cfg.get("regions") or [225])[0]))
                for task in store.db.execute(
                    "SELECT payload_json FROM provider_tasks WHERE provider='arsenkin' "
                    "AND stage='exact' AND region=?",
                    (region_id,),
                ):
                    claimed.update(normalize(value) for value in json.loads(task[0]).get("queries", []))
            count = sum(not claimed.intersection(members) for members in groups.values())
            limit = int(options.get("limit", 0) or 0)
            if limit:
                count = min(count, limit)
            provider, unit, tasks = "arsenkin_wordstat", "tasks", math.ceil(count / 100)
        else:
            raise ValueError(f"{stage} is not a paid semantic stage")
        choices = {
            "region": {"selected": (cfg.get("regions") or ["225"])[0], "available": cfg.get("regions") or ["225"]},
            "engine": {
                "selected": {"collect": "yandex_wordstat", "synonyms": "yandex_wordstat", "cluster": "yandex_search", "exact": "arsenkin_wordstat"}[stage],
                "available": ["yandex_wordstat"] if stage in {"collect", "synonyms"} else ["yandex_search"] if stage == "cluster" else ["arsenkin_wordstat"],
            },
            "query_source": {
                "selected": "seeds_file" if stage == "collect" else "synonyms_file" if stage == "synonyms" else "kept_phrases",
                "available": ["seeds_file", "synonyms_file", "kept_phrases"],
            },
        }
        request_estimate = {"unit": unit, "count": tasks, "kind": "upper_bound"}
        if stage == "collect":
            request_estimate.update({"count": None, "minimum": 0, "known_initial_requests": count, "maximum": maximum, "kind": "bounded_range"})
        return {
            "contract": "seohead.semantics.operation.v1",
            "mode": "estimate",
            "state": "complete",
            "format": "seohead.semantics.paid-estimate.v1",
            "stage": stage,
            "provider": provider,
            "request_estimate": request_estimate,
            "item_estimate": count,
            "choices": choices,
            "money_estimate": None,
            "money_unavailable_reason": "provider pricing is not available from this offline preflight",
            "paid_calls_made": 0,
            "estimate_kind": "upper_bound",
            "blocked_items": blocked_items,
            "blocked_reason": blocked_reason,
        }


def execute(stage: str, project: str | Path, **options: Any) -> dict[str, Any]:
    """Run one registered stage against a project directory and return its summary."""
    if stage not in STAGES:
        raise ValueError(f"stage must be one of: {', '.join(STAGES)}")
    args = SimpleNamespace(**{"out": None, **options})
    directory = config.data_dir(project)
    if stage in PAID_STAGES:
        estimate = paid_estimate(stage, project, **options)
        if getattr(args, "dry_run", False) or not getattr(args, "confirm_paid", False):
            return {
                "contract": "seohead.semantics.operation.v1",
                "mode": "estimate" if getattr(args, "dry_run", False) else "confirmation_required",
                "state": "complete",
                "project": str(project),
                "stage": stage,
                "paid": True,
                "estimate": estimate,
                "requires_confirmation": not getattr(args, "dry_run", False),
            }
    if stage == "init":
        config.ensure_project(project)
        with _store(project, create=True) as store:
            run_id = store.start_stage("init")
            store.finish_stage(run_id, "done", result={"directory": str(directory)})
        print(f"init OK: {directory}")
        return {"project": str(project), "stage": "init", "directory": str(directory)}
    out = Path(args.out) if args.out else directory
    readonly_stages = {"status", "clusters", "cluster-show", "cluster-queries", "insights"}
    dry_read = getattr(args, "dry_run", False) and stage in {"import", "export", "excel", "insights-tasks"}
    with _store(project, readonly=stage in readonly_stages or dry_read) as store:
        if stage == "status":
            result = _status(store, project)
            if not getattr(args, "json", False):
                print(f"=== {project} ===")
                print(" | ".join(f"{key}={value}" for key, value in result["counts"].items()))
                print(f"provider units: {result['spend']}")
                print(f"next: seohead semantics {result['next_stage']} --project {project}")
            return result
        if stage == "export":
            scope = getattr(args, "scope", "all")
            result = store.export(out, scope=scope, dry_run=getattr(args, "dry_run", False))
            if not getattr(args, "dry_run", False):
                result = {
                    "contract": "seohead.semantics.operation.v1",
                    "mode": "write",
                    "state": "complete",
                    "scope": scope,
                    "rows": result,
                    "files": [str(out / "pool.csv"), str(out / "pool.json")],
                }
            if not getattr(args, "json", False):
                print(f"export: {result['rows']} phrases -> {', '.join(result.get('planned_files', result.get('files', [])))}")
            return {"project": str(project), "stage": "export", **result}
        if stage == "import" and getattr(args, "dry_run", False):
            module = importlib.import_module("seohead.semantics.stages.import_phrases")
            return module.plan(
                store,
                config.load(project),
                args.file,
                sheet=getattr(args, "sheet", None),
                mapping=getattr(args, "mapping", None),
                merge=getattr(args, "merge", "add"),
            )
        if stage == "excel" and getattr(args, "dry_run", False):
            clause, params = store.scope_clause(getattr(args, "scope", "all"), "p")
            count = store.db.execute("SELECT COUNT(*) FROM phrases p " + clause, params).fetchone()[0]
            return {
                "contract": "seohead.semantics.operation.v1",
                "mode": "preview",
                "state": "complete",
                "format": "seohead.semantics.export-plan.v1",
                "scope": getattr(args, "scope", "all"),
                "rows": count,
                "planned_files": [str(out / f"{config.load(project)['_project']}_semantic_core.xlsx")],
            }
        if stage in {"clusters", "cluster-show", "cluster-queries", "set-landing", "insights", "insights-tasks"}:
            from seohead.semantics.read_model import execute_read

            journaled = stage == "set-landing" or (
                stage == "insights-tasks" and not getattr(args, "dry_run", False)
            )
            run_id = store.start_stage(stage) if journaled else None
            try:
                result = execute_read(store, project, stage, args)
                if run_id is not None:
                    store.finish_stage(run_id, "done", result=result)
                return result
            except Exception as error:
                if run_id is not None:
                    store.db.rollback()
                    store.finish_stage(run_id, "failed", error=str(error))
                raise
        cfg = config.load(project)
        module = importlib.import_module(f"seohead.semantics.stages.{_MODULES.get(stage, stage)}")
        tags = {"semantics_project": str(directory), "semantics_stage": stage}
        estimate = paid_estimate(stage, project, **options) if stage in PAID_STAGES else None
        run_id = store.start_stage(stage, estimate=estimate)
        try:
            with spend.context(**tags) if stage in PAID_STAGES else contextlib.nullcontext():
                summary = _run_stage(module, stage, store, cfg, directory, out, args)
            if stage == "cluster":
                from seohead.semantics.read_model import refresh_landing_candidates

                own_domain = cfg.get("domain")
                project_root = directory.parent if directory.name == "semantics" else directory
                if (project_root / "project.json").is_file():
                    from seohead.projects.workspace import _load

                    _, document = _load(project_root)
                    own_domain = document["site"]["host"]
                summary["landing_candidates"] = refresh_landing_candidates(store, own_domain)
            store.finish_stage(run_id, "done", result=summary)
        except Exception as error:
            store.db.rollback()
            store.finish_stage(run_id, "failed", error=str(error))
            raise
        if stage in _EXPORTING:
            store.export(out)
        return {
            "contract": "seohead.semantics.operation.v1",
            "mode": "write",
            "state": "complete",
            "project": str(project),
            "stage": stage,
            "result": summary,
            "estimate": estimate,
            "counts": store.counts(),
            "spend": store.spend(),
        }


def _run_stage(module, stage, store, cfg, directory, out, args):
    if stage == "collect":
        seed_file = directory / cfg.get("seeds_file", "seeds.txt")
        seeds = (
            [line.strip() for line in seed_file.read_text(encoding="utf-8").splitlines()]
            if seed_file.exists()
            else []
        )
        seeds = [s for s in seeds if s]
        if not seeds and not args.resume:
            raise ValueError(f"no seeds: {seed_file}")
        return module.run(
            store,
            cfg,
            seeds=seeds,
            max_seeds=args.max_seeds,
            max_phrases=args.max_phrases,
            resume=args.resume,
        )
    if stage == "import":
        return module.run(
            store, cfg, args.file, sheet=getattr(args, "sheet", None),
            mapping=getattr(args, "mapping", None), merge=getattr(args, "merge", "add")
        )
    if stage == "exact":
        return module.run(store, cfg, yes=args.yes, limit=args.limit)
    if stage == "cluster":
        return module.run(store, cfg, limit=args.limit, method=args.method)
    if stage == "excel":
        return module.run(store, cfg, out, scope=getattr(args, "scope", "all"))
    if stage in ("graph", "competitors", "report", "mine"):
        return module.run(store, cfg, out)
    return module.run(store, cfg)


def run_quiet(
    stage: str,
    project: str | Path,
    *,
    allowed: frozenset[str] | tuple[str, ...],
    confirm_paid: bool = False,
    **options: Any,
) -> dict[str, Any]:
    """Run a stage for an MCP caller; progress lines go to ``logs`` instead of stdout.

    A paid stage runs only with ``confirm_paid=True``; without it the reply names the stage
    and spends nothing. Failures are returned as ``ok: false`` rather than raised.
    """
    if stage not in allowed:
        return {"ok": False, "error": f"stage must be one of: {', '.join(sorted(allowed))}"}
    if stage in PAID_STAGES and not confirm_paid:
        try:
            estimate = paid_estimate(stage, project, **options)
            return {
                "ok": True,
                "result": {
                    "stage": stage,
                    "paid": True,
                    "estimate": estimate,
                    "requires_confirmation": True,
                    "paid_calls_made": 0,
                },
                "logs": "",
            }
        except (OSError, ValueError, sqlite3.Error) as exc:
            return {"ok": False, "error": str(exc), "paid": True}
    options.setdefault("json", True)
    options.setdefault("resume", False)
    options.setdefault("max_seeds", 0)
    options.setdefault("max_phrases", 100000)
    options.setdefault("limit", 0)
    options.setdefault("yes", False)
    options.setdefault("method", "serp")
    options["confirm_paid"] = confirm_paid
    if stage == "insights-tasks":
        options["_mcp_call"] = True
    with contextlib.redirect_stdout(io.StringIO()) as logs:
        try:
            result = execute(stage, project, **options)
        except (OSError, ValueError, RuntimeError, ImportError, sqlite3.Error) as exc:
            return {"ok": False, "error": str(exc), "logs": logs.getvalue()}
    return {"ok": True, "result": result, "logs": logs.getvalue()}
