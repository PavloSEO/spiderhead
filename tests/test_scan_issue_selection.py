"""Saved issue selection is streamed, read-only and honest about unmeasured checks."""

from __future__ import annotations

import sqlite3

import pytest

from seohead.storage import open_scan
from seohead.storage.url_query import scan_url_query
from tests.test_scan_hreflang_graph import BASE, _capture, _html, _Response


def test_issue_url_query_includes_group_members_and_is_read_only(tmp_path):
    import hashlib

    _capture(
        tmp_path,
        {
            BASE: _Response(
                _html(
                    extra_head="<title>Repeated synthetic title</title>",
                    body='<a href="/a/">a</a><a href="/b/">b</a>',
                )
            ),
            BASE + "a/": _Response(_html(extra_head="<title>Repeated synthetic title</title>")),
            BASE + "b/": _Response(_html(extra_head="<title>Repeated synthetic title</title>")),
        },
        max_urls=5,
    )
    scan = tmp_path / "scan.sqlite"
    before = hashlib.sha256(scan.read_bytes()).hexdigest()
    selected = scan_url_query(str(scan), issue_check="title-duplicate", issue_severity="warning")
    assert selected["ok"] and selected["filtered_total"] == 3
    assert {row["url"] for row in selected["rows"]} == {BASE, BASE + "a/", BASE + "b/"}
    assert hashlib.sha256(scan.read_bytes()).hexdigest() == before
    empty = scan_url_query(str(scan), issue_check="NOINDEX")
    assert empty["ok"] and empty["filtered_total"] == 0


def test_issue_url_query_streams_audit_v2_group_members(tmp_path, monkeypatch):
    from seohead.storage.audit_v2 import AuditV2Reader, write_audit_v2

    audit = _capture(
        tmp_path,
        {
            BASE: _Response(
                _html(extra_head="<title>Repeated</title>", body='<a href="/a/">a</a>')
            ),
            BASE + "a/": _Response(_html(extra_head="<title>Repeated</title>")),
        },
    )
    scan = tmp_path / "scan.sqlite"
    with open_scan(scan) as con:
        header = dict(con.execute("SELECT * FROM scan").fetchone())
    binding = {
        "scan_uuid": header["scan_uuid"],
        "evidence_revision": header["evidence_revision"],
        "analyzer_version": header["writer_version"],
        "analyzer_revision": header["writer_revision"],
    }
    write_audit_v2(
        scan,
        audit,
        {
            "/issues": iter(audit["issues"]),
            "/groups": iter(audit["groups"]),
            "/pages": iter(audit["pages"]),
        },
        binding,
    )
    monkeypatch.setattr(
        AuditV2Reader,
        "materialize_legacy",
        lambda *_args, **_kwargs: pytest.fail("query must stream the companion"),
    )
    selected = scan_url_query(str(scan), issue_check="TITLE_DUPLICATE")
    assert selected["ok"] and selected["filtered_total"] == 2
    assert {row["url"] for row in selected["rows"]} == {BASE, BASE + "a/"}


@pytest.mark.parametrize(
    "check,coverage,expected",
    [
        ("EXCLUDED", {"checks_disabled_ids": ["EXCLUDED"], "checks_silent_ids": []}, "disabled"),
        (
            "UNKNOWN",
            {"checks_disabled_ids": [], "checks_silent_ids": ["TITLE_DUPLICATE"]},
            "absent",
        ),
    ],
)
def test_disabled_and_absent_saved_checks_are_unmeasured(monkeypatch, check, coverage, expected):
    from seohead.storage import issue_selection

    monkeypatch.setattr(
        issue_selection,
        "resolve_audit_source",
        lambda _path: (
            {
                "run": {"checks_skipped": []},
                "summary": {"check_coverage": coverage},
                "issues": [],
                "groups": [],
            },
            [],
        ),
    )
    with sqlite3.connect(":memory:") as con:
        con.execute("CREATE TABLE urls(url_id INTEGER PRIMARY KEY,url TEXT)")
        with pytest.raises(issue_selection.UnmeasuredChecks, match=expected):
            issue_selection.materialize(con, "missing.sqlite", checks=check)


def test_streaming_issue_reader_deadline_is_explicit_unavailable(tmp_path, monkeypatch):
    from seohead.storage.audit_v2 import AuditV2Reader, write_audit_v2

    audit = _capture(tmp_path, {BASE: _Response(_html())})
    scan = tmp_path / "scan.sqlite"
    with open_scan(scan) as con:
        row = dict(con.execute("SELECT * FROM scan").fetchone())
    binding = {
        "scan_uuid": row["scan_uuid"],
        "evidence_revision": row["evidence_revision"],
        "analyzer_version": row["writer_version"],
        "analyzer_revision": row["writer_revision"],
    }
    write_audit_v2(
        scan, audit, {"/issues": iter(audit["issues"]), "/groups": iter(audit["groups"])}, binding
    )
    original = AuditV2Reader._validate

    def expire(reader, **kwargs):
        reader._read_deadline = 0
        original(reader, **kwargs)

    monkeypatch.setattr(AuditV2Reader, "_validate", expire)
    queried = scan_url_query(str(scan), issue_check="TITLE_MISSING")
    assert queried["ok"] and queried["state"] == "unavailable"
    assert queried["reason_code"] == "issue_selection_timeout"
    assert queried["filtered_total"] is None


def test_interrupted_skipped_check_stream_closes_reader_and_is_unavailable(tmp_path, monkeypatch):
    from types import SimpleNamespace

    from seohead.storage import audit_v2

    _capture(tmp_path, {BASE: _Response(_html())})
    closed = []

    class InterruptedReader:
        def __init__(self):
            self.header = {"run": {}, "summary": {}}
            self.collections = {"/run/checks_skipped"}

        def iter_collection(self, _pointer):
            raise sqlite3.OperationalError("interrupted")
            yield  # keep a lazy cursor interface

        def close(self):
            closed.append(True)

    monkeypatch.setattr(
        audit_v2, "audit_v2_path", lambda _path: SimpleNamespace(exists=lambda: True)
    )
    monkeypatch.setattr(audit_v2, "AuditV2Reader", lambda *_args, **_kwargs: InterruptedReader())
    queried = scan_url_query(str(tmp_path / "scan.sqlite"), issue_check="TITLE_MISSING")
    assert queried["ok"] and queried["state"] == "unavailable"
    assert queried["reason_code"] == "issue_selection_timeout"
    assert queried["filtered_total"] is None and closed == [True]
