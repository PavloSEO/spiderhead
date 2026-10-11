from __future__ import annotations

import csv
import json
import sqlite3

import pytest

from seohead.semantics.store import Store

pytestmark = pytest.mark.usefixtures("semantics_offline")


def test_upsert_accumulates_the_highest_demand_without_erasing_live_evidence(sem_store):
    assert sem_store.upsert("краулер сайта онлайн", base=120, src="import") is True
    sem_store.set_fields("краулер сайта онлайн", impr=47, pos_y=3, pos_g=8, exact=None)
    assert sem_store.upsert("краулер сайта онлайн", base=12, src="collect") is False
    assert sem_store.upsert("краулер сайта онлайн", base=None, src="collect") is False
    sem_store.commit()

    row = sem_store.db.execute(
        "SELECT * FROM phrases WHERE norm=?", ("краулер сайта онлайн",)
    ).fetchone()
    assert row["base"] == 120
    assert (row["impr"], row["pos_y"], row["pos_g"], row["exact"]) == (47, 3, 8, None)


def test_legacy_database_migrates_without_losing_existing_phrase_data(tmp_path):
    db_path = tmp_path / "legacy.db"
    db = sqlite3.connect(db_path)
    # A released pre-extension schema: it has the core tables and indexes' columns,
    # but none of the later real-demand, SERP-position and multi-level cluster fields.
    db.execute("""CREATE TABLE phrases(
        id INTEGER PRIMARY KEY, norm TEXT NOT NULL UNIQUE, base INTEGER, quoted INTEGER, exact INTEGER,
        region TEXT, depth INTEGER, src TEXT, status TEXT, reason TEXT, reason_stage TEXT,
        route TEXT, lemma_group INTEGER, cluster_id INTEGER, first_seen TEXT, last_update TEXT
    )""")
    db.execute(
        "CREATE TABLE clusters(id INTEGER PRIMARY KEY, name TEXT, marker_norm TEXT, coherence REAL, is_real INTEGER, note TEXT)"
    )
    db.execute(
        "INSERT INTO phrases(norm, base, status) VALUES(?, ?, ?)",
        ("парсер для seo", 91, "kept"),
    )
    db.commit()
    db.close()

    sem_store = Store(db_path)
    try:
        row = sem_store.db.execute(
            "SELECT norm, base, status, impr, pos_y, pos_g, page_types FROM phrases"
        ).fetchone()
        assert dict(row) == {
            "norm": "парсер для seo",
            "base": 91,
            "status": "kept",
            "impr": None,
            "pos_y": None,
            "pos_g": None,
            "page_types": None,
        }
        columns = {item["name"] for item in sem_store.db.execute("PRAGMA table_info(phrases)")}
        assert {"cluster_soft", "cluster_mid", "cluster_hard", "pos_serp"} <= columns
    finally:
        sem_store.close()


def test_export_keeps_nullable_frequency_and_real_demand_columns(sem_store, tmp_path):
    sem_store.upsert("краулер сайта", base=55, src="webmaster")
    sem_store.set_fields("краулер сайта", impr=21, pos_y=6, pos_g=None, quoted=None, exact=None)
    sem_store.commit()

    sem_store.export(tmp_path / "export")
    with (tmp_path / "export" / "pool.json").open(encoding="utf-8") as handle:
        row = json.load(handle)[0]
    assert row["impr"] == 21
    assert row["pos_y"] == 6
    assert row["quoted"] is None and row["exact"] is None

    with (tmp_path / "export" / "pool.csv").open(encoding="utf-8-sig", newline="") as handle:
        exported = next(csv.DictReader(handle, delimiter=";"))
    assert exported["impr"] == "21"
    assert exported["pos_y"] == "6"
    assert exported["exact"] == ""


def test_own_domain_detection_requires_a_hostname_boundary(sem_store):
    sem_store.add_serp(
        "synthetic phrase",
        [
            {"url": "https://example.invalid/a", "domain": "example.invalid", "pos": 1},
            {"url": "https://sub.example.invalid/a", "domain": "sub.example.invalid", "pos": 2},
            {
                "url": "https://example.invalid.other.invalid/a",
                "domain": "example.invalid.other.invalid",
                "pos": 3,
            },
        ],
    )
    sem_store.refresh_domains("example.invalid")
    kinds = {row["domain"]: row["kind"] for row in sem_store.db.execute("SELECT * FROM domains")}
    assert kinds == {
        "example.invalid": "own",
        "sub.example.invalid": "own",
        "example.invalid.other.invalid": "competitor",
    }


