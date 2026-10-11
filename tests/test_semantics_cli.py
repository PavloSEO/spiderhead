"""CLI, configuration and filters of the semantic core; everything stays offline."""

from __future__ import annotations

import json
import shutil
import sqlite3
from pathlib import Path

import pytest

from seohead.cli import main as seohead_main
from seohead.data_sources import spend
from seohead.semantics import config
from seohead.semantics.cli import main
from seohead.semantics.norm import Filters, normalize
from seohead.semantics.runner import run_quiet
from seohead.semantics.store import Store

pytestmark = pytest.mark.usefixtures("semantics_offline")
EXAMPLE = Path(__file__).resolve().parents[1] / "docs" / "examples" / "semantics" / "seohead-tech"


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        ("  Ёлка---аудит!!! ", "елка аудит"),
        ("SEO-аудит/2026", "seo аудит 2026"),
        (None, ""),
        ("краулер\n\tдля   сайта", "краулер для сайта"),
    ],
)
def test_normalize_produces_a_stable_deduplication_key(raw, expected):
    assert normalize(raw) == expected


def test_presets_and_explicit_patterns():
    seo = Filters(preset="seo")
    assert seo.classify("краулер сайта", 100, 20) == ("kept", "commercial")
    assert seo.classify("как провести seo аудит", 40, 20) == ("kept", "info")
    assert seo.classify("seo курсы обучение", 100, 20)[0] == "dropped"
    assert seo.classify("ремонт велосипеда", 100, 20)[0] == "review"
    generic = Filters(preset="generic", stop=r"forbidden", info=r"guide")
    assert generic.classify("arbitrary commercial phrase", 100, 20) == ("kept", "commercial")
    assert generic.classify("guide to arbitrary phrase", 100, 20) == ("kept", "info")
    assert generic.classify("forbidden arbitrary phrase", 100, 20)[0] == "dropped"
    with pytest.raises(ValueError, match="unknown preset"):
        Filters(preset="missing")


def test_workspace_project_keeps_its_core_in_semantics(tmp_path):
    workspace = tmp_path / "site"
    workspace.mkdir()
    (workspace / "project.json").write_text("{}", encoding="utf-8")
    assert config.data_dir(workspace) == workspace / "semantics"
    config.ensure_project(workspace)
    assert config.db_path(workspace) == workspace / "semantics" / "sya.db"


def test_config_preserves_quoted_hash_regex_and_custom_database(tmp_path):
    directory = config.ensure_project(tmp_path / "demo")
    (directory / "project.yaml").write_text(
        'intent: "alpha#beta" # comment\nregions: [1, 2]\nany_intent: true\ndb: core.db\n',
        encoding="utf-8",
    )
    cfg = config.load(directory)
    assert cfg["intent"] == "alpha#beta"
    assert cfg["regions"] == ["1", "2"]
    assert cfg["any_intent"] is True
    assert config.db_path(directory) == directory / "core.db"


def test_status_is_readonly_and_missing_project_is_not_created(tmp_path, capsys):
    project = tmp_path / "demo"
    assert seohead_main(["semantics", "init", "--project", str(project)]) == 0
    capsys.readouterr()
    database = project / "sya.db"
    with sqlite3.connect(database) as db:
        db.execute("PRAGMA journal_mode=DELETE")
        db.execute("INSERT INTO phrases(norm,status) VALUES('retained phrase','parked')")
    before = database.read_bytes()
    assert main(["status", "--project", str(project), "--json"]) == 0
    current = json.loads(capsys.readouterr().out)
    assert current["counts"]["parked"] == 1
    assert database.read_bytes() == before

    missing = tmp_path / "missing"
    assert main(["status", "--project", str(missing)]) == 1
    assert "database not found" in capsys.readouterr().err
    assert not missing.exists()


def test_negative_limits_fail_before_creating_paths(tmp_path):
    with pytest.raises(SystemExit) as error:
        main(["collect", "--project", str(tmp_path / "missing"), "--max-seeds", "-1"])
    assert error.value.code == 2
    assert not (tmp_path / "missing").exists()


