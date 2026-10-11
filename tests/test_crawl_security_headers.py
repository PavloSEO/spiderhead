"""Per-URL security-header findings from stored response evidence (#1013)."""

from __future__ import annotations

import json
import sqlite3

import pytest

from seohead.crawl import security_headers

_ALL_PRESENT = [
    ["strict-transport-security", "max-age=31536000"],
    ["content-security-policy", "default-src 'self'"],
    ["x-content-type-options", "nosniff"],
    ["x-frame-options", "SAMEORIGIN"],
    ["referrer-policy", "strict-origin-when-cross-origin"],
]


def _without(name):
    return [pair for pair in _ALL_PRESENT if pair[0] != name]


def _store(pages):
    """Minimal scan tables: one row per page, optionally with one page response."""
    con = sqlite3.connect(":memory:")
    con.executescript(
        """
        CREATE TABLE urls (url_id INTEGER PRIMARY KEY, url TEXT NOT NULL);
        CREATE TABLE pages (
          url_id INTEGER PRIMARY KEY, page_ordinal INTEGER NOT NULL,
          status_code INTEGER, content_type TEXT NOT NULL);
        CREATE TABLE responses (
          response_id INTEGER PRIMARY KEY, effective_url_id INTEGER,
          purpose TEXT NOT NULL, response_headers_redacted_json TEXT NOT NULL,
          effective_headers_redacted_json TEXT NOT NULL);
        """
    )
    for ordinal, (url_id, url, status, content_type, headers) in enumerate(pages, 1):
        con.execute("INSERT INTO urls VALUES (?,?)", (url_id, url))
        con.execute("INSERT INTO pages VALUES (?,?,?,?)", (url_id, ordinal, status, content_type))
        if headers is not None:
            con.execute(
                "INSERT INTO responses (effective_url_id, purpose, response_headers_redacted_json,"
                " effective_headers_redacted_json) VALUES (?, 'page', '[]', ?)",
                (url_id, json.dumps(headers)),
            )
    return con


def _fired(result):
    return {
        check_id: [item["target_url"] for item in items]
        for check_id, items in result["findings"].items()
        if items
    }


def test_page_with_all_four_headers_is_clean():
    con = _store([(1, "https://e.test/", 200, "text/html; charset=utf-8", _ALL_PRESENT)])
    result = security_headers.evaluate(con)
    assert _fired(result) == {}
    assert result["pages_measured"] == 1
    assert result["pages_unmeasured"] == 0


def test_each_missing_header_fires_on_its_own_check_only():
    headers = [pair for pair in _ALL_PRESENT if pair[0] != "referrer-policy"]
    con = _store([(1, "https://e.test/", 200, "text/html", headers)])
    assert _fired(security_headers.evaluate(con)) == {
        "MISSING_REFERRER_POLICY": ["https://e.test/"]
    }


def test_frame_ancestors_in_csp_satisfies_x_frame_options():
    headers = [
        ["strict-transport-security", "max-age=31536000"],
        ["content-security-policy", "frame-ancestors 'none'"],
        ["x-content-type-options", "nosniff"],
        ["referrer-policy", "no-referrer"],
    ]
    con = _store([(1, "https://e.test/", 200, "text/html", headers)])
    assert _fired(security_headers.evaluate(con)) == {}


def test_empty_header_value_counts_as_missing_for_csp():
    headers = [["content-security-policy", "   "], *_without("content-security-policy")]
    con = _store([(1, "https://e.test/", 200, "text/html", headers)])
    assert _fired(security_headers.evaluate(con)) == {"MISSING_CSP": ["https://e.test/"]}


def test_repeated_security_header_is_reported_and_its_values_combined():
    headers = [*_ALL_PRESENT, ["x-content-type-options", "nosniff"]]
    con = _store([(1, "https://e.test/", 200, "text/html", headers)])
    result = security_headers.evaluate(con)
    assert _fired(result) == {"DUPLICATE_SECURITY_HEADER": ["https://e.test/"]}
    assert result["findings"]["DUPLICATE_SECURITY_HEADER"] == [
        {"target_url": "https://e.test/", "header": "x-content-type-options"}
    ]


def test_repeated_content_security_policy_is_not_a_defect():
    headers = [*_ALL_PRESENT, ["content-security-policy", "frame-ancestors 'none'"]]
    con = _store([(1, "https://e.test/", 200, "text/html", headers)])
    assert _fired(security_headers.evaluate(con)) == {}


@pytest.mark.parametrize(
    "status, content_type",
    [
        (200, "application/pdf"),
        (200, "image/png"),
        (404, "text/html"),
        (301, "text/html"),
    ],
)
def test_non_html_or_non_2xx_pages_are_not_judged(status, content_type):
    con = _store([(1, "https://e.test/x", status, content_type, [])])
    result = security_headers.evaluate(con)
    assert _fired(result) == {}
    assert result["pages_measured"] == 0
    assert result["pages_unmeasured"] == 0


