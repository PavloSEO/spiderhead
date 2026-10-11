"""Local provider database: incremental sync, no duplicates, status gaps, and exports."""

from __future__ import annotations

import csv
import io
import json
import os
import sqlite3
from datetime import datetime, timedelta
from zoneinfo import ZoneInfo

import pytest

from seohead.cli import main as cli_main
from seohead.data_sources import ga4, metrika, sources_db, yandex_webmaster
from seohead.mcp import handlers


def _gsc_rows(resource, start, end):
    days = sources_db._days(start, end)
    return {
        "ok": True,
        "complete": True,
        "rows": [
            {"date": day, "query": "pump", "page": "https://example.test/", "clicks": 1}
            for day in days
        ],
    }


def _recorder(fetch):
    calls = []

    def wrapped(resource, start, end):
        calls.append((start, end))
        return fetch(resource, start, end)

    return calls, wrapped


def test_sync_fetches_only_missing_days_and_never_duplicates(tmp_path):
    db = tmp_path / "sources.sqlite"
    calls, fetch = _recorder(_gsc_rows)
    first = sources_db.sync(
        db,
        "gsc",
        "sc-domain:example.test",
        start_date="2026-01-01",
        end_date="2026-01-03",
        fetchers={"gsc": fetch},
    )
    assert first["synced_days"] == 3 and first["rows_stored"] == 3
    assert calls == [(d, d) for d in ("2026-01-01", "2026-01-02", "2026-01-03")]

    calls.clear()
    second = sources_db.sync(
        db,
        "gsc",
        "sc-domain:example.test",
        start_date="2026-01-01",
        end_date="2026-01-05",
        fetchers={"gsc": fetch},
    )
    assert calls == [("2026-01-04", "2026-01-04"), ("2026-01-05", "2026-01-05")]
    assert second["already_synced_days"] == 3

    sources_db.sync(
        db,
        "gsc",
        "sc-domain:example.test",
        start_date="2026-01-01",
        end_date="2026-01-05",
        force=True,
        fetchers={"gsc": fetch},
    )
    with sqlite3.connect(db) as connection:
        assert connection.execute("SELECT COUNT(*) FROM gsc").fetchone()[0] == 5


def test_sync_respects_source_lag(tmp_path):
    today = datetime.now(ZoneInfo("America/Los_Angeles")).date()
    calls, fetch = _recorder(_gsc_rows)
    result = sources_db.sync(
        tmp_path / "s.sqlite",
        "gsc",
        "sc-domain:example.test",
        start_date=(today - timedelta(days=4)).isoformat(),
        end_date=today.isoformat(),
        fetchers={"gsc": fetch},
    )
    assert result["requested_period"][1] == today.isoformat()
    assert result["available_period"][1] == (today - timedelta(days=3)).isoformat()
    assert len(result["lagged_days"]) == 3
    assert len(calls) == 2
    entry = sources_db.status(tmp_path / "s.sqlite")["sources"][0]
    assert entry["coverage"]["lagged"] == result["lagged_days"]


def test_multi_day_sources_are_fetched_in_contiguous_chunks(tmp_path):
    def metrika_rows(resource, start, end):
        return {
            "ok": True,
            "complete": True,
            "rows": [
                {
                    "date": day,
                    "traffic_source": "direct",
                    "landing_page": "/",
                    "visits": 1,
                    "users": 1,
                    "bounce_rate": 0,
                }
                for day in sources_db._days(start, end)
            ],
        }

    calls, fetch = _recorder(metrika_rows)
    sources_db.sync(
        tmp_path / "s.sqlite",
        "metrika",
        "123",
        start_date="2026-01-01",
        end_date="2026-03-01",
        fetchers={"metrika": fetch},
    )
    assert calls == [
        ("2026-01-01", "2026-01-31"),
        ("2026-02-01", "2026-03-01"),
    ]