def test_import_preserves_existing_frequencies_and_rolls_back_bad_files(tmp_path):
    project = tmp_path / "demo"
    main(["init", "--project", str(project)])
    with Store(project / "sya.db") as store:
        store.upsert("synthetic phrase", base=99)
        store.set_fields("synthetic phrase", exact=0)
    source = tmp_path / "input.csv"
    source.write_text(
        "norm;base;quoted;exact;pos_g\nsynthetic phrase;1;7;5;3\nunmeasured phrase;;;0;\n",
        encoding="utf-8",
    )
    assert main(["import", "--project", str(project), "--file", str(source)]) == 0
    with Store(project / "sya.db", readonly=True) as store:
        rows = {row["norm"]: row for row in store.phrases()}
    assert (rows["synthetic phrase"]["base"], rows["synthetic phrase"]["exact"]) == (99, 0)
    assert rows["synthetic phrase"]["status"] == "kept"
    assert json.loads((project / "pool.json").read_text(encoding="utf-8"))

    bad = tmp_path / "bad.csv"
    bad.write_text("norm;base\ngood phrase;2\nbad phrase;not-a-number\n", encoding="utf-8")
    assert main(["import", "--project", str(project), "--file", str(bad)]) == 1
    with Store(project / "sya.db", readonly=True) as store:
        assert "good phrase" not in {row["norm"] for row in store.phrases()}


def test_example_project_runs_every_free_stage(tmp_path):
    project = tmp_path / "seohead-tech"
    shutil.copytree(EXAMPLE, project)
    for argv in (
        ["init"],
        ["import", "--file", str(project / "phrases.csv")],
        ["clean"],
        ["graph"],
        ["sitematch"],
        ["report"],
        ["excel"],
        ["export"],
    ):
        assert main([argv[0], "--project", str(project), *argv[1:]]) == 0, argv
    with Store(project / "sya.db", readonly=True) as store:
        statuses = {row["norm"]: row["status"] for row in store.phrases()}
    assert len(statuses) == 41
    assert statuses["seo аудит сайта"] == "kept"
    assert statuses["seo курсы обучение"] == "dropped"
    assert statuses["ремонт велосипеда"] == "review"
    assert (project / "report.md").is_file()
    assert (project / "seohead-tech_semantic_core.xlsx").is_file()


def test_mcp_runner_refuses_paid_stages_without_confirmation(tmp_path, monkeypatch):
    from seohead.semantics.stages import collect

    project = tmp_path / "demo"
    assert run_quiet("init", project, allowed=("init",))["ok"]
    (project / "seeds.txt").write_text("seo аудит сайта\n", encoding="utf-8")

    def forbidden(**_kwargs):
        raise AssertionError("an unconfirmed paid stage must not construct a provider")

    monkeypatch.setattr(collect, "Wordstat", forbidden)
    refused = run_quiet("collect", project, allowed=("collect",))
    assert refused["ok"] is True
    assert refused["result"]["requires_confirmation"] is True
    assert refused["result"]["estimate"]["paid_calls_made"] == 0
    assert run_quiet("collect", project, allowed=("clean",))["ok"] is False


def test_louvain_paid_preflight_counts_uncached_serps_and_never_constructs_provider(
    tmp_path, monkeypatch
):
    from seohead.semantics.stages import cluster

    project = tmp_path / "demo"
    run_quiet("init", project, allowed=("init",))
    with Store(project / "sya.db") as store:
        store.upsert("synthetic phrase", base=20)
        store.set_status("synthetic phrase", "kept")

    def forbidden(**_kwargs):
        raise AssertionError("an unconfirmed cluster plan must not construct the provider")

    monkeypatch.setattr(cluster, "WebSearch", forbidden)
    result = run_quiet("cluster", project, allowed=("cluster",), method="louvain")
    assert result["ok"] is True
    assert result["result"]["requires_confirmation"] is True
    assert result["result"]["estimate"]["request_estimate"]["count"] == 1
    assert result["result"]["estimate"]["paid_calls_made"] == 0


