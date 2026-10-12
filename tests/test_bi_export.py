"""Offline acceptance for the typed BI projection (#834)."""

from __future__ import annotations

import asyncio
import csv
import hashlib
import json
import sqlite3
from pathlib import Path
from types import SimpleNamespace

import pytest

from seohead import cli
from seohead.crawl import sqlite_adapter
from seohead.data_sources.evidence_import import normalize_inline
from seohead.data_sources.evidence_join import evidence_compatibility, join_evidence
from seohead.data_sources.evidence_join_store import open_store
from seohead.data_sources.evidence_join_store import write as write_join_store
from seohead.mcp import handlers
from seohead.reports import bi as bi_report
from seohead.reports.bi import BIExportError, export_bi
from seohead.storage import open_scan, read_audit


def _offline_fetch(url: str):
    if url.endswith("/robots.txt"):
        body = "User-agent: *\nAllow: /\n"
        return SimpleNamespace(
            status_code=200,
            text=body,
            content=body.encode(),
            headers={"content-type": "text/plain"},
        )
    if url.endswith("/child"):
        body = (
            "<html><head><title>Child</title>"
            '<meta name="description" content="A child page description">'
            "</head><body><h1>Child page</h1></body></html>"
        )
    else:
        body = (
            "<html><head><title>=Home formula</title>"
            '<meta name="description" content="A home page description long enough for the fixture">'
            "</head><body><h1>Home page</h1>"
            '<a href="/child">=first link</a><a href="/child">second link</a>'
            "</body></html>"
        )
    return SimpleNamespace(
        status_code=200,
        text=body,
        content=body.encode(),
        headers={"content-type": "text/html; charset=utf-8"},
    )


def _crawl_with_audit(tmp_path: Path, monkeypatch, overrides=None) -> Path:
    original = sqlite_adapter.crawl_to_scan

    def offline_crawl(*args, **kwargs):
        kwargs["fetcher"] = _offline_fetch
        kwargs["sleeper"] = lambda _seconds: None
        return original(*args, **kwargs)

    monkeypatch.setattr(sqlite_adapter, "crawl_to_scan", offline_crawl)
    scan_path = tmp_path / "synthetic-scan.sqlite"
    crawl_overrides = {
        "speed.min_delay_seconds": 0,
        "limits.max_urls": 2,
        "limits.max_depth": 1,
    }
    crawl_overrides.update(overrides or {})
    result = handlers.crawl_site(
        url="https://example.test/",
        scan_out=str(scan_path),
        producer_build="a" * 40,
        overrides=crawl_overrides,
    )
    assert result["audit_available"] is True
    return scan_path


def _provider_manifest() -> dict:
    return {
        "format": "seohead.evidence-mapping.v1",
        "source": {
            "provider": "gsc",
            "operation": "search_analytics",
            "reporting_identity": "synthetic-property",
            "privacy": "supplied",
            "timezone": "America/Los_Angeles",
            "attribution": "last-click",
            "search_engine": "google",
            "search_type": "web",
        },
        "url": {"field": "url", "kind": "absolute"},
        "row_shape": "flat",
        "dimensions": ["query"],
        "metrics": [
            {"name": "clicks", "type": "number", "unit": "count"},
            {"name": "impressions", "type": "number", "unit": "count"},
        ],
        "period": {"start_date": "2026-01-01", "end_date": "2026-01-07"},
        "collection": {"state": "partial", "reason": "synthetic sampled fixture", "sampled": True},
        "duplicate_policy": "mark",
    }


def _provider_document() -> dict:
    return normalize_inline(
        [
            {
                "url": "https://example.test/",
                "query": "=formula query",
                "clicks": 0,
                "impressions": 100,
            },
            {
                "url": "https://example.test/",
                "query": "=formula query",
                "clicks": 3,
                "impressions": 120,
            },
            {
                "url": "https://outside.example.test/page",
                "query": "external only",
                "clicks": 4,
                "impressions": None,
            },
            {"url": "/relative", "query": "unkeyable", "clicks": 2, "impressions": 3},
        ],
        manifest=_provider_manifest(),
    )


def _csv_rows(package: Path, manifest: dict, dataset: str) -> list[dict[str, str]]:
    result = []
    for partition in manifest["datasets"][dataset]["partitions"]:
        with (package / partition["path"]).open(encoding="utf-8", newline="") as stream:
            result.extend(csv.DictReader(stream))
    return result


def _files(package: Path) -> dict[str, bytes]:
    return {
        path.relative_to(package).as_posix(): path.read_bytes()
        for path in sorted(package.rglob("*"))
        if path.is_file()
    }


