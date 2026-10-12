"""Offline status summaries distinguish completed evidence from unfinished work."""

from __future__ import annotations

import hashlib
import json
import socket
import sqlite3

import httpx
import pytest

from seohead import cli
from seohead.storage import import_run
from seohead.storage.native_scan import NativeScan
from seohead.storage.status import scan_status
from tests.test_scan_artifact import BUILD
from tests.test_scan_artifact import legacy_run as legacy_run
from tests.test_scan_native import _metadata, _record, _runtime


def _commit(scan: NativeScan, status_code: int | None, *, error: str = "") -> None:
    lease = scan.claim(1)[0]
    record = _record(lease.url)
    record["crawl_depth"] = lease.depth
    record["status_code"] = status_code
    record["error"] = error
    record["error_kind"] = "transport" if error else ""
    scan.commit_page(lease, record, runtime=_runtime())


def _native_status_fixture(path) -> None:
    with NativeScan.create(path, **_metadata()) as scan:
        scan.enqueue(
            [
                ("https://example.test/ok", 0),
                ("https://example.test/missing", 0),
                ("https://example.test/unavailable", 0),
                ("https://example.test/transport", 0),
                ("https://example.test/excluded", 0),
                ("https://example.test/inflight", 0),
                ("https://example.test/queued", 0),
            ]
        )
        _commit(scan, 200)
        _commit(scan, 404)
        _commit(scan, 503)
        _commit(scan, None, error="connection reset")
        scan.exclude_lease(scan.claim(1)[0], "robots denied", runtime=_runtime())
        scan.claim(1)
        # URL identities outside `frontier` (such as declared resources) must not be work.
        scan.con.execute("INSERT INTO urls(url) VALUES(?)", ("https://cdn.example.test/app.js",))
        scan.interrupt("operator_cancelled")


def test_native_status_counts_disjoint_frontier_and_http_outcomes_without_network(
    tmp_path, monkeypatch
):
    path = tmp_path / "scan.sqlite"
    _native_status_fixture(path)
    before = hashlib.sha256(path.read_bytes()).hexdigest()

    def no_network(*_args, **_kwargs):
        raise AssertionError("scan-status must stay offline")

    monkeypatch.setattr(socket, "getaddrinfo", no_network)
    monkeypatch.setattr(socket, "create_connection", no_network)
    monkeypatch.setattr(httpx, "Client", no_network)

    result = scan_status(str(path))

    assert result["source"] == {
        "scan_uuid": result["source"]["scan_uuid"],
        "format_version": "scan.v1",
        "source_kind": "native",
        "parent_scan_uuid": None,
        "writer_version": result["source"]["writer_version"],
        "writer_revision": "a" * 40,
        "evidence_revision": 4,
        "created_at": result["source"]["created_at"],
        "finished_at": None,
        "lifecycle": "interrupted",
        "finish_reason": "operator_cancelled",
        "crawl_partial": True,
        "corpus_partial": True,
    }
    assert result["frontier"] == {
        "state": "available",
        "reason": "",
        "counts": {"queued": 1, "inflight": 1, "done": 4, "excluded": 1},
    }
    assert result["committed_page_outcomes"] == {
        "2xx": 1,
        "3xx": 0,
        "4xx": 1,
        "5xx": 1,
        "other": 0,
        "no_response": 1,
    }
    assert result["integrity"]["state"] == "not_checked"
    assert result["integrity"]["checked_at"] is None
    assert result["checkpoint_available"] is True
    assert hashlib.sha256(path.read_bytes()).hexdigest() == before


def test_legacy_import_reports_observed_pages_but_no_invented_frontier(legacy_run, tmp_path):
    path = import_run(legacy_run, tmp_path / "legacy.sqlite", producer_build=BUILD)
    before = hashlib.sha256(path.read_bytes()).hexdigest()

    result = scan_status(str(path))

    assert result["source"]["source_kind"] == "legacy_import"
    assert result["source"]["lifecycle"] == "finished"
    assert result["source"]["finish_reason"] == "legacy_import"
    assert result["frontier"] == {
        "state": "unavailable",
        "reason": "legacy import retains no native frontier",
        "counts": None,
    }
    assert sum(result["committed_page_outcomes"].values()) == 2
    assert hashlib.sha256(path.read_bytes()).hexdigest() == before


def test_cli_and_mcp_return_the_same_status_summary(tmp_path, capsys):
    path = tmp_path / "scan.sqlite"
    _native_status_fixture(path)

    assert cli.main(["scan-status", "--input", str(path)]) == 0
    command_result = json.loads(capsys.readouterr().out)
    from seohead.mcp.mcp_server import build_server

    tool = build_server()._tool_manager.get_tool("seo_scan_status")
    assert tool.fn(input_path=str(path)) == command_result


@pytest.mark.parametrize(
    ("mutation", "message"),
    [
        ("PRAGMA user_version=999", "user_version"),
        (
            "PRAGMA ignore_check_constraints=ON; UPDATE scan SET source_kind='unknown'",
            "cannot read scan",
        ),
    ],
)
def test_status_refuses_unknown_or_unsupported_artifacts_without_rewriting(
    tmp_path, mutation, message
):
    path = tmp_path / "invalid.sqlite"
    _native_status_fixture(path)
    with sqlite3.connect(path) as con:
        for statement in mutation.split("; "):
            con.execute(statement)
    before = hashlib.sha256(path.read_bytes()).hexdigest()

    result = scan_status(str(path), full_validation=True)
    assert result["integrity"]["state"] == "damaged"
    assert result["integrity"]["checked_at"]
    assert result["integrity"]["affected_scan_ids"]
    assert message in result["integrity"]["reason"]
    assert result["ok"] is False

    assert hashlib.sha256(path.read_bytes()).hexdigest() == before


def test_status_rejects_missing_artifact(tmp_path):
    result = scan_status(str(tmp_path / "missing.sqlite"))
    assert result["integrity"] == {
        "state": "not_checked",
        "checked_at": None,
        "damaged_pages": None,
        "affected_scan_ids": [],
        "reason": "missing_file",
    }
    assert result["checkpoint_available"] is None
