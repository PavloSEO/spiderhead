"""Synthetic, offline coverage for retained log imports and native-scan joins."""

from __future__ import annotations

import gzip
import hashlib
import sqlite3
from contextlib import closing
from types import SimpleNamespace

import pytest

from seohead import __version__
from seohead.checks import logstore
from seohead.projects.workspace import create_project


def _line(path="/", status=200, ip="192.0.2.1", when="11/Oct/2026:00:30:00 +0300", ua="Googlebot"):
    return f'{ip} - - [{when}] "GET {path} HTTP/1.1" {status} 10 "-" "{ua}"\n'


@pytest.fixture
def project(tmp_path):
    root = tmp_path / "project"
    create_project(str(root), target="https://example.test")
    return root


def _import(tmp_path, project, lines, **kwargs):
    source = tmp_path / "access.log"
    source.write_text("".join(lines), encoding="utf-8")
    result = logstore.import_logs(str(source), str(project), **kwargs)
    assert result["ok"], result
    return result


def _scan(tmp_path, *, limit=20, directives="noindex"):
    from seohead.crawl.settings import load
    from seohead.crawl.sqlite_adapter import crawl_to_scan
    from seohead.storage.native_scan import NativeScan

    pages = {
        "/": '<a href="/visited">Visited</a><a href="/never">Never</a><a href="/noindex">Noindex</a><a href="/unknown">Unknown</a>',
        "/visited": "<p>Visited</p>",
        "/never": "<p>Not requested in the synthetic log</p>",
        "/noindex": f'<meta name="robots" content="{directives}"><p>Excluded</p>',
    }

    def fetch(url):
        path = url.removeprefix("https://example.test")
        if path == "/robots.txt":
            content = b"User-agent: *\nAllow: /\n"
            kind = "text/plain"
        else:
            content = f"<!doctype html><html><head><title>Synthetic</title></head><body>{pages.get(path, '')}</body></html>".encode()
            kind = "text/html"
        return SimpleNamespace(
            content=content, text=content.decode(), status_code=200, headers={"content-type": kind}
        )

    settings = load(
        overrides={"speed.min_delay_seconds": 0, "limits.max_urls": limit, "resources.fetch": False}
    )
    scan = tmp_path / "scan.sqlite"
    crawl_to_scan(
        "https://example.test/",
        scan_out=str(scan),
        settings=settings,
        producer_version=__version__,
        producer_revision="a" * 40,
        runtime_versions={
            "python": "test",
            "sqlite": "test",
            "httpx": "test",
            "lxml": "test",
            "beautifulsoup4": "test",
        },
        fetcher=fetch,
        sleeper=lambda _: None,
    )
    with NativeScan.open(scan) as native:
        native.finish_capture("synthetic fixture complete")
    return scan


def test_import_is_separate_fresh_and_preserves_project_manifest(tmp_path, project):
    before = (project / "project.json").read_bytes()
    imported = _import(tmp_path, project, [_line("/a"), _line("/a", ip="192.0.2.2")])
    again = _import(tmp_path, project, [_line("/a")])
    assert imported["source"] != again["source"]
    assert (project / "project.json").read_bytes() == before
    assert imported["complete"] is True
    assert imported["period"] == {
        "from": "2026-10-10T21:30:00+00:00",
        "to": "2026-10-10T21:30:00+00:00",
    }
    with closing(logstore.open_log_source(imported["source"])) as con:
        rows = list(con.execute("SELECT * FROM bot_ips ORDER BY ip"))
        assert len(rows) == 2
        assert [r["visits"] for r in rows] == [1, 1]
        stored = con.execute("SELECT * FROM sources").fetchone()
        assert (
            stored["sha256"]
            == hashlib.sha256((_line("/a") + _line("/a", ip="192.0.2.2")).encode()).hexdigest()
        )
        with pytest.raises(sqlite3.OperationalError):
            con.execute("DELETE FROM bot_ips")


