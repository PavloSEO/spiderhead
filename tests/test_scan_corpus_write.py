"""Schema-backed bounded captured-response writer tests."""

from __future__ import annotations

import json
import sqlite3
from dataclasses import replace
from pathlib import Path

import pytest

from seohead.crawl.capture import CaptureEvent
from seohead.storage.bodies import read_document
from seohead.storage.corpus import (
    _reserve_unique_bytes,
    _retained_body_bytes,
    corpus_summary,
    rendered_body_retention,
    store_rendered_document,
    store_response,
)
from seohead.storage.corpus_validation import validate_corpus
from seohead.storage.retention import NO_BODY_RETENTION


def _con():
    con = sqlite3.connect(":memory:")
    con.executescript((Path("seohead/storage/scan_v1.sql")).read_text())
    return con


def _policy(**values):
    return {
        **NO_BODY_RETENTION,
        "body_mode": "captured_entity_bytes",
        "max_body_bytes": 1024,
        "max_body_store_bytes": 1024,
        **values,
    }


def _event(**values):
    row = {
        "method": "GET",
        "requested_url": "https://example.test/a",
        "effective_url": "https://example.test/a",
        "redirect_history": (),
        "requested_at": "2026-01-01T00:00:00Z",
        "received_at": "2026-01-01T00:00:01Z",
        "status_code": 200,
        "request_headers": (("accept", "text/html"),),
        "credentials_used": False,
        "response_headers": (("content-type", "text/html; charset=utf-8"),),
        "content_type": "text/html; charset=utf-8",
        "content_encoding": "",
        "entity_bytes": b"<html>body</html>",
        "body_fidelity": "entity_bytes",
        "body_state": "complete",
        "body_reason": "none",
        "error": "",
        "error_kind": "",
        "effective_status_code": 200,
        "effective_headers": (("content-type", "text/html; charset=utf-8"),),
        "response_time": 0.1,
    }
    row.update(values)
    return CaptureEvent(**row)


def _renderer(**values):
    row = {
        "engine": "playwright-chromium",
        "engine_version": "1.52.0",
        "settings": {
            "viewport": {"width": 1280, "height": 720},
            "device_pixel_ratio": 1.0,
            "mobile_emulation": False,
            "touch_emulation": False,
            "script_timeout_seconds": 0.0,
            "resize_to_content": False,
            "resize_to_content_max_height_px": 15000,
            "persistent_profile": False,
        },
        "navigation": {
            "requested_url": "https://example.test/a",
            "final_url": "https://example.test/a",
            "wait_until": "load",
            "timeout_seconds": 30.0,
        },
        "transforms": {
            "flatten_shadow_dom_requested": True,
            "flatten_shadow_dom_applied": 1,
            "flatten_iframes_requested": True,
            "flatten_iframes_applied": 2,
        },
        "policy": {"credentials_used": False, "cache_control_no_store": False},
    }
    row.update(values)
    return row


def _encoded(size, key):
    return {
        "sha256": f"{key:064x}",
        "codec": "identity",
        "decoded_bytes": size,
        "stored_bytes": size,
        "data": b"x" * size,
    }


def test_body_budget_counts_existing_bytes_once_and_keeps_deduplication():
    con = _con()
    con.executemany(
        "INSERT INTO bodies VALUES(?, 'identity', 1, 1, X'78')",
        [(f"{key:064x}",) for key in range(2000)],
    )
    statements = []
    con.set_trace_callback(statements.append)
    policy = _policy(max_body_store_bytes=2010)
    for key in range(2000, 2010):
        assert _reserve_unique_bytes(con, _encoded(1, key), policy)
    assert not _reserve_unique_bytes(con, _encoded(1, 2010), policy)
    assert _reserve_unique_bytes(con, _encoded(1, 2000), policy)
    assert _retained_body_bytes(con) == 2010
    sums = [sql for sql in statements if "SUM(stored_bytes)" in sql]
    assert len(sums) == 1