def test_million_page_projection_factories_are_lazy_and_reiterable():
    produced = {"count": 0}

    def pages():
        for ordinal in range(1_000_000):
            produced["count"] += 1
            yield {
                "url": f"https://example.test/{ordinal}",
                "page_ordinal": ordinal,
                "status_code": 200,
                "content_type": "text/html",
                "size_bytes": 1,
                "response_time": 0.01,
                "title": "Page",
                "meta_description": "",
                "h1": "Page",
                "canonical": "",
                "word_count": 1,
                "crawl_depth": 1,
                "inlinks": 0,
                "unique_inlinks": 0,
                "outlinks": 0,
                "external_outlinks": 0,
                "representation": "static",
                "document_id": 1,
            }

    run = bi_report._RunInput(
        run_id="large-synthetic",
        source_kind="scan",
        source_schema="scan.v1",
        source_name="large.sqlite",
        source_sha256="a" * 64,
        source_bytes=1,
        run_metadata={
            "crawl_state": "complete",
            "crawl_reason": None,
            "capabilities": {"links": {"state": "complete"}},
        },
        pages_factory=pages,
        page_count=1_000_000,
        findings_factory=lambda: iter(()),
        finding_count=0,
        groups=[],
        links_factory=lambda: iter(()),
        links_source_state="complete",
        links_source_reason=None,
        coverage_rows=[],
        link_count=0,
    )
    first = next(run.pages_factory())
    assert first["url"] == "https://example.test/0"
    assert produced["count"] == 1, "page factory must not materialize the million-page population"
    first_cohorts = list(__import__("itertools").islice(bi_report._cohort_rows(run, [], None), 5))
    assert len(first_cohorts) == 5
    assert produced["count"] == 2, "cohort projection must consume only its current page"
    assert sum(1 for _ in run.pages_factory()) == run.page_count
    assert produced["count"] == 1_000_002, "the full source remains re-iterable without a list"


def test_scan_projection_conserves_pages_findings_links_and_provider_grain(tmp_path, monkeypatch):
    scan_path = _crawl_with_audit(tmp_path, monkeypatch)
    provider_path = tmp_path / "synthetic-evidence.json"
    provider_path.write_text(json.dumps(_provider_document()), encoding="utf-8")
    package = tmp_path / "bi-package"

    result = export_bi(
        scan=scan_path,
        provider_joins=[provider_path],
        out_dir=package,
        max_rows_per_file=3,
    )
    manifest = json.loads((package / "manifest.json").read_text(encoding="utf-8"))

    source_audit = read_audit(scan_path)
    with open_scan(scan_path) as con:
        source_pages = con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]
        source_links = con.execute("SELECT COUNT(*) FROM links").fetchone()[0]

    pages = _csv_rows(package, manifest, "pages")
    findings = _csv_rows(package, manifest, "findings")
    metrics = _csv_rows(package, manifest, "metrics")
    links = _csv_rows(package, manifest, "link_occurrences")
    coverage = _csv_rows(package, manifest, "coverage")

    assert len(pages) == source_pages == manifest["datasets"]["pages"]["row_count"] == 2
    assert len(findings) == len(source_audit["issues"])
    assert len(links) == source_links == 2
    assert len({row["link_occurrence_id"] for row in links}) == 2
    assert {row["link_ordinal"] for row in links} == {"0", "1"}
    assert len(metrics) == 4 * 2  # every provider source row remains at its own grain
    assert [part["rows"] for part in manifest["datasets"]["metrics"]["partitions"]] == [3, 3, 2]
    assert all(
        part["sha256"] == hashlib.sha256((package / part["path"]).read_bytes()).hexdigest()
        for part in manifest["datasets"]["metrics"]["partitions"]
    )
    assert all(
        part["bytes"] == (package / part["path"]).stat().st_size
        for part in manifest["datasets"]["metrics"]["partitions"]
    )

    clicks = [row for row in metrics if row["metric_name"] == "clicks"]
    zero = next(row for row in clicks if row["source_row_index"] == "0")
    assert zero["value_number"] == "0"
    assert zero["value_state"] == "measured"
    assert zero["period_start"] == "2026-01-01" and zero["period_end"] == "2026-01-07"
    assert zero["timezone"] == "America/Los_Angeles"
    assert zero["attribution"] == "last-click"
    assert zero["dimension_query"] == "'=formula query"

    suppressed = next(
        row
        for row in metrics
        if row["metric_name"] == "impressions" and row["source_row_index"] == "2"
    )
    assert suppressed["value_number"] == ""
    assert suppressed["value_state"] == "unavailable"
    assert suppressed["value_reason"] == "null"
    assert all(
        row["ambiguous_source_row"] == "true"
        for row in metrics
        if row["source_row_index"] in {"0", "1"}
    )
    assert manifest["datasets"]["pages"]["schema_version"] == "seohead.bi.pages.v1"
    assert manifest["datasets"]["pages"]["primary_key"] == ["run_id", "url_observation_id"]
    assert manifest["datasets"]["metrics"]["row_count"] == len(metrics)
    assert manifest["datasets"]["link_occurrences"]["row_count"] == source_links
    assert manifest["datasets"]["metrics"]["formula_safe_cells_prefixed"] > 0
    assert any(row["population"] == "crawl_only" and row["source_rows"] == "1" for row in coverage)
    assert any(
        row["population"] == "external_only" and row["source_rows"] == "1" for row in coverage
    )
    assert any(
        row["population"] == "unkeyable_rows" and row["source_rows"] == "1" for row in coverage
    )
    assert result["crawl_state"] in {"complete", "partial"}

    repeated = export_bi(
        scan=scan_path,
        provider_joins=[provider_path],
        out_dir=tmp_path / "bi-package-repeat",
        max_rows_per_file=3,
    )
    assert _files(package) == _files(Path(repeated["output_directory"]))


