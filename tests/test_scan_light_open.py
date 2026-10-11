"""Issue #931: reading a scan accepts it lightly; full validation is explicit or cached."""

from __future__ import annotations

import sqlite3

import pytest

from seohead.mcp import handlers
from seohead.storage import ScanError, import_run, open_scan_mode
from seohead.storage.native_scan import NativeScan
from seohead.storage.status import scan_status
from tests.test_scan_artifact import BUILD
from tests.test_scan_artifact import legacy_run as legacy_run
from tests.test_scan_status import _native_status_fixture


@pytest.fixture
def counters(monkeypatch):
    calls = {"full": 0, "light": 0}
    full, light = NativeScan._validate_native, NativeScan._validate_light

    def count_full(con):
        calls["full"] += 1
        return full(con)

    def count_light(con):
        calls["light"] += 1
        return light(con)

    monkeypatch.setattr(NativeScan, "_validate_native", staticmethod(count_full))
    monkeypatch.setattr(NativeScan, "_validate_light", staticmethod(count_light))
    return calls


def _scan(tmp_path, counters=None):
    path = tmp_path / "scan.sqlite"
    _native_status_fixture(path)
    if counters is not None:  # building the fixture validates too; count only the reads
        counters.update(full=0, light=0)
    return path


def test_status_is_light_by_default_and_counts_match_full(tmp_path, counters):
    path = _scan(tmp_path, counters)

    light = scan_status(str(path))
    assert (counters["full"], counters["light"]) == (0, 1)
    full = scan_status(str(path), full_validation=True)
    assert counters["full"] == 1

    assert (light["validation"], full["validation"]) == ("light", "full")
    assert {
        k: v for k, v in light.items() if k not in {"validation", "integrity"}
    } == {k: v for k, v in full.items() if k not in {"validation", "integrity"}}
    assert light["integrity"]["state"] == "not_checked"
    assert full["integrity"]["state"] == "ok"
    assert light["committed_page_outcomes"]["2xx"] == 1


def test_full_result_is_reused_for_unchanged_bytes_and_dropped_on_change(tmp_path, counters):
    path = _scan(tmp_path, counters)
    scan_status(str(path), full_validation=True)

    assert scan_status(str(path))["validation"] == "full"
    assert counters["light"] == 0

    with sqlite3.connect(path) as con:  # any byte change invalidates the cached result
        con.execute("UPDATE scan SET pinned=1")
    assert scan_status(str(path))["validation"] == "light"
    assert counters["light"] == 1


@pytest.mark.parametrize(
    "mutation",
    [
        "DROP INDEX pages_status",
        "UPDATE scan SET config_fingerprint='0000000000000000'",
        "DELETE FROM resume_state",
        "PRAGMA ignore_check_constraints=ON; UPDATE scan SET lifecycle='bogus'",
    ],
)
def test_light_open_still_rejects_schema_and_header_damage(tmp_path, mutation):
    path = _scan(tmp_path)
    with sqlite3.connect(path) as con:
        for statement in mutation.split("; "):
            con.execute(statement)

    with pytest.raises(ScanError):
        open_scan_mode(path, require_audit=False, light=True)


def test_light_open_rejects_non_scan_and_foreign_files(tmp_path):
    garbage = tmp_path / "garbage.sqlite"
    garbage.write_bytes(b"not a database" * 100)
    foreign = tmp_path / "foreign.sqlite"
    with sqlite3.connect(foreign) as con:
        con.execute("CREATE TABLE t(x)")

    for path in (garbage, foreign, tmp_path / "missing.sqlite"):
        with pytest.raises(ScanError):
            open_scan_mode(path, require_audit=False, light=True)


def test_legacy_import_is_not_light_eligible(legacy_run, tmp_path):
    path = import_run(legacy_run, tmp_path / "legacy.sqlite", producer_build=BUILD)

    con, mode = open_scan_mode(path, require_audit=False, light=True)
    con.close()

    assert mode == "full"


def test_url_detail_and_link_inspect_report_light_validation(tmp_path, counters):
    path = _scan(tmp_path, counters)

    detail = handlers.scan_url_detail(str(path), "https://example.test/ok")
    inlinks = handlers.scan_link_inspect(
        str(path), view="inlinks", target="https://example.test/ok"
    )

    assert detail["ok"] and detail["state"] == "available"
    assert detail["validation"] == "light"
    assert detail["responses"]["redirect_target_lookup"] == {"state": "complete"}
    assert inlinks["ok"] and inlinks["validation"] == "light"
    assert counters["full"] == 0


def test_project_observer_evidence_reads_scan_lightly(tmp_path, counters):
    from seohead.projects import observer

    path = _scan(tmp_path, counters)

    evidence = observer._read_scan_evidence(
        {"path": str(path), "source_kind": "native", "lifecycle": "interrupted"}
    )

    assert evidence["state"] == "available"
    assert evidence["validation"] == "light"
    assert evidence["committed_page_outcomes"]["2xx"] == 1
    assert counters["full"] == 0