@pytest.mark.parametrize("outer_transaction", [False, True])
def test_body_byte_counter_follows_rollback_savepoint_delete_and_update(outer_transaction):
    con = _con()
    policy = _policy(max_body_store_bytes=10)
    if outer_transaction:
        con.execute("BEGIN")
    assert _reserve_unique_bytes(con, _encoded(4, 1), policy)
    assert con.in_transaction
    con.rollback()  # May also undo initialization; either path must rebuild safely.
    assert _retained_body_bytes(con) == 0
    con.commit()
    assert _reserve_unique_bytes(con, _encoded(4, 1), policy)
    con.commit()
    con.execute("SAVEPOINT body_write")
    assert _reserve_unique_bytes(con, _encoded(6, 2), policy)
    assert not _reserve_unique_bytes(con, _encoded(1, 3), policy)
    con.execute("ROLLBACK TO body_write")
    con.execute("RELEASE body_write")
    assert _retained_body_bytes(con) == 4
    assert _reserve_unique_bytes(con, _encoded(6, 2), policy)
    con.execute("DELETE FROM bodies WHERE sha256=?", (f"{1:064x}",))
    assert _retained_body_bytes(con) == 6
    con.execute("UPDATE bodies SET data=X'7878',stored_bytes=2 WHERE sha256=?", (f"{2:064x}",))
    assert _retained_body_bytes(con) == 2
    con.rollback()
    assert _retained_body_bytes(con) == 4


def test_body_byte_counter_rebuilds_on_reopen_without_changing_durable_schema(tmp_path):
    path = tmp_path / "bodies.sqlite"
    con = sqlite3.connect(path)
    con.executescript(Path("seohead/storage/scan_v1.sql").read_text())
    schema = list(con.execute("SELECT name,sql FROM sqlite_master ORDER BY name"))
    assert _reserve_unique_bytes(con, _encoded(7, 1), _policy())
    con.commit()
    assert list(con.execute("SELECT name,sql FROM sqlite_master ORDER BY name")) == schema
    con.close()
    con = sqlite3.connect(path)
    assert _retained_body_bytes(con) == 7
    assert not _reserve_unique_bytes(con, _encoded(4, 2), _policy(max_body_store_bytes=10))
    assert _reserve_unique_bytes(con, _encoded(3, 2), _policy(max_body_store_bytes=10))
    assert _retained_body_bytes(con) == 10
    con.close()


def test_complete_zero_and_duplicate_bodies_are_deduplicated_but_responses_remain_distinct():
    con = _con()
    first, document = store_response(
        con, _event(entity_bytes=b""), purpose="page", policy=_policy()
    )
    second, _ = store_response(
        con,
        _event(request_headers=(("accept", "application/xhtml+xml"),)),
        purpose="page",
        policy=_policy(),
    )
    assert document is not None and (first, second) == (1, 2)
    assert con.execute("SELECT COUNT(*) FROM bodies").fetchone()[0] == 2
    variants = [
        row[0] for row in con.execute("SELECT variant_key FROM responses ORDER BY request_ordinal")
    ]
    assert len(set(variants)) == 2
    zero = con.execute(
        "SELECT body_state,body_reason,body_sha256 FROM responses WHERE response_id=1"
    ).fetchone()
    assert zero[0:2] == ("complete", "none") and zero[2]


def test_rendered_dom_is_utf8_serialized_with_provenance_and_body_deduplication():
    con = _con()
    store_response(con, _event(entity_bytes=b"<html>same</html>"), purpose="page", policy=_policy())
    document_id = store_rendered_document(
        con,
        logical_url="https://example.test/a",
        html="<html>same</html>",
        renderer=_renderer(),
        policy=_policy(),
        captured_at="2026-01-01T00:00:02Z",
    )
    document = con.execute(
        "SELECT representation,source_response_id,fidelity,body_state,body_sha256,decoder_source,"
        "decoder_charset,renderer_json FROM documents WHERE document_id=?",
        (document_id,),
    ).fetchone()
    assert document[:7] == (
        "rendered",
        None,
        "serialized_dom",
        "complete",
        document[4],
        "renderer_utf8",
        "utf-8",
    )
    provenance = json.loads(document[7])
    assert provenance["navigation_transform"] == "direct"
    assert provenance["flattened_iframes"] is True
    assert provenance["capture_limitations"] == []
    assert provenance["page_concurrency"] == 1
    assert "persistent_profile_dir" not in provenance["settings"]
    assert con.execute("SELECT COUNT(*) FROM bodies").fetchone()[0] == 1
    validate_corpus(con, {"source_kind": "native"}, _policy())

    # Pre-#744 scans have no page-concurrency field; readers still accept their
    # historical renderer document without treating it as malformed.
    provenance.pop("page_concurrency")
    con.execute(
        "UPDATE documents SET renderer_json=? WHERE document_id=?",
        (json.dumps(provenance), document_id),
    )
    validate_corpus(con, {"source_kind": "native"}, _policy())
    assert read_document(con, document_id, max_decoded_bytes=1024) == "<html>same</html>"