def test_stage_journal_and_landing_change_keep_utc_state_and_audit(sem_store):
    cluster_id = sem_store.upsert_cluster("industrial pumps", source="serp")
    run_id = sem_store.start_stage("clean", estimate={"request_estimate": {"count": 0}})
    sem_store.finish_stage(run_id, "done", result={"kept": 2})
    sem_store.set_landing(cluster_id, "https://example.test/pumps")
    sem_store.commit()

    state = sem_store.stage_states()
    assert state["latest"]["clean"]["state"] == "done"
    assert state["latest"]["clean"]["started_at"].endswith("Z")
    assert state["latest"]["clean"]["result"] == {"kept": 2}
    (audit,) = sem_store.db.execute(
        "SELECT action,before_json,after_json FROM semantic_audit"
    ).fetchall()
    assert audit["action"] == "set_landing"
    assert json.loads(audit["before_json"]) == {"landing_url": None}
    assert json.loads(audit["after_json"]) == {"landing_url": "https://example.test/pumps"}


def test_scoped_export_dry_run_counts_and_selects_only_requested_rows(sem_store, tmp_path):
    cluster_id = sem_store.upsert_cluster("industrial pumps")
    sem_store.upsert("industrial pumps", base=240)
    sem_store.assign_cluster("industrial pumps", cluster_id)
    sem_store.upsert("water pumps", base=80)
    sem_store.commit()

    plan = sem_store.export(tmp_path / "export", scope="cluster:industrial pumps", dry_run=True)
    assert plan["rows"] == 1
    assert len(plan["planned_files"]) == 2
    assert not (tmp_path / "export").exists()
    written = sem_store.export(tmp_path / "scoped", scope="cluster:industrial pumps")
    assert written == 1
    with (tmp_path / "scoped" / "pool.json").open(encoding="utf-8") as handle:
        assert [row["norm"] for row in json.load(handle)] == ["industrial pumps"]


def test_semantic_csv_export_neutralizes_formula_like_phrase_text(sem_store, tmp_path):
    sem_store.upsert("synthetic formula phrase")
    sem_store.set_fields("synthetic formula phrase", comment='=HYPERLINK("https://example.test/")')
    sem_store.commit()
    sem_store.export(tmp_path / "safe")

    with (tmp_path / "safe" / "pool.csv").open(encoding="utf-8-sig", newline="") as handle:
        row = next(csv.DictReader(handle, delimiter=";"))
    assert row["comment"].startswith("'=")
    with (tmp_path / "safe" / "pool.json").open(encoding="utf-8") as handle:
        assert json.load(handle)[0]["comment"].startswith("=")


def test_cluster_reads_are_paged_and_keep_missing_frequencies_null(sem_store, tmp_path):
    from types import SimpleNamespace

    from seohead.semantics.read_model import execute_read

    cluster_id = sem_store.upsert_cluster("industrial pumps")
    sem_store.upsert("industrial pumps", base=240)
    sem_store.assign_cluster("industrial pumps", cluster_id)
    sem_store.set_status("industrial pumps", "kept")
    sem_store.upsert("water pumps")
    sem_store.assign_cluster("water pumps", cluster_id)
    sem_store.set_status("water pumps", "kept")
    sem_store.commit()

    first = execute_read(
        sem_store,
        tmp_path,
        "cluster-queries",
        SimpleNamespace(cluster=str(cluster_id), offset=0, limit=1, json=False),
    )
    assert first["total"] == 2 and first["has_more"] is True
    second = execute_read(
        sem_store,
        tmp_path,
        "cluster-queries",
        SimpleNamespace(cluster=str(cluster_id), offset=1, limit=1, json=False),
    )
    assert second["items"][0]["base"] is None
    assert second["items"][0]["exact"] is None


def test_insights_keep_missing_source_metrics_null_with_reason(sem_store, tmp_path):
    from seohead.semantics.read_model import insights

    result = insights(sem_store, tmp_path, "2026-09-01..2026-09-07", ["gsc", "metrika", "webmaster"])
    assert result["provider_calls_made"] == 0
    assert all(item["state"] == "unavailable" for item in result["sources"].values())
