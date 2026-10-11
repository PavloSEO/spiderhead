"""Offline position checkpoint resume, occurrence identity and missing evidence."""

import hashlib

from seohead.storage import open_scan
from seohead.storage.link_positions import backfill_positions, position_summary
from seohead.storage.reanalysis import derived_scan
from seohead.storage.status import scan_status
from seohead.storage.url_links import url_links
from tests.test_scan_link_context import ROOT, _scan
from tests.test_scan_native import _metadata


def _derive(source, out):
    meta = _metadata()
    return derived_scan(
        source,
        out,
        producer_version="test",
        producer_revision="b" * 40,
        runtime_versions=meta["runtime_versions"],
    )


def test_backfill_fills_occurrences_without_mutating_source(tmp_path):
    source = _scan(
        tmp_path,
        "<nav class='breadcrumbs'><a href='/target'>Home</a></nav><main><a href='/target'>Body</a></main>",
    )
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    assert scan_status(str(source))["link_positions"]["state"] == "not_classified"
    assert all(
        item["position"] is None and item["position_status"] == "not_classified"
        for item in url_links(source, ROOT)["items"]
    )
    out = tmp_path / "positions.sqlite"
    with _derive(source, out) as (scan, _):
        first = backfill_positions(scan, max_documents=1)
        assert first["filled"] == 2
        second = backfill_positions(scan, max_documents=1)
        assert second["filled"] == 2 and second["documents"] == first["documents"]
        assert position_summary(scan.con)["state"] == "complete"
    assert hashlib.sha256(source.read_bytes()).hexdigest() == digest
    assert [item["position"] for item in url_links(out, ROOT)["items"]] == [
        "breadcrumbs",
        "content",
    ]
    assert scan_status(str(out))["link_positions"]["state"] == "complete"


def test_missing_body_and_changed_link_are_unavailable(tmp_path):
    for folder, kwargs in (
        ("missing", {"retain_body": False}),
        ("mismatch", {"stored_destination_override": ROOT + "wrong"}),
    ):
        source = _scan(tmp_path / folder, '<main><a href="/target">Body</a></main>', **kwargs)
        out = tmp_path / (folder + ".sqlite")
        with _derive(source, out) as (scan, _):
            result = backfill_positions(scan)
            assert result["filled"] == 0
            assert scan.con.execute("SELECT position FROM links").fetchone()[0] == ""


def test_checkpoint_resumes_in_a_new_derived_output(tmp_path):
    from seohead.storage.native_scan import NativeScan
    from tests.test_native_capture import _event
    from tests.test_scan_link_context import _batch
    from tests.test_scan_native import _record, _runtime

    metadata = _metadata()
    source = tmp_path / "source.sqlite"
    html = '<main><a href="/target">Body</a></main>'
    with NativeScan.create(source, **metadata) as scan:
        scan.enqueue([(ROOT, 0), (ROOT + "next", 0)])
        for url in (ROOT, ROOT + "next"):
            links = _batch(html, metadata["config"]).links
            for link in links:
                link["source"] = url
            record = _record(url)
            record["crawl_depth"] = 0
            scan.commit_page(
                scan.claim(1)[0],
                record,
                links=links,
                captures=[_event(url, body=html.encode())],
                runtime=_runtime(),
            )
        scan.finish_capture()
    first = tmp_path / "first.sqlite"
    with _derive(source, first) as (scan, _):
        checkpoint = backfill_positions(scan, max_documents=1)
        assert checkpoint["has_more"] and checkpoint["filled"] == 1
    second = tmp_path / "second.sqlite"
    with _derive(first, second) as (scan, _):
        resumed = backfill_positions(scan, max_documents=1)
        assert not resumed["has_more"] and resumed["filled"] == 2
        assert resumed["documents"] == 2
    with open_scan(second, require_audit=False) as con:
        assert {row[0] for row in con.execute("SELECT position FROM links")} == {"content"}


def test_position_only_handler_publishes_resumable_output_without_network(tmp_path, monkeypatch):
    import socket

    from seohead.mcp.reanalysis_handlers import reanalyze_scan

    source = _scan(tmp_path, '<main><a href="/target">Body</a></main>')
    digest = hashlib.sha256(source.read_bytes()).hexdigest()

    def forbidden(*args, **kwargs):
        raise AssertionError("offline backfill attempted network")

    monkeypatch.setattr(socket.socket, "connect", forbidden)
    monkeypatch.setattr(socket, "create_connection", forbidden)
    first = tmp_path / "first.sqlite"
    result = reanalyze_scan(
        str(source), str(first), producer_build="b" * 40, step="links.position", max_documents=1
    )
    assert result["link_positions"]["filled"] == 1
    assert result["link_positions"]["network"] == "disabled"
    assert first.exists()
    second = tmp_path / "second.sqlite"
    repeated = reanalyze_scan(
        str(first), str(second), producer_build="c" * 40, step="links.position", max_documents=1
    )
    assert repeated["link_positions"]["filled"] == 1
    assert repeated["link_positions"]["documents"] == 1
    assert not repeated["link_positions"]["has_more"]
    assert hashlib.sha256(source.read_bytes()).hexdigest() == digest


def test_full_reanalysis_preserves_capture_manifest_and_classifies_retained_links(tmp_path):
    from seohead.mcp.reanalysis_handlers import reanalyze_scan
    from seohead.storage import read_audit
    from tests.test_scan_reanalysis_integration import _source

    source = tmp_path / "captured.sqlite"
    settings = _source(source)
    assert settings["link_position"]["classify"] is False
    out = tmp_path / "derived.sqlite"
    result = reanalyze_scan(str(source), str(out), producer_build="b" * 40)
    assert result["audit_available"]
    with open_scan(out) as con:
        assert position_summary(con)["classified"] > 0
        import json

        assert (
            json.loads(con.execute("SELECT config_json FROM scan").fetchone()[0])["link_position"][
                "classify"
            ]
            is False
        )
    audit = read_audit(out)
    assert audit["run"]["crawl_config"]["link_position.classify"] is False
    assert audit["run"]["reanalysis"]["analysis_policy"] == {"link_position.classify": True}