@pytest.mark.parametrize(
    ("renderer", "policy", "state", "reason"),
    [
        (
            _renderer(policy={"credentials_used": True, "cache_control_no_store": False}),
            _policy(),
            "omitted",
            "credentialed",
        ),
        (
            _renderer(policy={"credentials_used": False, "cache_control_no_store": True}),
            _policy(),
            "omitted",
            "cache_control_no_store",
        ),
        (_renderer(), dict(NO_BODY_RETENTION), "omitted", "not_enabled"),
        (_renderer(), _policy(max_body_bytes=4), "omitted", "body_budget_exhausted"),
    ],
)
def test_rendered_dom_retention_respects_privacy_and_finite_budgets(
    renderer, policy, state, reason
):
    con = _con()
    document_id = store_rendered_document(
        con,
        logical_url="https://example.test/a",
        html="<html>DOM</html>",
        renderer=renderer,
        policy=policy,
        captured_at="2026-01-01T00:00:02Z",
    )
    document = con.execute(
        "SELECT fidelity,body_state,body_reason,body_sha256,renderer_json FROM documents WHERE document_id=?",
        (document_id,),
    ).fetchone()
    assert document[:4] == ("unavailable", state, reason, None)
    assert json.loads(document[4])["capture_limitations"] == [reason]
    assert con.execute("SELECT COUNT(*) FROM bodies").fetchone()[0] == 0


def test_rendered_dom_truncation_never_accepts_partial_bytes_and_legacy_fragment_keeps_entity_fidelity():
    con = _con()
    document_id = store_rendered_document(
        con,
        logical_url="https://example.test/a",
        html=None,
        renderer=_renderer(),
        policy=_policy(),
        captured_at="2026-01-01T00:00:02Z",
        body_state="truncated",
        body_reason="truncated",
    )
    assert con.execute(
        "SELECT fidelity,body_state,body_reason,body_sha256 FROM documents WHERE document_id=?",
        (document_id,),
    ).fetchone() == ("unavailable", "truncated", "truncated", None)
    with pytest.raises(Exception, match="cannot retain DOM bytes"):
        store_rendered_document(
            con,
            logical_url="https://example.test/a",
            html=b"partial",
            renderer=_renderer(),
            policy=_policy(),
            captured_at="2026-01-01T00:00:02Z",
            body_state="truncated",
            body_reason="truncated",
        )

    response, legacy = store_response(
        con,
        _event(
            requested_url="https://example.test/?_escaped_fragment_=",
            effective_url="https://example.test/?_escaped_fragment_=",
            entity_bytes=b"<html>legacy</html>",
        ),
        purpose="page",
        policy=_policy(),
        logical_url="https://example.test/",
        representation="legacy_fragment",
        renderer=_renderer(
            navigation={
                "requested_url": "https://example.test/?_escaped_fragment_=",
                "final_url": "https://example.test/?_escaped_fragment_=",
                "wait_until": "load",
                "timeout_seconds": 30.0,
            },
            navigation_transform="legacy_escaped_fragment",
        ),
    )
    assert legacy is not None
    legacy_row = con.execute(
        "SELECT source_response_id,fidelity,body_state,renderer_json FROM documents WHERE document_id=?",
        (legacy,),
    ).fetchone()
    assert legacy_row[:3] == (response, "entity_bytes", "complete")
    provenance = json.loads(legacy_row[3])
    assert provenance["navigation_transform"] == "legacy_escaped_fragment"
    assert (
        con.execute(
            "SELECT url FROM urls WHERE url_id=?", (provenance["navigation_url_id"],)
        ).fetchone()[0]
        == "https://example.test/?_escaped_fragment_="
    )
    assert read_document(con, legacy, max_decoded_bytes=1024) == "<html>legacy</html>"
    validate_corpus(con, {"source_kind": "native"}, _policy())