def test_collect_resume_estimate_includes_retryable_and_high_base_frontier(tmp_path):
    from seohead.semantics.runner import paid_estimate

    project = tmp_path / "demo"
    run_quiet("init", project, allowed=("init",))
    (project / "seeds.txt").write_text("seed phrase\n", encoding="utf-8")
    with Store(project / "sya.db") as store:
        store.upsert("frontier phrase", base=200)
        store.set_status("frontier phrase", "kept")
        store.db.execute(
            "CREATE TABLE collect_attempts(seed TEXT PRIMARY KEY,depth INTEGER,state TEXT,error TEXT)"
        )
        store.db.execute("INSERT INTO collect_attempts VALUES('retry phrase',1,'retryable',NULL)")
        store.commit()

    estimate = paid_estimate("collect", project, resume=True, max_phrases=10)
    assert estimate["request_estimate"]["known_initial_requests"] == 3
    assert estimate["request_estimate"]["maximum"] == 10


def test_exact_estimate_uses_the_selected_region_receipts(tmp_path):
    from seohead.semantics.runner import paid_estimate

    project = tmp_path / "demo"
    run_quiet("init", project, allowed=("init",))
    with Store(project / "sya.db") as store:
        store.upsert("synthetic phrase", base=20)
        store.set_status("synthetic phrase", "kept")
        store.db.execute(
            "CREATE TABLE provider_tasks(id INTEGER PRIMARY KEY,provider TEXT,stage TEXT,region INTEGER,payload_json TEXT,state TEXT)"
        )
        payload = json.dumps({"queries": ["synthetic phrase"]})
        store.db.execute(
            "INSERT INTO provider_tasks(provider,stage,region,payload_json,state) VALUES('arsenkin','exact',225,?,'pending')",
            (payload,),
        )
        store.commit()
    assert paid_estimate("exact", project)["item_estimate"] == 0
    (project / "project.yaml").write_text("regions: [213]\n", encoding="utf-8")
    assert paid_estimate("exact", project)["item_estimate"] == 1


def test_paid_stage_charges_are_tagged_in_the_shared_journal(tmp_path, monkeypatch):
    from seohead.semantics.stages import collect

    project = tmp_path / "demo"
    run_quiet("init", project, allowed=("init",))
    (project / "seeds.txt").write_text("seo аудит сайта\n", encoding="utf-8")

    class Wordstat:
        def __init__(self, **_kwargs):
            pass

        def expand(self, phrase, *, limit, regions):
            spend.record("yandex_cloud", "wordstat.top", cost=1, unit="requests")
            return {"seo аудит сайта онлайн": 90}, {"results": 1, "associations": 0}

    monkeypatch.setattr(collect, "Wordstat", Wordstat)
    result = run_quiet("collect", project, allowed=("collect",), confirm_paid=True)
    assert result["ok"], result
    (entry,) = spend.read_all()
    assert entry["extra"] == {"semantics_project": str(project), "semantics_stage": "collect"}
    assert result["result"]["spend"]["yandex_requests"] == 1


def test_import_preview_applies_mapping_and_counts_duplicates_without_writes(sem_store, tmp_path):
    from seohead.semantics.stages.import_phrases import plan

    fixture = Path(__file__).parent / "fixtures" / "semantics" / "import-mapped.csv"
    sem_store.upsert("industrial pumps", base=10)
    sem_store.commit()
    before = sem_store.db.execute("SELECT COUNT(*) FROM phrases").fetchone()[0]
    preview = plan(
        sem_store,
        config.load(tmp_path),
        fixture,
        mapping={
            "query": "Keyword",
            "frequency": "Searches",
            "group": "Category",
            "url": "URL",
            "comment": "Comment",
        },
        merge="update",
    )
    assert preview["applied_mapping"]["query"] == "Keyword"
    assert preview["unique_queries"] == 2
    assert preview["duplicates_in_input"] == 1
    assert preview["already_in_core"] == 1
    assert preview["preview"][0]["values"]["group_name"] == "Pumps"
    assert preview["preview"][0]["values"]["subgroup"] == "Vertical"
    assert sem_store.db.execute("SELECT COUNT(*) FROM phrases").fetchone()[0] == before


def test_import_replace_rejects_empty_input_without_clearing_core(sem_store, tmp_path):
    from seohead.semantics.stages.import_phrases import run

    sem_store.upsert("retained phrase", base=42)
    sem_store.commit()
    source = tmp_path / "empty.csv"
    source.write_text("query;base\n", encoding="utf-8")
    with pytest.raises(ValueError, match="refusing to clear"):
        run(sem_store, config.load(tmp_path), source, merge="replace")
    assert (
        sem_store.db.execute("SELECT base FROM phrases WHERE norm='retained phrase'").fetchone()[0]
        == 42
    )