def test_bi_settings_manifest_uses_native_effective_config_without_secrets(tmp_path, monkeypatch):
    username_env = "SEOHEAD_BI_AUTH_USER"
    password_env = "SEOHEAD_BI_AUTH_PASSWORD"
    username = "synthetic-bi-user"
    password = "synthetic-bi-password"
    monkeypatch.setenv(username_env, username)
    monkeypatch.setenv(password_env, password)
    scan_path = _crawl_with_audit(
        tmp_path,
        monkeypatch,
        overrides={
            "scope.normalize_tracking_parameters": True,
            "http.auth": [
                {
                    "host": "example.test",
                    "scheme": "basic",
                    "username_env": f"env:{username_env}",
                    "password_env": f"env:{password_env}",
                }
            ],
            "http.auth_acknowledged": True,
            "rendering.actions": [{"action": "wait_ms", "ms": 25}],
            "rendering.presets": ["expand_tabs"],
            "rendering.artifacts.screenshot_mode": "first_screen",
        },
    )
    package = tmp_path / "bi-native-effective-config"

    export_bi(scan=scan_path, out_dir=package)

    manifest = json.loads((package / "manifest.json").read_text(encoding="utf-8"))
    settings = manifest["run"]["crawl_settings"]
    assert settings["scope.normalize_tracking_parameters"] is True
    assert settings["http.auth"] == [{"host": "example.test", "scheme": "basic"}]
    assert settings["http.auth_acknowledged"] is True
    assert settings["rendering.actions"] == [{"action": "wait_ms", "ms": 25}]
    assert settings["rendering.presets"] == ["expand_tabs"]
    # BI's settings manifest includes result-affecting options only.
    assert "rendering.artifacts.screenshot_mode" not in settings
    encoded = json.dumps(settings)
    assert username_env not in encoded and password_env not in encoded
    assert username not in encoded and password not in encoded


def test_audit_v2_page_overlay_is_keyed_not_positional(tmp_path, monkeypatch):
    """A valid audit.v2 collection may be ordered independently from the crawl."""
    from seohead.storage.audit_v2 import write_audit_v2

    scan_path = _crawl_with_audit(tmp_path, monkeypatch)
    document = read_audit(scan_path)
    with open_scan(scan_path) as con:
        scan = dict(con.execute("SELECT * FROM scan WHERE singleton=1").fetchone())
    binding = {
        "scan_uuid": scan["scan_uuid"],
        "evidence_revision": scan["evidence_revision"],
        "analyzer_version": scan["writer_version"],
        "analyzer_revision": scan["writer_revision"],
    }
    expected = {page["url"]: page.get("indexability") for page in document["pages"]}
    write_audit_v2(
        scan_path,
        document,
        {
            "/issues": document["issues"],
            "/pages": reversed(document["pages"]),
            "/groups": document["groups"],
        },
        binding,
    )

    package = tmp_path / "shuffled-audit-v2-bi"
    export_bi(scan=scan_path, out_dir=package)
    manifest = json.loads((package / "manifest.json").read_text(encoding="utf-8"))
    pages = _csv_rows(package, manifest, "pages")
    assert {row["url"]: row["indexability"] or None for row in pages} == expected