def test_failed_fetch_stops_without_marking_the_period(tmp_path, monkeypatch):
    monkeypatch.setattr(sources_db, "RETRY_PAUSE", 0)
    attempts = []

    def failing(resource, start, end):
        attempts.append(start)
        return {"ok": False, "error": "quota exceeded"}

    db = tmp_path / "s.sqlite"
    result = sources_db.sync(
        db,
        "gsc",
        "sc-domain:example.test",
        start_date="2026-01-01",
        end_date="2026-01-02",
        fetchers={"gsc": failing},
    )
    assert result["ok"] is False and result["failed_period"] == ["2026-01-01", "2026-01-01"]
    assert len(attempts) == sources_db.RETRIES + 1
    entry = sources_db.status(db)["sources"][0]
    assert entry["coverage"]["failed"] == ["2026-01-01"]
    assert entry["coverage"]["pending"] == ["2026-01-02"]
    assert entry["rows"] == 0


def test_adapter_exception_marks_failed_without_leaking_exception_text(tmp_path):
    db = tmp_path / "sources.sqlite"

    def raised(*_):
        raise RuntimeError("synthetic credential text")

    result = sources_db.sync(
        db,
        "gsc",
        "p",
        start_date="2026-01-01",
        end_date="2026-01-01",
        fetchers={"gsc": raised},
    )
    assert result["ok"] is False and result["attempted_days"] == 1
    assert "synthetic credential text" not in str(result)
    assert sources_db.status(db)["sources"][0]["coverage"]["failed"] == ["2026-01-01"]


@pytest.mark.parametrize(
    ("failure", "expected"),
    [
        ({"status": 403, "error": "forbidden"}, "no_access"),
        ({"status": 401, "error": "invalid_grant"}, "invalid_grant"),
        ({"status": 400, "error": "insufficient_scope"}, "insufficient_scope"),
        ({"status": 500, "error": "unavailable"}, "failed"),
    ],
)
def test_sync_retains_only_explicit_provider_access_failure_classes(tmp_path, failure, expected):
    db = tmp_path / "sources.sqlite"
    sources_db.sync(
        db,
        "gsc",
        "sc-domain:example.test",
        start_date="2026-01-01",
        end_date="2026-01-01",
        fetchers={"gsc": lambda *_: {"ok": False, "retry": False, **failure}},
    )
    status = sources_db.status(db)["sources"][0]
    assert status["last_attempts"]["2026-01-01"] == expected


def test_incomplete_days_are_reported_and_refetched(tmp_path):
    db = tmp_path / "s.sqlite"

    def truncated(resource, start, end):
        return dict(_gsc_rows(resource, start, end), complete=False)

    sources_db.sync(
        db, "gsc", "p", start_date="2026-01-01", end_date="2026-01-01", fetchers={"gsc": truncated}
    )
    assert sources_db.status(db)["sources"][0]["incomplete_days"] == ["2026-01-01"]
    calls, fetch = _recorder(_gsc_rows)
    sources_db.sync(
        db, "gsc", "p", start_date="2026-01-01", end_date="2026-01-01", fetchers={"gsc": fetch}
    )
    assert calls and sources_db.status(db)["sources"][0]["incomplete_days"] == []


def test_forced_partial_retry_preserves_complete_day_and_marks_attempt(tmp_path):
    db = tmp_path / "sources.sqlite"
    sources_db.sync(
        db,
        "gsc",
        "p",
        start_date="2026-01-01",
        end_date="2026-01-01",
        fetchers={"gsc": _gsc_rows},
    )

    def partial(*_):
        return {
            "ok": True,
            "complete": False,
            "rows": [{"date": "2026-01-01", "query": "other", "page": "/other", "clicks": 99}],
        }

    sources_db.sync(
        db,
        "gsc",
        "p",
        start_date="2026-01-01",
        end_date="2026-01-01",
        force=True,
        fetchers={"gsc": partial},
    )
    rows = sources_db.query(db, "gsc")["rows"]
    assert [(row["query"], row["clicks"]) for row in rows] == [("pump", 1.0)]
    entry = sources_db.status(db)["sources"][0]
    assert entry["coverage"]["complete"] == ["2026-01-01"]
    assert entry["last_attempts"] == {"2026-01-01": "partial"}


