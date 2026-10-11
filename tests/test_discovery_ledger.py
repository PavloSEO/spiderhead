"""Offline contract coverage for the scan.v2 discovery occurrence ledger."""

from __future__ import annotations

import sqlite3

import pytest

from seohead.storage import ScanError
from seohead.storage.discovery_ledger import (
    MAX_OCCURRENCES_PER_DOCUMENT,
    _relation_items,
    ensure_schema,
    put,
    read,
    store_document_relations,
)


def _con() -> sqlite3.Connection:
    con = sqlite3.connect(":memory:")
    con.row_factory = sqlite3.Row
    con.executescript(
        "CREATE TABLE urls(url_id INTEGER PRIMARY KEY,url TEXT UNIQUE);"
        "CREATE TABLE frontier(url_id INTEGER,state TEXT);"
        "CREATE TABLE decisions(decision_id INTEGER PRIMARY KEY,url TEXT,reason TEXT);"
    )
    con.execute("INSERT INTO urls VALUES(1,'https://example.test/page')")
    con.execute("INSERT INTO frontier VALUES(1,'done')")
    ensure_schema(con)
    return con


def _occurrence(key: str = "one") -> dict:
    return {
        "occurrence_key": key,
        "source_kind": "html",
        "relation": "canonical",
        "source_url_id": 1,
        "source_document_id": 1,
        "source_response_id": 2,
        "representation": "static",
        "carrier": "link[0]",
        "raw_value": "/page",
        "resolved_value": "https://example.test/page",
        "depth": 0,
        "outcome": "fetched",
        "reason": "",
        "attributes": {},
    }


def test_link_header_keeps_quoted_comma_and_hreflang_metadata():
    items = _relation_items(
        '<base href="https://example.test/base/"><link rel="canonical" href="preferred">',
        "https://example.test/page",
        {"link": '<alt>; title="one,two"; hreflang=fr; rel=alternate, <next>; rel="next"'},
    )
    assert ("canonical", "link[0]", "preferred", "https://example.test/base/preferred", {}) in items
    assert any(item[0] == "hreflang" and item[4]["hreflang"] == "fr" for item in items)
    assert any(item[0] == "next" for item in items)


def test_occurrence_retry_is_idempotent_but_mismatch_is_refused():
    con = _con()
    put(con, _occurrence())
    put(con, _occurrence())
    changed = _occurrence()
    changed["raw_value"] = "/other"
    with pytest.raises(ScanError, match="immutable"):
        put(con, changed)


def test_document_cap_is_partial_not_a_page_write_failure():
    con = _con()
    forms = [{"action": f"/form-{index}"} for index in range(MAX_OCCURRENCES_PER_DOCUMENT + 1)]
    store_document_relations(
        con,
        source_url_id=1,
        source_document_id=1,
        source_response_id=None,
        representation="static",
        source_url="https://example.test/page",
        depth=0,
        html=None,
        headers={},
        links=[],
        candidates=[],
        decisions=[],
        forms=forms,
    )
    result = read(con)
    assert result["state"] == "partial"
    assert result["coverage"][0]["omitted"] == 1


def _validation_con():
    con = _con()
    con.execute(
        "CREATE TABLE documents(document_id INTEGER PRIMARY KEY,url_id INTEGER,representation TEXT,source_response_id INTEGER)"
    )
    con.execute("INSERT INTO documents VALUES(1,1,'static',2)")
    return con


def test_validation_preserves_missing_capture_target_after_later_discovery():
    from seohead.storage.retry import validate_discovery_ledger

    con = _validation_con()
    occurrence = {
        **_occurrence(),
        "relation": "hreflang",
        "raw_value": "/en/page",
        "resolved_value": "https://example.test/en/page",
        "outcome": "unresolved",
        "reason": "target URL was not retained in this scan",
    }
    put(con, occurrence)
    observed = tuple(con.execute("SELECT * FROM discovery_occurrences").fetchone())
    validate_discovery_ledger(con)
    con.execute("INSERT INTO urls VALUES(2,'https://example.test/en/page')")
    con.execute("INSERT INTO frontier VALUES(2,'queued')")
    validate_discovery_ledger(con)
    item = read(con)["items"][0]
    assert item["target_url_id"] is None
    assert item["observed_outcome"] == "unresolved"
    assert item["current_outcome"] == "queued"
    con.execute("UPDATE frontier SET state='done' WHERE url_id=2")
    validate_discovery_ledger(con)
    assert read(con)["items"][0]["current_outcome"] == "fetched"
    assert tuple(con.execute("SELECT * FROM discovery_occurrences").fetchone()) == observed


@pytest.mark.parametrize("outcome", ["queued", "fetched", "blocked"])
def test_validation_rejects_missing_identity_for_claimed_known_target(outcome):
    from seohead.storage.retry import validate_discovery_ledger

    con = _validation_con()
    put(con, _occurrence())
    con.execute("UPDATE discovery_occurrences SET target_url_id=NULL,outcome=?", (outcome,))
    with pytest.raises(ScanError, match="capture-time target identity"):
        validate_discovery_ledger(con)


def test_validation_rejects_an_unexplained_missing_target():
    from seohead.storage.retry import validate_discovery_ledger

    con = _validation_con()
    put(
        con,
        {**_occurrence(), "resolved_value": "https://example.test/later", "outcome": "unresolved"},
    )
    con.execute("INSERT INTO urls VALUES(2,'https://example.test/later')")
    with pytest.raises(ScanError, match="capture-time target identity"):
        validate_discovery_ledger(con)


@pytest.mark.parametrize("tamper", ["wrong_id", "wrong_value", "missing_url"])
def test_validation_keeps_non_null_target_identity_tamper_detection(tamper):
    from seohead.storage.retry import validate_discovery_ledger

    con = _validation_con()
    put(con, _occurrence())
    con.execute("INSERT INTO urls VALUES(2,'https://example.test/other')")
    if tamper == "wrong_id":
        con.execute("UPDATE discovery_occurrences SET target_url_id=2")
    elif tamper == "wrong_value":
        con.execute("UPDATE discovery_occurrences SET resolved_value='https://example.test/other'")
    else:
        con.execute(
            "UPDATE discovery_occurrences SET resolved_value='https://example.test/missing'"
        )
    with pytest.raises(ScanError, match="target identity disagrees"):
        validate_discovery_ledger(con)