def test_audit_v2_finding_segment_selection_matches_saved_view_engine(tmp_path, monkeypatch):
    """Streamed audit.v2 selection keeps project-view analysis segment semantics."""
    from seohead.crawl.settings import manifest as settings_manifest
    from seohead.projects.finding_views import _segment_definitions
    from seohead.reports.bi_destinations import filter_package
    from seohead.sf.core.segments import assign_segments
    from seohead.storage import _dump, _sha
    from seohead.storage.audit_v2 import write_audit_v2
    from seohead.storage.native_scan import (
        _config_extension_context,
        _frozen_header_config,
        crawl_config_fingerprint,
        read_effective_config,
    )

    scan_path = _crawl_with_audit(tmp_path, monkeypatch)
    document = read_audit(scan_path)
    definitions = [
        {"name": "after-child", "rules": [{"op": "segment", "value": "child"}]},
        {"name": "child", "rules": [{"op": "contains", "field": "url", "value": "/child"}]},
    ]
    with sqlite3.connect(scan_path) as con:
        con.row_factory = sqlite3.Row
        config = read_effective_config(con)
        config["analysis"]["segments"] = definitions
        header_config = _frozen_header_config(config)
        extension = _config_extension_context(config)
        document["run"]["crawl_config"] = settings_manifest(config)
        document_json = _dump(document)
        con.execute(
            "UPDATE scan SET config_json=?,config_fingerprint=? WHERE singleton=1",
            (_dump(header_config), crawl_config_fingerprint(header_config)),
        )
        con.execute(
            "UPDATE context_items SET payload_json=? WHERE kind='native_config_extension' AND item_key='run'",
            (extension["payload_json"],),
        )
        con.execute(
            "UPDATE audit SET document_json=?,sha256=? WHERE singleton=1",
            (document_json, _sha(document_json)),
        )
        metadata = con.execute(
            "SELECT scan_uuid,evidence_revision,writer_version,writer_revision FROM scan WHERE singleton=1"
        ).fetchone()
    write_audit_v2(
        scan_path,
        document,
        {
            "/issues": document["issues"],
            "/pages": reversed(document["pages"]),
            "/groups": document["groups"],
        },
        {
            "scan_uuid": metadata[0],
            "evidence_revision": metadata[1],
            "analyzer_version": metadata[2],
            "analyzer_revision": metadata[3],
        },
    )
    primary = assign_segments(
        document["pages"],
        _segment_definitions({"run": {"crawl_config": {"analysis.segments": definitions}}}),
    )["primary"]
    expected_urls = {
        item["target_url"]
        for item in document["issues"]
        if primary.get(item.get("target_url")) == "child"
    }
    assert expected_urls

    package = tmp_path / "audit-v2-bi"
    export_bi(scan=scan_path, out_dir=package)
    selected = filter_package(
        package,
        dataset="findings",
        out_dir=tmp_path / "audit-v2-selected",
        where={"segment": ["child"]},
        columns=["url", "check_id"],
    )
    selected_urls = set()
    for part in selected["partitions"]:
        with (Path(selected["output_directory"]) / part["path"]).open(newline="") as stream:
            selected_urls.update(row["url"] for row in csv.DictReader(stream))
    assert selected_urls == expected_urls


def test_segment_projection_refuses_absent_fields_but_keeps_explicit_null(tmp_path):
    from seohead.reports.bi_index import PrimarySegmentIndex, projection_index
    from seohead.sf.core.segments import assign_segments

    definitions = [
        {"name": "has-title", "rules": [{"op": "eq", "field": "title", "value": "Home"}]}
    ]
    assert (
        assign_segments([{"url": "https://example.test/", "title": "Home"}], definitions)[
            "primary"
        ]["https://example.test/"]
        == "has-title"
    )
    with projection_index(tmp_path, 1024 * 1024) as con:
        index = PrimarySegmentIndex(con, definitions)
        index.build([{"url": "https://example.test/"}])
        with pytest.raises(BIExportError, match="retained page fields are missing: title"):
            index.primary("https://example.test/")
    with projection_index(tmp_path, 1024 * 1024) as con:
        index = PrimarySegmentIndex(con, definitions)
        index.build([{"url": "https://example.test/", "title": None}])
        assert index.primary("https://example.test/") == "default"


def test_cursor_backed_join_store_preserves_grain_without_matched_url_lists(tmp_path, monkeypatch):
    scan_path = _crawl_with_audit(tmp_path, monkeypatch)
    store_path = tmp_path / "evidence-join.sqlite"
    metadata = write_join_store(scan_path, _provider_document(), store_path)
    store = open_store(store_path)

    observations = list(store.iter_observations())
    assert len(observations) == metadata["summary"]["rows"] == 4
    assert metadata["allocation_policy"]["domain_totals"] == "never_allocate_to_urls"
    assert (
        sum(item["matched_page_count"] for item in observations)
        == metadata["summary"]["candidate_pairs"]
    )
    assert [item["url"] for item in store.iter_matches()] == ["https://example.test/"] * 2
    assert list(store.iter_observations()) == observations, "store cursors must be re-iterable"

    package = tmp_path / "cursor-backed-bi"
    export_bi(scan=scan_path, provider_joins=[store_path], out_dir=package, max_rows_per_file=3)
    manifest = json.loads((package / "manifest.json").read_text(encoding="utf-8"))
    metrics = _csv_rows(package, manifest, "metrics")
    assert len(metrics) == len(observations) * 2
    matched = next(row for row in metrics if row["population_state"] == "matched")
    provenance = json.loads(matched["matched_page_urls_json"])
    assert provenance["state"] == "cursor_backed"
    assert matched["matched_page_count"] == "1"


def test_cli_reaches_durable_join_store_for_a_large_scan_path(tmp_path, monkeypatch, capsys):
    """The public CLI uses the same large-scan branch as local MCP."""
    scan_path = _crawl_with_audit(tmp_path, monkeypatch)
    monkeypatch.setattr(handlers, "LARGE_EVIDENCE_JOIN_SCAN_PAGES", 1)
    artifacts = tmp_path / "large-join-artifacts"

    assert (
        cli.main(
            [
                "evidence-join",
                "--scan",
                str(scan_path),
                "--evidence",
                json.dumps(_provider_document()),
                "--out-dir",
                str(artifacts),
            ]
        )
        == 0
    )
    result = json.loads(capsys.readouterr().out)
    assert result["join_store"]["format"] == "seohead.evidence-join-sqlite.v1"
    assert len(list(artifacts.glob("*.sqlite"))) == 1


