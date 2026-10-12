"""Per-site observer projections stay bounded, local and explicit about unfinished work."""

from __future__ import annotations

import os
from pathlib import Path

from seohead.mcp.mcp_server import build_server
from seohead.projects.observer import observe
from seohead.projects.runtime import prepare_project
from seohead.projects.workspace import create_project
from seohead.storage.native_scan import NativeScan
from tests.test_scan_history import _finished
from tests.test_scan_native import _metadata, _record, _runtime


def _row(path: Path, *, fingerprint: str = "first") -> dict:
    return {
        "path": str(path),
        "uuid": "synthetic",
        "lifecycle": "running",
        "finish_reason": "running",
        "crawl_partial": False,
        "corpus_partial": True,
        "evidence_revision": 1,
        "config_fingerprint": fingerprint,
        "format_version": "scan.v1",
        "source_kind": "native",
    }


def _prepare_with_competitors(root: Path) -> dict:
    create_project(
        root,
        "https://owner.example.test/",
        label="Owner",
        template_references=["ecommerce/product-card"],
        profile_references=["ecommerce/basic"],
    )

    def unavailable_crawl(**_kwargs):
        return {"ok": False, "error": "synthetic crawler unavailable"}

    return prepare_project(
        str(root),
        tools={"crawl_site": unavailable_crawl},
        competitors=[
            {
                "url": "https://competitor-one.example.test/",
                "source": "synthetic shortlist",
                "observed_at": None,
            },
            {
                "url": "https://competitor-two.example.test/",
                "source": "synthetic shortlist",
                "observed_at": None,
            },
        ],
    )


def test_observer_projects_scans_coverage_and_methods_per_declared_site(tmp_path):
    root = tmp_path / "owner"
    preparation = _prepare_with_competitors(root)["preparation"]
    first = root / preparation["competitors"][0]["directory"]
    second = root / preparation["competitors"][1]["directory"]
    _finished(root / "scans" / "owner.sqlite")
    _finished(first / "scans" / "competitor-one.sqlite")
    _finished(second / "scans" / "competitor-two.sqlite")
    owner_before = (root / "scans" / "owner.sqlite").read_bytes()
    competitor_before = (first / "scans" / "competitor-one.sqlite").read_bytes()

    snapshot = observe(str(root), scan_limit=1)

    assert snapshot["policy"] == {
        "ok": True,
        "applied": False,
        "revision": 0,
        "policy": {
            "approval_thresholds": {"pages": 1000, "requests": 3000, "seconds": 600},
            "quick_crawl": {"pages": 50, "requests": 150, "seconds": 60},
            "crawl_profile": None,
            "crawl_overrides": {},
            "competitor_limit": 5,
            "evidence_hash": {"max_bytes": 1073741824, "max_seconds": 5},
        },
        "operation_id": None,
    }
    assert snapshot["sites"]["total"] == 3
    assert snapshot["sites"]["scan_limit_per_site"] == 1
    owner, competitor_one, competitor_two = snapshot["sites"]["items"]
    assert owner["role"] == "primary"
    assert owner["site"]["target"] == "https://owner.example.test/"
    assert owner["candidate"] is None
    assert owner["scans"]["items"][0]["artifact"] == {
        "state": "available",
        "path": "scans/owner.sqlite",
    }
    assert owner["scans"]["items"][0]["evidence"]["state"] == "available"
    assert owner["coverage"]["state"] == "initialized"
    assert owner["methods"]["kinds"]["scenario"]["expected"] is None
    assert owner["methods"]["kinds"]["scenario"]["applicable"] > 0
    assert owner["methods"]["kinds"]["scenario"]["completed"] == 0
    assert competitor_one["role"] == competitor_two["role"] == "competitor"
    assert competitor_one["candidate"]["state"] == "candidate; audit not run"
    assert competitor_one["candidate"]["source"] == "synthetic shortlist"
    assert competitor_one["scans"]["items"][0]["artifact"]["path"].startswith("scans/")
    assert competitor_one["scans"]["items"][0]["evidence"]["state"] == "available"
    assert competitor_two["site"]["target"] == "https://competitor-two.example.test/"
    assert snapshot["scans"]["total"] == 1  # Legacy root-only projection remains compatible.
    assert all(method["site"]["target"] for method in snapshot["methods"])
    assert (root / "scans" / "owner.sqlite").read_bytes() == owner_before
    assert (first / "scans" / "competitor-one.sqlite").read_bytes() == competitor_before


def test_mcp_observer_exposes_the_same_bounded_site_projection(tmp_path):
    root = tmp_path / "owner"
    preparation = _prepare_with_competitors(root)["preparation"]
    competitor = root / preparation["competitors"][0]["directory"]
    _finished(root / "scans" / "owner.sqlite")
    _finished(competitor / "scans" / "competitor.sqlite")

    tool = build_server()._tool_manager.get_tool("seo_project_observe")
    snapshot = tool.fn(directory=str(root), scan_limit=1)

    assert snapshot["sites"]["scan_limit_per_site"] == 1
    assert {row["role"] for row in snapshot["sites"]["items"]} == {"primary", "competitor"}
    assert snapshot["policy"]["policy"]["quick_crawl"]["pages"] == 50
    assert all(item["scans"]["shown"] <= 1 for item in snapshot["sites"]["items"])