def test_summary_pages_are_bounded_exact_and_filter_date_subnet(tmp_path, project):
    imported = _import(
        tmp_path,
        project,
        [
            _line("/a"),
            _line("/a"),
            _line("/b", status=404, ip="192.0.2.2"),
            _line("/c", ip="198.51.100.1", when="12/Oct/2026:12:00:00 +0000"),
            _line("/human", ua="Mozilla/5.0"),
        ],
    )
    summary = logstore.log_summary(imported["source"], family="googlebot", limit=1)
    assert summary["total"] == 3
    assert len(summary["rows"]) == 1
    assert summary["rows"][0]["visits"] == 2
    assert summary["totals"] == {"visits": 4, "urls": 3, "ips": 3}
    assert summary["has_more"]
    filtered = logstore.log_summary(
        imported["source"],
        view="bot_ips",
        ip="192.0.2.0/24",
        date_from="2026-10-10",
        date_to="2026-10-10",
        limit=1,
        offset=1,
    )
    assert filtered["total"] == 2
    assert filtered["bot_ips"][0]["ip"] == "192.0.2.2"
    assert filtered["totals"]["visits"] == 3
    for view in ("bots", "daily", "statuses", "sections"):
        assert logstore.log_summary(imported["source"], view=view)["ok"]


def test_directory_gzip_and_iis_utc(tmp_path, project):
    directory = tmp_path / "logs"
    directory.mkdir()
    (directory / "access.1").write_text(_line())
    iis = "#Fields: date time c-ip cs-uri-stem cs-uri-query cs-method sc-status cs(User-Agent)\n2026-10-11 12:00:00 2001:db8::1 /b q=1 GET 301 bingbot\n"
    with gzip.open(directory / "renamed.archive", "wt") as stream:
        stream.write(iis)
    imported = logstore.import_logs(str(directory), str(project))
    assert imported["ok"] and imported["files"] == {"imported": 2, "selected": 2}
    summary = logstore.log_summary(imported["source"], family="bingbot", ip="2001:db8::/32")
    assert summary["rows"][0]["url"] == "/b?q=1"
    assert summary["rows"][0]["last_visit"] == "2026-10-11T12:00:00+00:00"


def test_limits_and_unavailable_evidence_are_explicit(tmp_path, project):
    imported = _import(tmp_path, project, [_line(), _line("/later")], max_lines=1)
    assert imported["lines"]["truncated"] and imported["complete"] is False
    assert logstore.log_summary(imported["source"])["totals"]["visits"] == 1
    malformed = _import(tmp_path, project, [_line(), "bad row\n"])
    assert malformed["lines"]["skipped"] == 1 and not malformed["complete"]
    common = _import(
        tmp_path, project, ['192.0.2.1 - - [11/Oct/2026:12:00:00 +0000] "GET / HTTP/1.1" 200 10\n']
    )
    assert common["complete"] and not common["bot_detection_complete"]
    assert logstore.log_summary(common["source"], view="bot_ips")["total"] == 0
    assert logstore.log_summary(common["source"], family="unknown")["totals"]["visits"] == 1
    assert (
        logstore.join_log_scan(imported["source"], str(tmp_path / "missing"))["available"] is False
    )
    for kwargs in (
        {"limit": 0},
        {"offset": -1},
        {"ip": "invalid"},
        {"date_from": "2026-10-12", "date_to": "2026-10-11"},
        {"view": "unknown"},
        {"date_from": "20261010"},
        {"limit": 501},
    ):
        assert not logstore.log_summary(imported["source"], **kwargs)["ok"]


def test_failed_import_does_not_publish_partial_artifact(tmp_path, project, monkeypatch):
    source = tmp_path / "bad.gz"
    source.write_bytes(b"\x1f\x8bcorrupt")
    assert not logstore.import_logs(str(source), str(project))["ok"]
    assert not list((project / "logs").glob("*.sqlite"))
    source.write_text(_line())
    monkeypatch.setattr(logstore.shutil, "disk_usage", lambda _: SimpleNamespace(free=1))
    assert not logstore.import_logs(str(source), str(project))["ok"]


def test_import_refuses_nonprojects_unsafe_sources_and_overlong_rows(tmp_path, project):
    source = tmp_path / "source.log"
    source.write_text(_line())
    assert not logstore.import_logs(str(source), str(tmp_path))["ok"]
    link = tmp_path / "link"
    link.symlink_to(source)
    assert not logstore.import_logs(str(link), str(project))["ok"]
    source.write_text(_line() + "x" * (logstore.MAX_LINE_CHARS + 1))
    assert not logstore.import_logs(str(source), str(project))["ok"]
    assert not logstore.import_logs(str(source), str(project), max_lines=0)["ok"]