def test_mcp_reaches_durable_join_store_for_a_large_scan_path(tmp_path, monkeypatch):
    pytest.importorskip("mcp")
    from seohead.mcp.mcp_server import build_server

    scan_path = _crawl_with_audit(tmp_path, monkeypatch)
    monkeypatch.setattr(handlers, "LARGE_EVIDENCE_JOIN_SCAN_PAGES", 1)
    artifacts = tmp_path / "mcp-large-join-artifacts"

    asyncio.run(
        build_server().call_tool(
            "seo_evidence_join",
            {
                "scan": str(scan_path),
                "evidence": _provider_document(),
                "out_dir": str(artifacts),
            },
        )
    )
    assert len(list(artifacts.glob("*.sqlite"))) == 1


def test_site_audit_nulls_and_formula_cells_are_explicitly_safe(tmp_path):
    audit = {
        "schema": "seohead.site-audit/1",
        "url": "https://example.test/",
        "domain": "example.test",
        "generated_at": "2026-01-02T03:04:05Z",
        "site": {},
        "pages": [
            {
                "url": "https://example.test/",
                "status_code": 200,
                "title": "=SUM(A1:A2)",
                "description": "Synthetic description",
                "h1": "Heading",
                "word_count": 0,
            }
        ],
        "findings": [
            {
                "source": "synthetic",
                "severity": "notice",
                "url": "https://example.test/",
                "text": "=formula finding",
            }
        ],
        "summary": {
            "pages_checked": 1,
            "findings_total": 1,
            "findings_by_severity": {"critical": 0, "warning": 0, "notice": 1},
            "tools_run": [],
            "tools_failed": [],
        },
    }
    package = tmp_path / "site-audit-bi"
    export_bi(audit=audit, out_dir=package)
    manifest = json.loads((package / "manifest.json").read_text(encoding="utf-8"))
    page = _csv_rows(package, manifest, "pages")[0]
    finding = _csv_rows(package, manifest, "findings")[0]

    assert page["title"] == "'=SUM(A1:A2)"
    assert page["title_state"] == "measured"
    assert page["word_count"] == "0" and page["word_count_state"] == "measured"
    assert finding["message"] == "'=formula finding"
    assert manifest["datasets"]["pages"]["formula_safe_cells_prefixed"] >= 1
    assert manifest["datasets"]["findings"]["formula_safe_cells_prefixed"] >= 1
    assert manifest["datasets"]["metrics"]["state"] == "not_configured"
    assert manifest["datasets"]["link_occurrences"]["state"] == "unavailable"


def test_legacy_untyped_provider_join_is_counted_as_unsupported(tmp_path):
    audit_path = Path("tests/doc_fixtures/run/audit.json")
    legacy = tmp_path / "legacy-provider-join.json"
    legacy.write_text(
        json.dumps(
            {
                "format": "seohead.provider-join.v1",
                "join": {"summary": {"rows": 7}},
            }
        ),
        encoding="utf-8",
    )
    package = tmp_path / "legacy-bi"

    export_bi(audit=audit_path, provider_joins=[legacy], out_dir=package)

    manifest = json.loads((package / "manifest.json").read_text(encoding="utf-8"))
    metric_sources = [
        row
        for row in _csv_rows(package, manifest, "coverage")
        if row["dataset"] == "metrics" and row["population"] == "provider_source"
    ]
    assert len(metric_sources) == 1
    assert metric_sources[0]["state"] == "unsupported"
    assert metric_sources[0]["source_rows"] == "7"
    assert "does not declare typed metric names" in metric_sources[0]["reason"]
    assert manifest["datasets"]["metrics"]["state"] == "unavailable"
    assert manifest["datasets"]["metrics"]["row_count"] == 0


def test_saved_issue781_join_artifact_projects_without_rejoining_or_summing(tmp_path):
    doc = _provider_document()
    audit = {
        "schema": "seohead.site-audit/1",
        "url": "https://example.test/",
        "domain": "example.test",
        "generated_at": "2026-01-02T03:04:05Z",
        "site": {},
        "pages": [{"url": "https://example.test/", "title": "Synthetic"}],
        "findings": [],
        "summary": {
            "pages_checked": 1,
            "findings_total": 0,
            "findings_by_severity": {"critical": 0, "warning": 0, "notice": 0},
            "tools_run": [],
            "tools_failed": [],
        },
    }
    join = join_evidence(audit["pages"], doc, crawl={"source": "audit"})
    compatibility = evidence_compatibility(doc, doc)
    artifact = tmp_path / "saved-evidence-join.json"
    artifact.write_text(
        json.dumps({"join": join, "compatibility": compatibility}), encoding="utf-8"
    )
    package = tmp_path / "saved-join-bi"

    export_bi(audit=audit, provider_joins=[artifact], out_dir=package)

    manifest = json.loads((package / "manifest.json").read_text(encoding="utf-8"))
    metrics = _csv_rows(package, manifest, "metrics")
    populations = [row["population_state"] for row in metrics]
    assert len(metrics) == 8
    assert populations.count("matched") == 4
    assert populations.count("external_only") == 2
    assert populations.count("unkeyable") == 2
    assert manifest["provider_sources"][0]["compatibility"]["verdict"] == "compatible"
    assert len({row["metric_observation_id"] for row in metrics}) == len(metrics)