def test_rendered_body_retention_counts_only_complete_documents_as_retained():
    con = _con()
    for state, reason, html in (
        ("complete", "none", "<html>complete</html>"),
        ("unavailable", "fetch_failed", None),
        ("truncated", "truncated", None),
    ):
        store_rendered_document(
            con,
            logical_url="https://example.test/a",
            html=html,
            renderer=_renderer(),
            policy=_policy(),
            captured_at="2026-01-01T00:00:02Z",
            body_state=state,
            body_reason=reason,
        )

    assert rendered_body_retention(con) == {
        "total": 3,
        "retained": 1,
        "omitted": {"fetch_failed": 1, "truncated": 1},
    }


def test_rendered_dom_rejects_non_utf8_and_wrong_legacy_transform():
    con = _con()
    with pytest.raises(Exception, match="not valid UTF-8"):
        store_rendered_document(
            con,
            logical_url="https://example.test/a",
            html=b"\xff",
            renderer=_renderer(),
            policy=_policy(),
            captured_at="2026-01-01T00:00:02Z",
        )
    with pytest.raises(Exception, match="must declare its navigation transform"):
        store_response(
            con,
            _event(),
            purpose="page",
            policy=_policy(),
            representation="legacy_fragment",
            renderer=_renderer(),
        )


@pytest.mark.parametrize(
    ("event", "policy", "state", "reason"),
    [
        (_event(credentials_used=True), _policy(), "omitted", "credentialed"),
        (
            _event(response_headers=(("cache-control", "private, no-store"),)),
            _policy(),
            "omitted",
            "cache_control_no_store",
        ),
        (_event(), dict(NO_BODY_RETENTION), "omitted", "not_enabled"),
        (_event(content_type="application/pdf"), _policy(), "omitted", "unsupported_media"),
        (
            _event(entity_bytes=b"x" * 20),
            _policy(max_body_bytes=10),
            "omitted",
            "body_budget_exhausted",
        ),
        (
            _event(entity_bytes=None, body_state="truncated", body_reason="truncated"),
            _policy(),
            "truncated",
            "truncated",
        ),
        (
            _event(entity_bytes=None, body_state="unavailable", body_reason="not_fetched"),
            _policy(),
            "unavailable",
            "not_fetched",
        ),
    ],
)
def test_retention_state_precedence_never_hashes_noncomplete_cases(event, policy, state, reason):
    con = _con()
    store_response(con, event, purpose="page", policy=policy)
    response = con.execute(
        "SELECT body_state,body_reason,body_sha256,body_fidelity FROM responses"
    ).fetchone()
    document = con.execute(
        "SELECT body_state,body_reason,body_sha256,fidelity FROM documents"
    ).fetchone()
    assert response == (state, reason, None, "unavailable")
    assert document == (state, reason, None, "unavailable")
    assert con.execute("SELECT COUNT(*) FROM bodies").fetchone()[0] == 0


def test_unique_encoded_budget_charges_only_new_body_rows():
    con = _con()
    body = b"compressible " * 50
    first = _event(entity_bytes=body)
    store_response(con, first, purpose="page", policy=_policy(max_body_store_bytes=30))
    store_response(con, first, purpose="page", policy=_policy(max_body_store_bytes=30))
    third, _ = store_response(
        con,
        _event(entity_bytes=b"different body" * 50),
        purpose="page",
        policy=_policy(max_body_store_bytes=30),
    )
    assert con.execute("SELECT COUNT(*) FROM bodies").fetchone()[0] == 1
    assert (
        con.execute("SELECT body_reason FROM responses WHERE response_id=?", (third,)).fetchone()[0]
        == "body_budget_exhausted"
    )