def test_import_replace_clears_derived_rows_but_preserves_paid_receipts(
    sem_store, tmp_path, monkeypatch
):
    from seohead.semantics.stages.import_phrases import run

    sem_store.upsert("industrial pumps", base=20)
    cluster_id = sem_store.upsert_cluster("old cluster", source="serp")
    sem_store.assign_cluster("industrial pumps", cluster_id)
    sem_store.add_serp(
        "industrial pumps",
        [{"url": "https://example.test/old", "domain": "example.test", "pos": 1}],
    )
    sem_store.db.execute(
        "INSERT INTO domains(domain,n_queries,avg_pos,best_pos,kind) VALUES('example.test',1,1,1,'own')"
    )
    sem_store.db.execute(
        "INSERT INTO landing_candidates(cluster_id,url,score,evidence_json,computed_at) VALUES(?,?,?,?,?)",
        (cluster_id, "https://example.test/old", 1.0, "{}", "2026-09-01T00:00:00Z"),
    )
    sem_store.db.execute(
        "CREATE TABLE serp_fetches(phrase_norm TEXT,region TEXT,top INTEGER,state TEXT,operation_id TEXT,raw_json TEXT,error TEXT,fetched_at TEXT,PRIMARY KEY(phrase_norm,region,top))"
    )
    sem_store.db.execute(
        "INSERT INTO serp_fetches VALUES(?,?,?,?,?,?,?,?)",
        (
            "industrial pumps",
            "225",
            10,
            "complete",
            None,
            '{"docs":[{"url":"https://example.test/pumps","domain":"example.test","pos":1}]}',
            None,
            "2026-09-01T00:00:00Z",
        ),
    )
    sem_store.db.execute(
        "INSERT INTO expansions(seed_norm,expanded_at,results_n,assoc_n,region) VALUES(?,?,?,?,?)",
        ("industrial pumps", "2026-09-01", 1, 0, "225"),
    )
    sem_store.commit()
    source = tmp_path / "replace.csv"
    source.write_text("query;base\nindustrial pumps;20\n", encoding="utf-8")

    run(sem_store, config.load(tmp_path), source, merge="replace")

    assert sem_store.db.execute("SELECT COUNT(*) FROM clusters").fetchone()[0] == 0
    assert sem_store.db.execute("SELECT COUNT(*) FROM domains").fetchone()[0] == 0
    assert sem_store.db.execute("SELECT COUNT(*) FROM landing_candidates").fetchone()[0] == 0
    assert sem_store.db.execute("SELECT COUNT(*) FROM serp").fetchone()[0] == 0
    assert sem_store.db.execute("SELECT COUNT(*) FROM serp_fetches").fetchone()[0] == 1
    assert sem_store.db.execute("SELECT COUNT(*) FROM expansions").fetchone()[0] == 1
    sem_store.set_status("industrial pumps", "kept")

    from seohead.semantics.stages import cluster

    def forbidden(**_kwargs):
        raise AssertionError("a retained SERP receipt must be reused without a provider call")

    monkeypatch.setattr(cluster, "WebSearch", forbidden)
    cluster.run(sem_store, config.load(tmp_path))
    assert (
        sem_store.db.execute(
            "SELECT url FROM serp WHERE phrase_norm='industrial pumps'"
        ).fetchone()[0]
        == "https://example.test/pumps"
    )