def test_redirect_hop_headers_are_not_judged_in_place_of_the_final_document():
    con = _store([(1, "https://e.test/", 200, "text/html", _ALL_PRESENT)])
    con.execute("UPDATE responses SET response_headers_redacted_json = '[]'")
    assert _fired(security_headers.evaluate(con)) == {}


def test_html_page_without_stored_response_is_unmeasured_not_clean():
    con = _store([(1, "https://e.test/", 200, "text/html", None)])
    result = security_headers.evaluate(con)
    assert _fired(result) == {}
    assert result["pages_measured"] == 0
    assert result["pages_unmeasured"] == 1


def test_unparseable_stored_headers_are_unmeasured():
    con = _store([(1, "https://e.test/", 200, "text/html", None)])
    con.execute("UPDATE responses SET effective_headers_redacted_json = 'not json'")
    result = security_headers.evaluate(con)
    assert result["pages_unmeasured"] == 1
    assert _fired(result) == {}


def test_only_the_final_response_is_judged_when_a_url_has_several():
    con = _store([(1, "https://e.test/", 200, "text/html", [])])
    con.execute(
        "INSERT INTO responses (effective_url_id, purpose, response_headers_redacted_json,"
        " effective_headers_redacted_json) VALUES (1, 'page', '[]', ?)",
        (json.dumps(_ALL_PRESENT),),
    )
    assert _fired(security_headers.evaluate(con)) == {}


def test_evaluate_rejects_a_non_sqlite_connection():
    with pytest.raises(TypeError):
        security_headers.evaluate(object())


def test_missing_csp_fires_on_a_defect_and_stays_silent_on_clean_markup():
    defect = _store([(1, "https://e.test/", 200, "text/html", _without("content-security-policy"))])
    clean = _store([(1, "https://e.test/", 200, "text/html", _ALL_PRESENT)])
    assert "MISSING_CSP" in _fired(security_headers.evaluate(defect))
    assert "MISSING_CSP" not in _fired(security_headers.evaluate(clean))


def test_missing_x_content_type_options_fires_on_a_defect_and_stays_silent_on_clean():
    defect = _store([(1, "https://e.test/", 200, "text/html", _without("x-content-type-options"))])
    clean = _store([(1, "https://e.test/", 200, "text/html", _ALL_PRESENT)])
    assert "MISSING_X_CONTENT_TYPE_OPTIONS" in _fired(security_headers.evaluate(defect))
    assert "MISSING_X_CONTENT_TYPE_OPTIONS" not in _fired(security_headers.evaluate(clean))


def test_missing_x_frame_options_fires_on_a_defect_and_stays_silent_on_clean():
    defect = _store([(1, "https://e.test/", 200, "text/html", _without("x-frame-options"))])
    clean = _store([(1, "https://e.test/", 200, "text/html", _ALL_PRESENT)])
    assert "MISSING_X_FRAME_OPTIONS" in _fired(security_headers.evaluate(defect))
    assert "MISSING_X_FRAME_OPTIONS" not in _fired(security_headers.evaluate(clean))


def test_missing_referrer_policy_fires_on_a_defect_and_stays_silent_on_clean():
    defect = _store([(1, "https://e.test/", 200, "text/html", _without("referrer-policy"))])
    clean = _store([(1, "https://e.test/", 200, "text/html", _ALL_PRESENT)])
    assert "MISSING_REFERRER_POLICY" in _fired(security_headers.evaluate(defect))
    assert "MISSING_REFERRER_POLICY" not in _fired(security_headers.evaluate(clean))


def test_missing_hsts_fires_on_a_page_that_sends_no_strict_transport_security():
    headers = [h for h in _ALL_PRESENT if h[0] != "strict-transport-security"]
    con = _store([(1, "https://e.test/", 200, "text/html", headers)])
    assert _fired(security_headers.evaluate(con)) == {"MISSING_HSTS": ["https://e.test/"]}


def test_hsts_is_read_from_the_final_response_not_a_redirect_hop():
    con = _store([(1, "https://e.test/", 200, "text/html", _ALL_PRESENT)])
    con.execute("UPDATE responses SET response_headers_redacted_json = '[]'")
    assert _fired(security_headers.evaluate(con)) == {}


def test_duplicate_security_header_fires_on_a_repeat_and_stays_silent_on_clean():
    defect = _store(
        [(1, "https://e.test/", 200, "text/html", [*_ALL_PRESENT, ["x-frame-options", "DENY"]])]
    )
    clean = _store([(1, "https://e.test/", 200, "text/html", _ALL_PRESENT)])
    assert "DUPLICATE_SECURITY_HEADER" in _fired(security_headers.evaluate(defect))
    assert "DUPLICATE_SECURITY_HEADER" not in _fired(security_headers.evaluate(clean))