def test_invalid_or_duplicate_provider_rows_do_not_replace_complete_evidence(tmp_path):
    db = tmp_path / "sources.sqlite"
    sources_db.sync(
        db,
        "gsc",
        "p",
        start_date="2026-01-01",
        end_date="2026-01-01",
        fetchers={"gsc": _gsc_rows},
    )
    for bad_rows in (
        [{"date": "2026-01-01", "query": "x", "page": "/", "clicks": float("inf")}],
        [
            {"date": "2026-01-01", "query": "x", "page": "/", "clicks": 1},
            {"date": "2026-01-01", "query": "x", "page": "/", "clicks": 2},
        ],
        [{"date": "2026-02-01", "query": "x", "page": "/", "clicks": 1}],
    ):

        def invalid(*_, rows=bad_rows):
            return {"ok": True, "complete": True, "rows": rows}

        result = sources_db.sync(
            db,
            "gsc",
            "p",
            start_date="2026-01-01",
            end_date="2026-01-01",
            force=True,
            fetchers={"gsc": invalid},
        )
        assert result["ok"] is False
        assert sources_db.query(db, "gsc")["rows"][0]["query"] == "pump"
    assert sources_db.status(db)["sources"][0]["last_attempts"]["2026-01-01"] == "failed"


def test_empty_partial_and_export_limit_are_explicit(tmp_path):
    db = tmp_path / "sources.sqlite"
    sources_db.sync(
        db,
        "gsc",
        "p",
        start_date="2026-01-01",
        end_date="2026-01-01",
        fetchers={"gsc": lambda *_: {"ok": True, "complete": True, "rows": []}},
    )
    sources_db.sync(
        db,
        "gsc",
        "p",
        start_date="2026-01-02",
        end_date="2026-01-02",
        fetchers={"gsc": lambda *_: {"ok": True, "complete": False, "rows": []}},
    )
    sources_db.sync(
        db,
        "gsc",
        "p",
        start_date="2026-01-03",
        end_date="2026-01-04",
        fetchers={"gsc": _gsc_rows},
    )
    entry = sources_db.status(db)["sources"][0]
    assert entry["coverage"]["empty"] == ["2026-01-01"]
    assert entry["coverage"]["partial"] == ["2026-01-02"]
    target = tmp_path / "out.csv"
    export = sources_db.query(db, "gsc", limit=1, out=str(target))
    assert export["rows"] == 1 and export["has_more"] is True
    assert len(list(csv.reader(target.open()))) == 2
    assert export["non_additive_metrics"] == ["ctr", "position"]


def test_status_uses_read_only_connection_and_rejects_foreign_schema(tmp_path):
    db = tmp_path / "sources.sqlite"
    sources_db.sync(
        db,
        "gsc",
        "p",
        start_date="2026-01-01",
        end_date="2026-01-01",
        fetchers={"gsc": _gsc_rows},
    )
    connection = sources_db.connect(db, create=False)
    try:
        assert connection.execute("PRAGMA query_only").fetchone()[0] == 1
        with pytest.raises(sqlite3.OperationalError):
            connection.execute("INSERT INTO sources_meta VALUES('x','y')")
    finally:
        connection.close()
    foreign = tmp_path / "foreign.sqlite"
    with sqlite3.connect(foreign) as connection:
        connection.execute("CREATE TABLE unrelated(x)")
    with pytest.raises(ValueError, match="not a versioned sources database"):
        sources_db.status(foreign)
    if os.name != "nt":
        assert db.stat().st_mode & 0o077 == 0


def test_failed_sql_insert_rolls_back_one_complete_day(tmp_path):
    db = tmp_path / "sources.sqlite"
    sources_db.sync(
        db,
        "gsc",
        "p",
        start_date="2026-01-01",
        end_date="2026-01-01",
        fetchers={"gsc": _gsc_rows},
    )
    with sqlite3.connect(db) as connection:
        connection.execute(
            "CREATE TRIGGER reject_replacement BEFORE INSERT ON gsc "
            "BEGIN SELECT RAISE(FAIL, 'synthetic insert failure'); END"
        )
    connection = sources_db.connect(db, create=True)
    try:
        with pytest.raises(sqlite3.IntegrityError, match="synthetic insert failure"):
            sources_db._store(
                connection,
                "gsc",
                "p",
                ["2026-01-01"],
                [{"date": "2026-01-01", "query": "new", "page": "/", "clicks": 2}],
                True,
            )
    finally:
        connection.close()
    assert sources_db.query(db, "gsc")["rows"][0]["query"] == "pump"
    assert sources_db.status(db)["sources"][0]["coverage"]["complete"] == ["2026-01-01"]