def test_hard_output_limit_fails_without_publishing_a_partial_package(tmp_path):
    audit_path = Path("tests/doc_fixtures/run/audit.json")
    destination = tmp_path / "too-small"
    with pytest.raises(BIExportError, match=r"bound|partition byte limit|row exceeds"):
        export_bi(
            audit=audit_path,
            out_dir=destination,
            max_rows_per_file=10,
            max_bytes_per_file=1024,
            max_output_bytes=1024,
        )
    assert not destination.exists()


def test_cli_and_mcp_share_the_same_offline_package_writer(tmp_path, capsys):
    audit = {
        "schema": "seohead.site-audit/1",
        "url": "https://example.test/",
        "domain": "example.test",
        "generated_at": "2026-01-02T03:04:05Z",
        "site": {},
        "pages": [{"url": "https://example.test/", "title": "Synthetic"}],
        "findings": [],
        "summary": {
            "pages_checked": 1,
            "findings_total": 0,
            "findings_by_severity": {"critical": 0, "warning": 0, "notice": 0},
            "tools_run": [],
            "tools_failed": [],
        },
    }
    audit_path = tmp_path / "audit.json"
    audit_path.write_text(json.dumps(audit), encoding="utf-8")
    cli_dir = tmp_path / "cli-bi"
    mcp_dir = tmp_path / "mcp-bi"
    assert cli.main(["bi-export", "--audit", str(audit_path), "--out-dir", str(cli_dir)]) == 0
    assert json.loads(capsys.readouterr().out)["ok"] is True

    tool = next(
        item
        for item in __import__("seohead.mcp.mcp_server", fromlist=["build_server"])
        .build_server()
        ._tool_manager.list_tools()
        if item.name == "seo_bi_export"
    )
    response = asyncio.run(tool.run({"audit": str(audit_path), "out_dir": str(mcp_dir)}))
    assert response["format"] == "seohead.bi-export-result.v1"
    assert _files(cli_dir) == _files(mcp_dir)


def test_companion_audit_findings_are_projected_without_reading_empty_inline_slot(
    tmp_path, monkeypatch
):
    from seohead.storage.audit_v2 import write_audit_v2

    scan_path = _crawl_with_audit(tmp_path, monkeypatch)
    document = read_audit(scan_path)
    with open_scan(scan_path) as con:
        metadata = dict(con.execute("SELECT * FROM scan").fetchone())
    issues, pages, groups = (document.pop(key) for key in ("issues", "pages", "groups"))
    document.update(issues=[], pages=[], groups=[])
    write_audit_v2(
        scan_path,
        document,
        {"/issues": issues, "/pages": pages, "/groups": groups},
        {
            "scan_uuid": metadata["scan_uuid"],
            "evidence_revision": metadata["evidence_revision"],
            "analyzer_version": metadata["writer_version"],
            "analyzer_revision": metadata["writer_revision"],
        },
    )
    monkeypatch.setattr(
        "seohead.storage.audit_v2.AuditV2Reader.materialize_legacy",
        lambda *_args, **_kwargs: pytest.fail(
            "BI must stream audit.v2 findings, not materialize legacy JSON"
        ),
    )
    package = tmp_path / "companion-bi"
    export_bi(scan=scan_path, out_dir=package)
    manifest = json.loads((package / "manifest.json").read_text())
    assert len(_csv_rows(package, manifest, "findings")) == len(issues)
    assert manifest["run"]["audit_available"] is True


