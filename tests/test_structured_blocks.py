"""Per-URL JSON-LD block facts: position, state, graph findings and source JSON."""

from __future__ import annotations

import pytest

from seohead.checks.structured_blocks import SOURCE_CHARS, structured_blocks
from seohead.mcp.evidence_handlers import scan_structured_blocks
from seohead.storage import open_scan
from seohead.storage.native_scan import NativeScan
from tests.test_native_capture import _claim, _event, _renderer
from tests.test_scan_native import _metadata, _record, _runtime

PRODUCT = (
    '{"@context":"https://schema.org","@type":"Product","name":"Pump",'
    '"offers":{"@type":"Offer","price":"10","priceCurrency":"RUB"}}'
)


def _page(*blocks: str) -> str:
    scripts = "".join(f'<script type="application/ld+json">{block}</script>\n' for block in blocks)
    return f"<html><head>\n{scripts}</head><body></body></html>"


def test_absent_when_document_has_no_blocks():
    result = structured_blocks("<html><body>plain</body></html>")
    assert result["state"] == "absent"
    assert result["block_count"] == 0 and result["blocks"] == []


def test_block_reports_types_properties_source_and_line():
    result = structured_blocks(_page(PRODUCT))
    (block,) = result["blocks"]
    assert result["state"] == "valid"
    assert block["state"] == "valid" and block["error"] is None
    assert block["line"] == 2
    assert {"Product", "Offer"} <= set(block["types"])
    paths = {item["path"]: item["value"] for item in block["properties"]}
    assert paths["offers.price"] == "10"
    assert paths["name"] == "Pump"
    assert '"priceCurrency": "RUB"' in block["source_json"]
    assert block["source_truncated"] is False


def test_parse_error_is_kept_with_message_and_excerpt_not_dropped():
    result = structured_blocks(_page(PRODUCT, '{"@type": "Article", bad}'))
    assert result["state"] == "malformed"
    assert result["block_count"] == 2 and result["parse_error_count"] == 1
    broken = result["blocks"][1]
    assert broken["state"] == "parse_error"
    assert broken["error"] and broken["source_json"] is None
    assert broken["raw_excerpt"] == '{"@type": "Article", bad}'
    assert broken["line"] == 3


def test_empty_block_is_a_parse_error():
    (block,) = structured_blocks(_page("   "))["blocks"]
    assert block["state"] == "parse_error" and block["error"] == "block is empty"


def test_graph_warnings_and_errors_are_counted_per_block():
    # Missing required @id target is an error; a literal where an object is expected warns.
    broken_ref = '{"@type":"Product","name":"A","brand":{"@id":"https://x.test/#missing"}}'
    result = structured_blocks(_page(broken_ref))
    (block,) = result["blocks"]
    assert block["errors"] >= 1
    assert block["state"] == "errors"
    assert result["state"] == "errors"
    assert any(message["level"] == "error" for message in block["messages"])


def test_cross_block_id_reference_is_not_an_error():
    org = '{"@context":"https://schema.org","@type":"Organization","@id":"https://x.test/#org","name":"X"}'
    ref = '{"@context":"https://schema.org","@type":"WebSite","name":"X","publisher":{"@id":"https://x.test/#org"}}'
    result = structured_blocks(_page(org, ref))
    assert [block["errors"] for block in result["blocks"]] == [0, 0]


def test_template_clones_are_not_reported_as_page_blocks():
    html = (
        "<html><body><template>"
        '<script type="application/ld+json">{"@type":"Thing"}</script>'
        "</template></body></html>"
    )
    assert structured_blocks(html)["block_count"] == 0


def test_source_json_is_bounded_and_flagged():
    long_name = "x" * (SOURCE_CHARS + 10)
    (block,) = structured_blocks(_page('{"@type":"Thing","name":"' + long_name + '"}'))["blocks"]
    assert block["source_truncated"] is True
    assert len(block["source_json"]) == SOURCE_CHARS


def test_unavailable_when_body_is_not_retained():
    assert structured_blocks(None)["state"] == "unavailable"