def test_status_reports_gaps_and_empty_days(tmp_path):
    db = tmp_path / "s.sqlite"
    for day in ("2026-01-01", "2026-01-02", "2026-01-06"):
        sources_db.sync(db, "gsc", "p", start_date=day, end_date=day, fetchers={"gsc": _gsc_rows})
    sources_db.sync(
        db,
        "gsc",
        "p",
        start_date="2026-01-07",
        end_date="2026-01-07",
        fetchers={"gsc": lambda *a: {"ok": True, "rows": [], "complete": True}},
    )
    entry = sources_db.status(db)["sources"][0]
    assert entry["gaps"] == [["2026-01-03", "2026-01-05"]]
    assert entry["synced_days"] == 4 and entry["empty_days"] == 1 and entry["rows"] == 3


def test_export_filters_json_and_writes_csv_without_overwriting(tmp_path):
    db = tmp_path / "s.sqlite"
    sources_db.sync(
        db,
        "gsc",
        "p",
        start_date="2026-01-01",
        end_date="2026-01-03",
        fetchers={"gsc": _gsc_rows},
    )
    result = sources_db.query(db, "gsc", start_date="2026-01-02", match="pum", limit=1)
    assert result["returned"] == 1 and result["has_more"] is True
    assert result["rows"][0]["date"] == "2026-01-02"
    assert result["rows"][0]["coverage_state"] == "complete"
    out = tmp_path / "gsc.csv"
    assert sources_db.query(db, "gsc", out=str(out))["rows"] == 3
    with out.open(encoding="utf-8") as handle:
        exported = list(csv.DictReader(handle))
        assert len(exported) == 3
        assert all(row["coverage_state"] == "complete" for row in exported)
    if os.name != "nt":
        assert out.stat().st_mode & 0o077 == 0
    with pytest.raises(ValueError, match="overwrite"):
        sources_db.query(db, "gsc", out=str(out))


def test_database_location_and_missing_database(tmp_path):
    with pytest.raises(ValueError, match="exactly one"):
        sources_db.db_path()
    with pytest.raises(ValueError, match=r"project\.json"):
        sources_db.db_path(project=str(tmp_path))
    (tmp_path / "project.json").write_text("{}", encoding="utf-8")
    assert sources_db.db_path(project=str(tmp_path)) == tmp_path / "sources.sqlite"
    with pytest.raises(ValueError, match="run sources-sync"):
        sources_db.status(tmp_path / "missing.sqlite")
    with pytest.raises(ValueError, match="source must be one of"):
        sources_db.sync(tmp_path / "s.sqlite", "bing", "x")


def test_handlers_route_to_the_database(tmp_path, monkeypatch):
    monkeypatch.setitem(sources_db.FETCHERS, "gsc", _gsc_rows)
    db = str(tmp_path / "s.sqlite")
    synced = handlers.sources_sync(
        source="gsc", resource="p", start_date="2026-01-01", end_date="2026-01-01", db=db
    )
    assert synced["rows_stored"] == 1
    assert handlers.sources_status(db=db)["sources"][0]["resource"] == "p"
    assert handlers.sources_export(source="gsc", db=db)["returned"] == 1


def test_offline_cli_status_and_export_match_shared_handlers(tmp_path, monkeypatch, capsys):
    db = tmp_path / "sources.sqlite"
    sources_db.sync(
        db,
        "gsc",
        "p",
        start_date="2026-01-01",
        end_date="2026-01-01",
        fetchers={"gsc": _gsc_rows},
    )
    monkeypatch.setattr("sys.stdin", io.StringIO('{"unexpected": true}'))
    assert cli_main(["sources-status", "--db", str(db)]) == 0
    cli_status = json.loads(capsys.readouterr().out)
    assert cli_status == handlers.sources_status(db=str(db))
    assert cli_main(["sources-export", "--db", str(db), "--source", "gsc"]) == 0
    cli_export = json.loads(capsys.readouterr().out)
    assert cli_export == handlers.sources_export(db=str(db), source="gsc")