def test_cohorts_keep_zero_quadrants_separate_from_unconfigured_provider_evidence(tmp_path):
    audit = {
        "schema": "seohead.site-audit/1",
        "url": "https://example.test/",
        "domain": "example.test",
        "generated_at": "2026-01-02T03:04:05Z",
        "site": {},
        "pages": [{"url": "https://example.test/", "status_code": 200, "crawl_depth": 3}],
        "findings": [],
        "summary": {
            "pages_checked": 1,
            "findings_total": 0,
            "findings_by_severity": {"critical": 0, "warning": 0, "notice": 0},
            "tools_run": [],
            "tools_failed": [],
        },
    }

    def evidence(provider, metric, value, *, timezone="UTC"):
        return normalize_inline(
            [{"url": "https://example.test/", metric: value}],
            manifest={
                "format": "seohead.evidence-mapping.v1",
                "source": {
                    "provider": provider,
                    "operation": "synthetic",
                    "privacy": "supplied",
                    "timezone": timezone,
                    "reporting_identity": f"synthetic-{provider}",
                    "attribution": "last-click",
                },
                "url": {"field": "url", "kind": "absolute"},
                "row_shape": "flat",
                "dimensions": [],
                "metrics": [{"name": metric, "type": "number", "unit": "count"}],
                "period": {"start_date": "2026-01-01", "end_date": "2026-01-07"},
                "collection": {
                    "state": "complete",
                    "sampled": False,
                    "thresholded": False,
                    "truncated": False,
                },
            },
        )

    gsc = tmp_path / "gsc.json"
    ga4 = tmp_path / "ga4.json"
    gsc.write_text(json.dumps(evidence("gsc", "clicks", 0)), encoding="utf-8")
    ga4.write_text(json.dumps(evidence("ga4", "sessions", 2)), encoding="utf-8")
    package = tmp_path / "cohort-bi"

    export_bi(
        audit=audit,
        provider_joins=[gsc, ga4],
        out_dir=package,
        search_metric="clicks",
    )

    manifest = json.loads((package / "manifest.json").read_text())
    cohorts = _csv_rows(package, manifest, "cohorts")
    quadrant = next(row for row in cohorts if row["cohort_id"] == "search_visibility_vs_sessions")
    assert quadrant["membership"] == "member"
    assert quadrant["value_label"] == "zero_search_positive_sessions"
    assert quadrant["search_value"] == "0"
    assert quadrant["sessions_value"] == "2"
    assert quadrant["period_start"] == "2026-01-01"
    assert quadrant["timezone"] == "UTC"
    assert manifest["datasets"]["cohorts"]["row_count"] == 5

    no_provider = tmp_path / "cohort-no-provider"
    export_bi(audit=audit, out_dir=no_provider)
    missing = next(
        row
        for row in _csv_rows(
            no_provider, json.loads((no_provider / "manifest.json").read_text()), "cohorts"
        )
        if row["cohort_id"] == "search_visibility_vs_sessions"
    )
    assert missing["membership"] == "unclassified"
    assert missing["state"] == "not_configured"
    inlinks = next(
        row
        for row in _csv_rows(
            no_provider, json.loads((no_provider / "manifest.json").read_text()), "cohorts"
        )
        if row["cohort_id"] == "observed_unique_inlink_share"
    )
    assert inlinks["state"] == "unavailable"

    mismatched = tmp_path / "ga4-mismatched.json"
    mismatched.write_text(
        json.dumps(evidence("ga4", "sessions", 2, timezone="America/New_York")),
        encoding="utf-8",
    )
    incompatible_package = tmp_path / "cohort-incompatible"
    export_bi(
        audit=audit,
        provider_joins=[gsc, mismatched],
        out_dir=incompatible_package,
        search_metric="clicks",
    )
    incompatible = next(
        row
        for row in _csv_rows(
            incompatible_package,
            json.loads((incompatible_package / "manifest.json").read_text()),
            "cohorts",
        )
        if row["cohort_id"] == "search_visibility_vs_sessions"
    )
    assert incompatible["membership"] == "unclassified"
    assert incompatible["state"] == "incomplete"


def test_cohort_quadrants_refuse_normalized_url_key_collisions(tmp_path):
    """A provider row may not be attributed to several raw crawl URLs."""
    audit = {
        "schema": "seohead.site-audit/1",
        "url": "https://example.test/",
        "domain": "example.test",
        "generated_at": "2026-01-02T03:04:05Z",
        "site": {},
        # These are distinct retained source URLs but share external_join.v1's
        # normalized key.  A traffic row must not become two URL cohorts.
        "pages": [
            {"url": "https://example.test/a", "status_code": 200},
            {"url": "https://EXAMPLE.test/a", "status_code": 200},
        ],
        "findings": [],
        "summary": {
            "pages_checked": 2,
            "findings_total": 0,
            "findings_by_severity": {"critical": 0, "warning": 0, "notice": 0},
            "tools_run": [],
            "tools_failed": [],
        },
    }

    def evidence(provider, metric, value):
        return normalize_inline(
            [{"url": "https://example.test/a", metric: value}],
            manifest={
                "format": "seohead.evidence-mapping.v1",
                "source": {
                    "provider": provider,
                    "operation": "synthetic",
                    "privacy": "supplied",
                    "timezone": "UTC",
                },
                "url": {"field": "url", "kind": "absolute"},
                "row_shape": "flat",
                "dimensions": [],
                "metrics": [{"name": metric, "type": "number", "unit": "count"}],
                "period": {"start_date": "2026-01-01", "end_date": "2026-01-07"},
                "collection": {"state": "complete"},
            },
        )

    gsc, ga4 = tmp_path / "gsc.json", tmp_path / "ga4.json"
    gsc.write_text(json.dumps(evidence("gsc", "clicks", 0)), encoding="utf-8")
    ga4.write_text(json.dumps(evidence("ga4", "sessions", 2)), encoding="utf-8")
    package = tmp_path / "collision-bi"
    export_bi(
        audit=audit,
        provider_joins=[gsc, ga4],
        out_dir=package,
        search_metric="clicks",
    )

    manifest = json.loads((package / "manifest.json").read_text())
    quadrants = [
        row
        for row in _csv_rows(package, manifest, "cohorts")
        if row["cohort_id"] == "search_visibility_vs_sessions"
    ]
    assert len(quadrants) == 2
    assert all(row["membership"] == "unclassified" for row in quadrants)
    assert all(row["state"] == "incomplete" for row in quadrants)
    assert all("matches multiple retained crawl URLs" in row["reason"] for row in quadrants)
    assert all(row["value_label"] == "" for row in quadrants)