def test_scan_reads_one_retained_document_and_reports_missing_urls(tmp_path):
    url = "https://example.test/"
    path = tmp_path / "scan.sqlite"
    body = _page(PRODUCT).encode("utf-8")
    with NativeScan.create(path, **_metadata()) as scan:
        lease = _claim(scan, url)
        scan.commit_page(lease, _record(url), captures=[_event(url, body=body)], runtime=_runtime())

    found = scan_structured_blocks(str(path), url)
    assert found["ok"] is True and found["state"] == "valid"
    assert found["blocks"][0]["types"]
    assert found["representation"] == "static"

    missing = scan_structured_blocks(str(path), "https://example.test/absent")
    assert missing["state"] == "not_found"

    con = open_scan(path, require_audit=False)
    try:
        assert con.execute("SELECT COUNT(*) FROM documents").fetchone()[0] == 1
    finally:
        con.close()


def test_parsed_json_retains_tree_without_validator_annotations():
    import json

    (block,) = structured_blocks(_page(PRODUCT))["blocks"]
    assert block["parsed_json"] == json.loads(PRODUCT)
    assert block["column"] == 0
    assert not any(key.startswith("_") for key in block["parsed_json"])
    broken = structured_blocks(_page("{bad}"))["blocks"][0]
    assert broken["parsed_json"] is None


def test_property_tree_limit_applies_to_wide_objects_and_lists():
    import json

    from seohead.checks.structured_blocks import MAX_PROPERTIES

    raw = json.dumps({"@type": "Thing", "extra": list(range(MAX_PROPERTIES + 50))})
    (block,) = structured_blocks(_page(raw))["blocks"]
    assert len(block["properties"]) == MAX_PROPERTIES
    assert block["property_limit_reached"] is True


def test_parsed_json_is_unavailable_when_source_exceeds_response_bound():
    raw = '{"@type":"Thing","name":"' + "x" * (SOURCE_CHARS + 10) + '"}'
    (block,) = structured_blocks(_page(raw))["blocks"]
    assert block["parsed_json"] is None
    assert block["source_truncated"] is True


def test_foreign_context_does_not_get_schema_org_clean_validation():
    (block,) = structured_blocks(
        _page('{"@context":"https://example.test/vocab","@type":"Thing"}')
    )["blocks"]
    assert block["state"] == "warnings"
    assert block["warnings"]


def test_non_entity_json_is_not_a_valid_structured_block():
    for raw in ["1", "null", "{}", "[]"]:
        (block,) = structured_blocks(_page(raw))["blocks"]
        assert block["state"] == "errors"
        assert block["errors"] == 1
        assert "no entity definitions" in block["messages"][0]["text"]


def test_padded_case_jsonld_type_uses_same_parser_and_served_selector():
    from seohead.checks.parser import parse_html

    html = '<script type=" APPLICATION/LD+JSON ">' + PRODUCT + "</script>"
    assert parse_html(html, "https://example.test/")["jsonld"]
    assert structured_blocks(html)["block_count"] == 1


def test_shared_relative_cross_format_identity_does_not_create_dangling_error():
    html = '<script type="application/ld+json">{"@type":"Article","headline":"A","author":{"@id":"#ada"}}</script><div itemscope itemtype="https://schema.org/Person" itemid="#ada"><span itemprop="name">Ada</span></div>'
    (block,) = structured_blocks(html, base_url="https://example.test/page")["blocks"]
    assert block["errors"] == 0
    assert block["parsed_json"]["author"]["@id"] == "#ada"


@pytest.mark.parametrize("representation", ["static", "rendered"])
def test_scan_normalizes_shared_ids_against_the_document_final_url(tmp_path, representation):
    url = "https://example.test/old"
    final_url = "https://example.test/new"
    html = '<script type="application/ld+json">{"@type":"Article","headline":"A","author":{"@id":"#ada"}}</script><div itemscope itemtype="https://schema.org/Person" itemid="https://example.test/new#ada"><span itemprop="name">Ada</span></div>'
    path = tmp_path / "redirected.sqlite"
    with NativeScan.create(path, **_metadata()) as scan:
        lease = _claim(scan, url)
        record = _record(url)
        record["final_url"] = final_url
        scan.commit_page(
            lease,
            record,
            captures=[_event(url, body=html.encode(), effective_url=final_url)],
            runtime=_runtime(),
        )
        if representation == "rendered":
            renderer = _renderer(url)
            renderer["navigation"]["final_url"] = final_url
            record["representation"] = "rendered"
            scan.commit_render(
                url,
                record,
                html=html,
                renderer=renderer,
                captured_at="2026-09-06T10:01:00Z",
            )
    (block,) = scan_structured_blocks(str(path), url, representation)["blocks"]
    assert block["errors"] == 0
    assert block["parsed_json"]["author"]["@id"] == "#ada"