def test_webmaster_history_sums_crawl_batches_and_keeps_level_snapshots(monkeypatch):
    answers = {
        "search_history": {
            "indicators": {"TOTAL_SHOWS": [{"date": "2026-01-01T00:00", "value": 9}]}
        },
        "indexing_history": {
            "indicators": {
                "HTTP_2XX": [
                    {"date": "2026-01-01T09:00", "value": 2},
                    {"date": "2026-01-01T18:00", "value": 3},
                ]
            }
        },
        "in_search_history": {
            "history": [
                {"date": "2026-01-01T09:00", "value": 100},
                {"date": "2026-01-01T19:00", "value": 120},
            ]
        },
    }
    monkeypatch.setattr(
        sources_db, "_webmaster", lambda op, host, params: {"ok": True, "data": answers[op]}
    )
    rows = sources_db._fetch_webmaster_history("h", "2026-01-01", "2026-01-01")["rows"]
    assert {(r["indicator"], r["value"]) for r in rows} == {
        ("TOTAL_SHOWS", 9.0),
        ("HTTP_2XX", 5.0),
        ("IN_SEARCH", 120.0),
    }


def test_webmaster_popular_queries_page_through_one_day(monkeypatch):
    calls = []

    def fake(op, host, params, *, paginate=False):
        calls.append((op, paginate, params["date_from"]))
        return {
            "ok": True,
            "truncated": False,
            "data": {
                "count": 501,
                "queries": [{"query_text": f"q{n}", "indicators": {}} for n in range(501)],
            },
        }

    monkeypatch.setattr(sources_db, "_webmaster", fake)
    result = sources_db._fetch_webmaster("h", "2026-01-01", "2026-01-01")
    assert calls == [("search_performance", True, "2026-01-01")]
    assert len(result["rows"]) == 501 and result["complete"]


def test_webmaster_bridge_uses_current_adapter(monkeypatch):
    calls = []

    def fake(operation, **kwargs):
        calls.append((operation, kwargs))
        return {"ok": True, "data": {"count": 0, "queries": []}, "truncated": False}

    monkeypatch.setattr(yandex_webmaster, "collect", fake)
    assert sources_db._fetch_webmaster("https:example.test:443", "2026-01-01", "2026-01-01")[
        "complete"
    ]
    assert calls[0][0] == "search_performance"
    assert calls[0][1]["paginate"] is True


def test_webmaster_missing_count_is_failed_evidence(monkeypatch):
    monkeypatch.setattr(
        sources_db,
        "_webmaster",
        lambda *args, **kwargs: {"ok": True, "data": {"queries": []}},
    )
    result = sources_db._fetch_webmaster("h", "2026-01-01", "2026-01-01")
    assert result["ok"] is False and result["retry"] is False


def test_ga4_run_report_pages_and_flattens_rows():
    offsets = []

    def transport(url, payload, token):
        offsets.append(payload["offset"])
        start = int(payload["offset"])
        rows = [
            {"dimensionValues": [{"value": f"/p{n}"}], "metricValues": [{"value": str(n)}]}
            for n in range(start, min(start + 2, 3))
        ]
        return json.dumps({"rows": rows, "rowCount": 3})

    result = ga4.run_report(
        "1",
        "2026-01-01",
        "2026-01-01",
        ["landingPage"],
        ["sessions"],
        token="t",
        transport=transport,
    )
    assert result["returned"] == 3 and offsets == ["0", "2"]
    assert result["rows"][2] == {"landingPage": "/p2", "sessions": "2"}


def test_ga4_report_keeps_zero_distinct_from_malformed_and_sampling():
    empty = ga4.run_report(
        "1",
        "2026-01-01",
        "2026-01-01",
        ["date"],
        ["sessions"],
        token="synthetic",
        transport=lambda *_: json.dumps({"rowCount": 0}),
    )
    assert empty["state"] == "complete" and empty["rows"] == []
    missing_count = ga4.run_report(
        "1",
        "2026-01-01",
        "2026-01-01",
        ["date"],
        ["sessions"],
        token="synthetic",
        transport=lambda *_: json.dumps({"rows": []}),
    )
    assert missing_count["ok"] is False
    sampled = ga4.run_report(
        "1",
        "2026-01-01",
        "2026-01-01",
        ["date"],
        ["sessions"],
        token="synthetic",
        transport=lambda *_: json.dumps(
            {
                "rowCount": 1,
                "rows": [
                    {"dimensionValues": [{"value": "20260101"}], "metricValues": [{"value": "0"}]}
                ],
                "metadata": {"samplingMetadatas": [{"samplesReadCount": "10"}]},
            }
        ),
    )
    assert sampled["state"] == "partial" and sampled["rows"][0]["sessions"] == "0"
    over_returned = ga4.run_report(
        "1",
        "2026-01-01",
        "2026-01-01",
        ["date"],
        ["sessions"],
        max_rows=1,
        token="synthetic",
        transport=lambda *_: json.dumps(
            {
                "rowCount": 2,
                "rows": [
                    {"dimensionValues": [{"value": "20260101"}], "metricValues": [{"value": "0"}]}
                ]
                * 2,
            }
        ),
    )
    assert over_returned["ok"] is False
    calls = []

    def two_pages(url, payload, token):
        calls.append(payload["offset"])
        row = {"dimensionValues": [{"value": "20260101"}], "metricValues": [{"value": "0"}]}
        return json.dumps(
            {
                "rowCount": 2,
                "rows": [row],
                "metadata": {"subjectToThresholding": payload["offset"] == "0"},
            }
        )

    thresholded = ga4.run_report(
        "1",
        "2026-01-01",
        "2026-01-01",
        ["date"],
        ["sessions"],
        token="synthetic",
        transport=two_pages,
    )
    assert calls == ["0", "1"] and thresholded["state"] == "partial"