def test_quadrant_pairing_refuses_cursor_backed_multi_page_match(tmp_path):
    """The durable store's matched_page_count has the same ambiguity gate."""

    def observation(metric, value):
        return {
            "row": {"dimensions": {}},
            "metric": {"name": metric, "unit": "count"},
            "entry": {"state": "measured", "value": value},
            "population": "matched",
            "url": {"state": "keyed", "normalized": "https://example.test/a"},
            "matched_page_count": 2,
        }

    def source(provider, metric, value):
        return (
            SimpleNamespace(source_id=provider),
            bi_report._ObservationStream(lambda: iter((observation(metric, value),)), 1),
            {
                "evidence": {
                    "provider": provider,
                    "collection": {"state": "complete"},
                    "period": {"start_date": "2026-01-01", "end_date": "2026-01-07"},
                    "timezone": "UTC",
                }
            },
        )

    from seohead.reports.bi_index import projection_index

    with projection_index(tmp_path, 1024 * 1024) as con:
        pairs, reason, blocked = bi_report._quadrant_candidates(
            [source("gsc", "clicks", 0), source("ga4", "sessions", 2)], "clicks", con=con
        )
        assert pairs.get("https://example.test/a") is None
        assert reason is None
        assert blocked.get("https://example.test/a") == (
            "normalized URL key matches multiple retained crawl URLs; "
            "provider traffic cannot be attributed to one URL observation"
        )


def test_bi_export_cli_reaches_the_split_xlsx_consumer(tmp_path, capsys):
    package = tmp_path / "bi"
    workbook = tmp_path / "pages.xlsx"
    assert (
        cli.main(
            [
                "bi-export",
                "--audit",
                "docs/examples/audit.json",
                "--out-dir",
                str(package),
                "--xlsx-out",
                str(workbook),
                "--xlsx-dataset",
                "pages",
                "--xlsx-max-rows-per-sheet",
                "2",
            ]
        )
        == 0
    )
    result = json.loads(capsys.readouterr().out)
    assert result["xlsx"]["dataset"] == "pages"
    assert workbook.is_file()


def test_scan_hash_budget_refuses_before_writing_a_package(tmp_path, monkeypatch):
    scan_path = _crawl_with_audit(tmp_path, monkeypatch)
    with pytest.raises(BIExportError, match="scan exceeds"):
        export_bi(scan=scan_path, out_dir=tmp_path / "bounded", max_scan_bytes=1)
    assert not (tmp_path / "bounded").exists()


def test_retained_occurrence_share_and_typed_provider_dimensions_reach_destinations(
    tmp_path, monkeypatch
):
    from seohead.reports.bi_destinations import bigquery_plan, filter_package, sheets_plan

    scan = _crawl_with_audit(tmp_path, monkeypatch)
    provider = tmp_path / "provider.json"
    provider.write_text(json.dumps(_provider_document()))
    package = tmp_path / "package"
    export_bi(scan=scan, provider_joins=[provider], out_dir=package)
    manifest = json.loads((package / "manifest.json").read_text())
    shares = {
        row["url"]: row
        for row in _csv_rows(package, manifest, "cohorts")
        if row["cohort_id"] == "observed_unique_inlink_share"
    }
    assert shares["https://example.test/child"]["numerator"] == "1"
    assert shares["https://example.test/child"]["denominator"] == "2"
    assert shares["https://example.test/child"]["value_number"] == "0.5"
    assert sheets_plan(package)["state"] == "ready"
    assert bigquery_plan(package, dataset="synthetic")["network"] is False
    filtered = tmp_path / "metrics"
    filter_package(
        package,
        dataset="metrics",
        out_dir=filtered,
        columns=["metric_name", "value_number", "dimension_query"],
    )
    assert sheets_plan(filtered)["worksheets"][0]["columns"] == 3


def test_native_scan_finding_projection_preserves_exact_urls_messages_and_selected_view(
    tmp_path, monkeypatch
):
    from seohead.reports.bi_destinations import filter_package

    scan = _crawl_with_audit(tmp_path, monkeypatch)
    original = read_audit(scan)["issues"]
    package = tmp_path / "package"
    export_bi(scan=scan, out_dir=package)
    manifest = json.loads((package / "manifest.json").read_text())
    findings = _csv_rows(package, manifest, "findings")
    assert len(findings) == len(original)
    for source, row in zip(original, findings, strict=True):
        assert row["url"] == (source.get("target_url") or "")
        assert row["message"] == (source.get("message") or "")
        assert row["finding_kind"] == "audit_finding"
        assert json.loads(row["source_finding_json"]) == source
    expected = [row for row in original if row.get("target_url") == "https://example.test/child"]
    assert expected
    selected = filter_package(
        package,
        dataset="findings",
        out_dir=tmp_path / "selected",
        where={"url": ["https://example.test/child"]},
    )
    assert selected["row_count"] == len(expected)
    assert selected["conservation"]["source_rows"] == len(original)