def test_zero_total_budget_allows_empty_and_known_bodies_but_no_new_nonzero_blob():
    con = _con()
    store_response(con, _event(entity_bytes=b"known"), purpose="page", policy=_policy())
    store_response(
        con,
        _event(entity_bytes=b"", requested_url="https://example.test/empty"),
        purpose="page",
        policy=_policy(max_body_store_bytes=0),
    )
    duplicate, _ = store_response(
        con, _event(entity_bytes=b"known"), purpose="page", policy=_policy(max_body_store_bytes=0)
    )
    blocked, _ = store_response(
        con,
        _event(entity_bytes=b"new", requested_url="https://example.test/new"),
        purpose="page",
        policy=_policy(max_body_store_bytes=0),
    )
    assert (
        con.execute(
            "SELECT body_state FROM responses WHERE response_id=?", (duplicate,)
        ).fetchone()[0]
        == "complete"
    )
    assert (
        con.execute("SELECT body_reason FROM responses WHERE response_id=?", (blocked,)).fetchone()[
            0
        ]
        == "body_budget_exhausted"
    )
    assert con.execute("SELECT COUNT(*) FROM bodies").fetchone()[0] == 2


def test_effective_headers_and_declared_metadata_are_preserved_without_body_length_guessing():
    con = _con()
    store_response(
        con,
        _event(
            entity_bytes=b"tiny",
            response_headers=(
                ("content-length", "42"),
                ("content-type", "text/html; charset=iso-8859-1"),
            ),
            effective_headers=(),
            effective_status_code=None,
            content_type="text/html; charset=iso-8859-1",
        ),
        purpose="page",
        policy=_policy(),
    )
    row = con.execute(
        "SELECT effective_status_code,effective_headers_redacted_json,reported_size_bytes,charset FROM responses"
    ).fetchone()
    assert row == (
        200,
        json.dumps(
            [["content-length", "42"], ["content-type", "text/html; charset=iso-8859-1"]],
            separators=(",", ":"),
        ),
        42,
        "iso-8859-1",
    )

    store_response(
        con,
        _event(
            requested_url="https://example.test/invalid-length",
            response_headers=(("content-length", "+1"),),
            effective_headers=(("content-length", "not-a-number"),),
        ),
        purpose="page",
        policy=_policy(),
    )
    assert (
        con.execute("SELECT reported_size_bytes FROM responses WHERE response_id=2").fetchone()[0]
        is None
    )


def test_no_store_uses_cache_directives_from_original_and_effective_headers_only():
    con = _con()
    store_response(
        con,
        _event(effective_headers=(("cache-control", "private, no-store"),)),
        purpose="page",
        policy=_policy(),
    )
    store_response(
        con,
        _event(
            requested_url="https://example.test/token",
            response_headers=(("cache-control", "x-no-store"),),
        ),
        purpose="page",
        policy=_policy(),
    )
    assert list(
        con.execute("SELECT body_state,body_reason FROM responses ORDER BY response_id")
    ) == [
        ("omitted", "cache_control_no_store"),
        ("complete", "none"),
    ]


def test_variant_includes_method_and_capture_scalars_cannot_be_coerced():
    con = _con()
    store_response(con, _event(), purpose="page", policy=_policy())
    store_response(con, _event(method="HEAD"), purpose="page", policy=_policy())
    keys = [row[0] for row in con.execute("SELECT variant_key FROM responses ORDER BY response_id")]
    assert keys[0] != keys[1]
    with pytest.raises(Exception, match="HTTP status code"):
        store_response(con, replace(_event(), status_code=True), purpose="page", policy=_policy())
    with pytest.raises(Exception, match="lacks an effective URL"):
        store_response(
            con,
            replace(
                _event(
                    requested_url="https://example.test/old",
                    effective_url="",
                    redirect_history=(
                        {
                            "request_url": "https://example.test/old",
                            "status_code": 301,
                            "location_raw": "/new",
                            "next_url": "https://example.test/new",
                            "blocked": False,
                        },
                    ),
                )
            ),
            purpose="page",
            policy=_policy(),
        )