def test_import_table_caps_refuse_before_replace_mutation(sem_store, tmp_path, monkeypatch):
    from seohead.semantics.stages import import_phrases

    source = tmp_path / "bounded.csv"
    source.write_text("query\nphrase one\nphrase two\n", encoding="utf-8")
    monkeypatch.setattr(import_phrases, "MAX_IMPORT_ROWS", 1)
    with pytest.raises(ValueError, match="row limit"):
        import_phrases.run(sem_store, config.load(tmp_path), source, merge="replace")
    assert sem_store.db.execute("SELECT COUNT(*) FROM phrases").fetchone()[0] == 0

    monkeypatch.setattr(import_phrases, "MAX_IMPORT_ROWS", 100_000)
    monkeypatch.setattr(import_phrases, "MAX_IMPORT_CELL_BYTES", 4)
    source.write_text("q\nlong phrase\n", encoding="utf-8")
    with pytest.raises(ValueError, match="cell exceeds"):
        import_phrases.run(
            sem_store, config.load(tmp_path), source, mapping={"query": "q"}, merge="replace"
        )
    assert sem_store.db.execute("SELECT COUNT(*) FROM phrases").fetchone()[0] == 0

    monkeypatch.setattr(import_phrases, "MAX_IMPORT_CELL_BYTES", 16_384)
    monkeypatch.setattr(import_phrases, "MAX_IMPORT_COLUMNS", 1)
    source.write_text("query;base\nphrase;10\n", encoding="utf-8")
    with pytest.raises(ValueError, match="column"):
        import_phrases.run(sem_store, config.load(tmp_path), source, merge="replace")
    assert sem_store.db.execute("SELECT COUNT(*) FROM phrases").fetchone()[0] == 0

    monkeypatch.setattr(import_phrases, "MAX_IMPORT_COLUMNS", 128)
    monkeypatch.setattr(import_phrases, "MAX_IMPORT_BYTES", 4)
    with pytest.raises(ValueError, match="file exceeds"):
        import_phrases.run(sem_store, config.load(tmp_path), source, merge="replace")
    assert sem_store.db.execute("SELECT COUNT(*) FROM phrases").fetchone()[0] == 0


def test_import_write_parses_the_source_once(sem_store, tmp_path, monkeypatch):
    from seohead.semantics.stages import import_phrases

    source = tmp_path / "once.csv"
    source.write_text("query\nsynthetic phrase\n", encoding="utf-8")
    original = import_phrases._parse
    calls = 0

    def counted(*args, **kwargs):
        nonlocal calls
        calls += 1
        return original(*args, **kwargs)

    monkeypatch.setattr(import_phrases, "_parse", counted)
    import_phrases.run(sem_store, config.load(tmp_path), source, merge="replace")
    assert calls == 1


def test_xlsx_import_preview_selects_requested_sheet(sem_store, tmp_path):
    from openpyxl import Workbook

    from seohead.semantics.stages.import_phrases import plan

    path = tmp_path / "mapped.xlsx"
    workbook = Workbook()
    default = workbook.active
    default.title = "Instructions"
    default.append(["not a query"])
    sheet = workbook.create_sheet("Queries")
    sheet.append(["Keyword", "Searches"])
    sheet.append(["industrial pumps", 240])
    workbook.save(path)
    preview = plan(
        sem_store,
        config.load(tmp_path),
        path,
        sheet="Queries",
        mapping={"query": "Keyword", "frequency": "Searches"},
    )
    assert preview["sheet"] == "Queries"
    assert preview["unique_queries"] == 1
    assert preview["preview"][0]["values"]["base"] == 240


def test_selected_insight_task_preview_and_revision_guard(tmp_path):
    from seohead.projects.coverage import coverage_status, initialize_coverage
    from seohead.projects.workspace import create_project
    from seohead.semantics.read_model import create_insight_tasks
    from seohead.semantics.store import Store

    project = tmp_path / "shop"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    with Store(project / "semantics" / "sya.db") as store:
        cluster_id = store.upsert_cluster("industrial pumps")
        store.upsert("industrial pumps", base=240)
        store.assign_cluster("industrial pumps", cluster_id)
        store.set_status("industrial pumps", "kept")
    revision = coverage_status(project)["revision"]
    selection = [{"rule_id": "landing.unassigned", "cluster_id": cluster_id}]
    preview = create_insight_tasks(
        project,
        "2026-09-01..2026-09-07",
        ["gsc"],
        selection,
        dry_run=True,
    )
    assert preview["written"] == 0
    assert preview["expected_revision"] == revision
    assert coverage_status(project)["revision"] == revision

    with pytest.raises(ValueError, match="revision changed"):
        create_insight_tasks(
            project,
            "2026-09-01..2026-09-07",
            ["gsc"],
            selection,
            expected_revision=revision - 1,
            confirm=True,
        )
    result = create_insight_tasks(
        project,
        "2026-09-01..2026-09-07",
        ["gsc"],
        selection,
        expected_revision=revision,
        confirm=True,
    )
    assert result["written"] == 1 and result["executed"] == 0