def test_observer_reuses_unchanged_scan_evidence_within_and_between_snapshots(
    tmp_path, monkeypatch
):
    """The legacy root row and site projection must not rematerialise one audit twice."""
    root = tmp_path / "owner"
    _prepare_with_competitors(root)
    _finished(root / "scans" / "owner.sqlite")
    import seohead.projects.observer as observer

    observer._EVIDENCE_CACHE.clear()
    calls = []

    def read(row):
        calls.append(row["path"])
        return {"state": "available", "value": len(calls)}

    monkeypatch.setattr(observer, "_read_scan_evidence", read)
    first = observer.observe(str(root), scan_limit=1)
    second = observer.observe(str(root), scan_limit=1)

    assert len(calls) == 1
    assert first["sites"]["items"][0]["scans"]["items"][0]["evidence"] == {
        "state": "available",
        "value": 1,
    }
    assert second["scans"]["items"][0]["evidence"] == {"state": "available", "value": 1}


def test_observer_evidence_cache_invalidates_source_wal_audit_and_config_state(
    tmp_path, monkeypatch
):
    """A dynamic scan cannot reuse a projection after any persisted state changes."""
    import seohead.projects.observer as observer

    source = tmp_path / "scan.sqlite"
    source.write_bytes(b"scan")
    row = _row(source)
    observer._EVIDENCE_CACHE.clear()
    calls = []

    def read(_row):
        calls.append(len(calls) + 1)
        return {"state": "available", "value": calls[-1]}

    monkeypatch.setattr(observer, "_read_scan_evidence", read)
    assert observer._scan_evidence(row)["value"] == 1
    assert observer._scan_evidence(row)["value"] == 1

    # A running crawl can change only the WAL; its identity must be in the key.
    wal = source.with_name(source.name + "-wal")
    wal.write_bytes(b"checkpoint")
    assert observer._scan_evidence(row)["value"] == 2

    from seohead.storage.audit_v2 import audit_v2_path

    audit = audit_v2_path(source)
    audit.write_bytes(b"audit")
    assert observer._scan_evidence(row)["value"] == 3

    # Metadata transitions are distinct even when a filesystem clock is coarse.
    assert observer._scan_evidence(_row(source, fingerprint="second"))["value"] == 4

    before = source.stat()
    os.utime(source, ns=(before.st_atime_ns, before.st_mtime_ns + 1))
    assert observer._scan_evidence(_row(source, fingerprint="second"))["value"] == 5


def test_retained_findings_cache_reuses_only_unchanged_artifact_state(tmp_path, monkeypatch):
    """Findings drill-down reuses a small current audit, then refreshes on a WAL change."""
    import seohead.projects.observer as observer

    source = tmp_path / "scan.sqlite"
    source.write_bytes(b"scan")
    row = _row(source)
    observer._FINDINGS_CACHE.clear()
    calls = []

    def read(_row):
        calls.append(len(calls) + 1)
        return {
            "issues": ({"id": str(calls[-1]), "severity": "warning"},),
            "skipped_checks": [],
            "schema_version": "audit.v1",
            "generated_at": "2026-10-04T00:00:00Z",
        }

    monkeypatch.setattr(observer, "_read_retained_findings", read)
    assert observer._retained_findings(row)["issues"][0]["id"] == "1"
    assert observer._retained_findings(row)["issues"][0]["id"] == "1"
    source.with_name(source.name + "-wal").write_bytes(b"new checkpoint")
    assert observer._retained_findings(row)["issues"][0]["id"] == "2"


def test_observer_keeps_active_collection_counters_when_audit_is_not_ready(tmp_path):
    """The collection is observable before the analyzer has written any findings."""
    root = tmp_path / "owner"
    create_project(root, "https://owner.example.test/")
    scan_path = root / "scans" / "active.sqlite"
    with NativeScan.create(scan_path, **_metadata()) as scan:
        scan.enqueue(
            [
                ("https://example.test/", 0),
                ("https://example.test/queued", 1),
            ]
        )
        lease = scan.claim(1)[0]
        record = _record(lease.url)
        record["crawl_depth"] = lease.depth
        scan.commit_page(lease, record, runtime=_runtime())

    evidence = observe(str(root), scan_limit=1)["scans"]["items"][0]["evidence"]

    assert evidence["state"] == "available"
    assert evidence["frontier"]["counts"] == {
        "queued": 1,
        "inflight": 0,
        "done": 1,
        "excluded": 0,
    }
    assert evidence["findings"] == {
        "state": "unavailable",
        "reason": evidence["findings"]["reason"],
        "total": None,
        "by_severity": {},
        "items": [],
        "truncated": False,
    }
    assert evidence["skipped_checks"] is None