def test_ga4_sampling_marks_daily_partition_partial(monkeypatch, tmp_path):
    def sampled(*args, **kwargs):
        return {
            "ok": True,
            "state": "partial",
            "rows": [
                {
                    "date": "20260101",
                    "landingPagePlusQueryString": "/",
                    "sessionDefaultChannelGroup": "Organic Search",
                    "sessions": "0",
                    "engagedSessions": "0",
                    "totalUsers": "0",
                }
            ],
        }

    monkeypatch.setattr(ga4, "run_report", sampled)
    db = tmp_path / "sources.sqlite"
    result = sources_db.sync(db, "ga4", "123", start_date="2026-01-01", end_date="2026-01-01")
    assert result["synced_days"] == 0 and result["attempted_days"] == 1
    assert sources_db.status(db)["sources"][0]["coverage"]["partial"] == ["2026-01-01"]
    assert sources_db.query(db, "ga4")["rows"][0]["sessions"] == 0.0


def test_metrika_report_offset_is_one_based(monkeypatch, tmp_path):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))
    client = metrika.MetrikaClient(token="synthetic")
    urls = []

    def fake(url):
        urls.append(url)
        return {"data": [], "query": {}}

    monkeypatch.setattr(client, "_request", fake)
    client.report({"ids": "1", "metrics": "ym:s:visits"})
    client.report({"ids": "1", "metrics": "ym:s:visits"}, paginate=True)
    assert all("offset=1" in url for url in urls)


def test_gsc_daily_bridge_uses_bounded_current_adapter(monkeypatch, tmp_path):
    from seohead.data_sources import gsc

    calls = []

    def page(site, **kw):
        calls.append(kw)
        row = {"keys": ["2026-01-01", "q", "/p"], "clicks": 1, "impressions": 2}
        return {"ok": True, "rows": [dict(row, ctr=0.5, position=3)], "truncated": True}

    monkeypatch.setattr(gsc, "search_analytics_pages", page)
    result = sources_db._fetch_gsc("p", "2026-01-01", "2026-01-01")
    assert calls[0]["max_rows"] == gsc.MAX_ANALYTICS_ROWS
    assert result["complete"] is False

    calls.clear()

    def quota(site, **kw):
        calls.append(1)
        return {"ok": False, "status": 429, "reason": "quotaExceeded", "error": "failed"}

    monkeypatch.setattr(gsc, "search_analytics_pages", quota)
    result = sources_db.sync(
        tmp_path / "s.sqlite", "gsc", "p", start_date="2026-01-01", end_date="2026-01-01"
    )
    assert result["ok"] is False and result["error"] == "failed" and len(calls) == 1


def test_webmaster_4xx_is_not_retried(monkeypatch, tmp_path):
    calls = []

    def failing(op, host, params):
        calls.append(op)
        return {"ok": False, "error": "HTTP 404", "status": 404}

    monkeypatch.setattr(sources_db, "_webmaster", failing)
    synced = sources_db.sync(
        tmp_path / "s.sqlite",
        "webmaster_history",
        "h",
        start_date="2026-01-01",
        end_date="2026-01-02",
    )
    assert synced["ok"] is False and len(calls) == 1