def test_foreign_or_corrupt_log_artifacts_fail_closed(tmp_path):
    path = tmp_path / "foreign.sqlite"
    with sqlite3.connect(path) as con:
        con.execute("CREATE TABLE visits(url TEXT)")
    assert not logstore.log_summary(str(path))["ok"]
    with pytest.raises(ValueError, match="identity"):
        logstore.open_log_source(str(path))


def test_join_candidates_waste_nonindexable_and_no_scan_mutation(tmp_path, project):
    scan = _scan(tmp_path)
    before = hashlib.sha256(scan.read_bytes()).hexdigest()
    imported = _import(
        tmp_path,
        project,
        [
            _line("/"),
            _line("/visited"),
            _line("/noindex"),
            _line("/candidate", status=404),
            _line("/redirect", status=301),
            _line("/server", status=503),
            _line("/visited?x=1"),
            _line("https://other.test/never"),
        ],
    )
    missing = logstore.join_log_scan(imported["source"], str(scan), limit=1)
    assert missing["ok"], missing
    assert missing["total"] == 2 and len(missing["rows"]) == 1
    assert "visits" not in missing["rows"][0]
    candidates = logstore.join_log_scan(imported["source"], str(scan), view="bot_urls_absent_scan")
    assert candidates["total"] == 4
    assert {row["classification"] for row in candidates["rows"]} == {"discovery_candidate"}
    waste = logstore.join_log_scan(imported["source"], str(scan), view="status_waste")
    assert waste["total"] == 3
    noindex = logstore.join_log_scan(imported["source"], str(scan), view="nonindexable")
    assert noindex["total"] == 1 and noindex["rows"][0]["url"].endswith("/noindex")
    sections = logstore.join_log_scan(imported["source"], str(scan), view="sections")
    assert sections["ok"] and sum(row["visits"] for row in sections["rows"]) == 7
    assert noindex["indexability"]["unknown_urls"] == 0
    assert "canonical" in noindex["indexability"]["basis"]
    wrong_origin = logstore.join_log_scan(
        imported["source"], str(scan), base_url="https://other.test"
    )
    assert not wrong_origin["available"] and "origin differs" in wrong_origin["reason"]
    assert hashlib.sha256(scan.read_bytes()).hexdigest() == before


def test_join_does_not_infer_missing_from_partial_inputs(tmp_path, project):
    scan = _scan(tmp_path)
    imported = _import(tmp_path, project, [_line(), _line("/visited")], max_lines=1)
    for view in ("scan_without_bot_visits", "bot_urls_absent_scan", "sections"):
        result = logstore.join_log_scan(imported["source"], str(scan), view=view)
        assert result["available"] is False and "incomplete" in result["reason"]
        assert "total" not in result
    partial = _scan(tmp_path / "partial", limit=1)
    assert (
        logstore.join_log_scan(imported["source"], str(partial), view="status_waste")["available"]
        is False
    )


def test_log_artifact_triggers_and_schema_changes_are_rejected(tmp_path, project):
    imported = _import(tmp_path, project, [_line()])
    with sqlite3.connect(imported["source"]) as con:
        con.execute("CREATE TRIGGER tamper AFTER INSERT ON dns_cache BEGIN DELETE FROM visits; END")
    assert not logstore.log_summary(imported["source"])["ok"]


def test_none_directive_and_unmeasured_status_are_distinguished(tmp_path, project):
    scan = _scan(tmp_path, directives="none")
    imported = _import(tmp_path, project, [_line("/noindex")])
    excluded = logstore.join_log_scan(imported["source"], str(scan), view="nonindexable")
    assert excluded["ok"] and excluded["total"] == 1
    assert logstore._status_directives_indexable(None, "", "") is None
    assert logstore._status_directives_indexable(200, "", "Googlebot: NONE") == 0
    assert logstore._status_directives_indexable(200, "nofollow", "") == 1


def test_unknown_log_dates_block_date_filtered_absence_and_section_counts(tmp_path, project):
    scan = _scan(tmp_path)
    imported = _import(tmp_path, project, [_line(), _line("/visited", when="invalid")])
    assert imported["complete"] and imported["lines"]["unknown_time"] == 1
    for view in ("scan_without_bot_visits", "bot_urls_absent_scan", "sections"):
        result = logstore.join_log_scan(
            imported["source"], str(scan), view=view, date_from="2026-10-10"
        )
        assert not result["available"] and "undated" in result["reason"]