def test_redirect_lineage_uses_interned_ids_and_summary_is_sql_only():
    con = _con()
    event = _event(
        requested_url="https://example.test/old",
        effective_url="https://example.test/new",
        redirect_history=(
            {
                "request_url": "https://example.test/old",
                "status_code": 301,
                "location_raw": "/new",
                "next_url": "https://example.test/new",
                "blocked": False,
            },
        ),
    )
    store_response(con, event, purpose="page", policy=_policy())
    chain = con.execute("SELECT redirect_chain_json FROM responses").fetchone()[0]
    assert set(json.loads(chain)[0]) == {
        "request_url_id",
        "status_code",
        "location_raw",
        "next_url_id",
        "blocked",
    }
    assert corpus_summary(con, _policy()) == {
        "capabilities": {
            "responses": {"state": "complete", "reason": ""},
            "html_bodies": {"state": "complete", "reason": ""},
            "rendered_bodies": {"state": "unavailable", "reason": "no captured rendered documents"},
        },
        "corpus_partial": False,
    }


def test_non_html_static_documents_do_not_claim_html_coverage_and_missing_capture_is_partial():
    con = _con()
    _response, document = store_response(
        con,
        _event(content_type="application/pdf", entity_bytes=b"%PDF"),
        purpose="page",
        policy=_policy(),
    )
    assert document is not None
    assert corpus_summary(con, _policy())["capabilities"]["html_bodies"] == {
        "state": "unavailable",
        "reason": "no captured HTML documents",
    }

    con.execute("INSERT INTO urls(url) VALUES(?)", ("https://example.test/missing",))
    missing_url_id = con.execute(
        "SELECT url_id FROM urls WHERE url=?", ("https://example.test/missing",)
    ).fetchone()[0]
    con.execute(
        "INSERT INTO pages("
        "url_id,page_ordinal,document_id,status_code,content_type,size_bytes,response_time,redirect_url,"
        "title,meta_description,h1,h1_2,h2,canonical,meta_robots,x_robots,og_title,og_description,"
        "og_image,word_count,text_ratio,content_frames,content_frames_same_origin,crawl_depth,"
        "content_encoding,charset,doctype,viewport,transport_upgraded,head_count,body_count,head_not_first,"
        "invalid_head_elements,outlinks,external_outlinks,jsonld_blocks_found,jsonld_blocks_parsed,"
        "error,error_kind,cache_status,representation,redirect_chain_json,final_url) "
        "VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            missing_url_id,
            0,
            None,
            200,
            "text/html",
            0,
            None,
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            0,
            None,
            None,
            None,
            0,
            "",
            "",
            "",
            "",
            0,
            0,
            0,
            0,
            "",
            0,
            0,
            0,
            0,
            "",
            "",
            "",
            "static",
            "[]",
            "https://example.test/missing",
        ),
    )
    assert corpus_summary(con, _policy())["capabilities"]["html_bodies"] == {
        "state": "unavailable",
        "reason": "no captured HTML documents",
    }


def test_empty_capture_and_off_policy_are_unavailable_or_partial_and_validate_roundtrip():
    assert corpus_summary(_con(), _policy()) == {
        "capabilities": {
            "responses": {"state": "unavailable", "reason": "no captured responses"},
            "html_bodies": {"state": "unavailable", "reason": "no captured HTML documents"},
            "rendered_bodies": {"state": "unavailable", "reason": "no captured rendered documents"},
        },
        "corpus_partial": True,
    }

    con = _con()
    store_response(con, _event(), purpose="page", policy=dict(NO_BODY_RETENTION))
    assert (
        corpus_summary(con, dict(NO_BODY_RETENTION))["capabilities"]["html_bodies"]["state"]
        == "unavailable"
    )
    validate_corpus(con, {"source_kind": "native"}, dict(NO_BODY_RETENTION))


def test_complete_writer_rows_validate_with_the_retained_body():
    con = _con()
    store_response(con, _event(), purpose="page", policy=_policy())
    validate_corpus(con, {"source_kind": "native"}, _policy())
