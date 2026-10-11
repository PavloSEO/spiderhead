"""Remediation ledger schema, identity and ingest tests (issue #788)."""

from __future__ import annotations

import hashlib
import json
import sqlite3
import uuid
from pathlib import Path

import pytest

from seohead import __version__
from seohead.crawl.collect import PageRecord
from seohead.crawl.settings import manifest
from seohead.projects.workspace import create_project
from seohead.storage import ScanError, import_run, open_scan
from seohead.storage.ledger import (
    APPLICATION_ID,
    LedgerError,
    canonical_url,
    create_ledger,
    finding_key,
    ingest_scan,
    ledger_summary,
    note_source_missing,
    occurrence_key,
    open_ledger,
    read_cases,
    record_verification,
    remediation_report,
    remediation_summary,
    transition_occurrence,
    write_remediation_report,
)
from seohead.storage.native_scan import NativeScan
from tests.test_scan_native import _metadata, _runtime

BUILD = "25fd2ed032a31d63c5811722619e35c14b631476"
SITE = "https://example.test/"
HOME = "https://example.test"
A = "https://example.test/a"
B = "https://example.test/b"
DEAD1 = "https://example.test/dead-1"
DEAD2 = "https://example.test/dead-2"
PAGES = [HOME + "/", A, B]


def _file_sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _user_version(path: Path) -> int:
    con = sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True)
    try:
        return con.execute("PRAGMA user_version").fetchone()[0]
    finally:
        con.close()


def _project(tmp_path: Path) -> Path:
    directory = tmp_path / "project"
    create_project(directory, SITE)
    return directory


def _ledger(tmp_path: Path, *, project_dir: Path | None = None) -> Path:
    return create_ledger(
        tmp_path / "ledger.sqlite",
        project_dir=project_dir or _project(tmp_path),
        producer_build=BUILD,
    )


def _issue(ordinal, check, *, target=None, count=1, locations=None, **fields):
    issue = {
        "id": ordinal,
        "check": check,
        "severity": fields.pop("severity", "warning"),
        "source": "native",
        "message": fields.pop("message", "synthetic finding"),
        "occurrences_count": count,
    }
    if target is not None:
        issue["target_url"] = target
    if locations is not None:
        issue["locations"] = locations
    issue.update(fields)
    return issue


def _document(scan, meta, *, issues, page_urls, groups=None, generated_at, partial):
    return {
        "schema_version": "2.0",
        "tool": {"name": "seohead", "version": meta["writer_version"]},
        "run": {
            "input_mode": "crawl",
            "source": meta["start_url"],
            "generated_at": generated_at,
            "crawl_config": manifest(meta["config"]),
            "crawl_partial": partial,
            "scan_uuid": scan.con.execute("SELECT scan_uuid FROM scan").fetchone()[0],
        },
        "summary": {
            "totals": {},
            "by_severity": {"critical": 0, "warning": 0, "notice": 0},
            "by_check": {},
            "health_score": 100,
        },
        "issues": issues,
        "pages": [
            {"url": url, "status_code": 200, "metrics": {"representation": "static"}}
            for url in page_urls
        ],
        "groups": groups or [],
    }


def _scan(
    path: Path,
    *,
    issues,
    page_urls=PAGES,
    groups=None,
    generated_at="2026-10-01T00:00:00Z",
    config_overrides=None,
    start_url=SITE,
    partial=False,
    v2=False,
) -> Path:
    overrides = (
        {
            "storage.format_version": "scan.v2",
            "resources.fetch": True,
            **(config_overrides or {}),
        }
        if v2
        else dict(config_overrides or {})
    )
    meta = _metadata(**overrides)
    meta["start_url"] = start_url
    kwargs = {"format_version": "scan.v2"} if v2 else {}
    with NativeScan.create(path, **kwargs, **meta) as scan:
        scan.enqueue([(url, 0) for url in page_urls])
        for _ in page_urls:
            lease = scan.claim(1)[0]
            scan.commit_page(
                lease,
                vars(
                    PageRecord(
                        url=lease.url,
                        content_type="text/html",
                        title="synthetic",
                        crawl_depth=0,
                    )
                ),
                runtime=_runtime(),
            )
        if partial:
            scan.enqueue([("https://example.test/never-crawled", 1)])
            scan.interrupt("test budget")
        scan.save_audit(
            _document(
                scan,
                meta,
                issues=issues,
                page_urls=page_urls,
                groups=groups,
                generated_at=generated_at,
                partial=partial,
            )
        )
        if not partial:
            scan.finish_capture("test")
    return path


@pytest.mark.parametrize("resource_limit", [2 * 1024 * 1024, 12345])
def test_ingest_binds_effective_asset_policy_without_changing_frozen_scan(tmp_path, resource_limit):
    from seohead.core.verification import source_identity
    from seohead.storage.inputs import load_audit_source

    ledger = _ledger(tmp_path)
    scan = _scan(
        tmp_path / "scan.sqlite",
        issues=[_issue("ISSUE-000001", "CHECK_ONE", target=A)],
        config_overrides={"analysis.asset_thresholds.resource_max_bytes": resource_limit},
    )
    before = _file_sha(scan)
    with open_scan(scan) as con:
        frozen = con.execute("SELECT config_fingerprint FROM scan").fetchone()[0]
    identity = source_identity(load_audit_source(scan, "baseline"))
    assert identity["results_policy_fingerprint"] != frozen
    ingest_scan(ledger, scan)
    con = open_ledger(ledger)
    try:
        policy = con.execute("SELECT config_fingerprint FROM source_scan").fetchone()[0]
        assert policy == identity["results_policy_fingerprint"]
    finally:
        con.close()
    ingest_scan(ledger, scan)
    assert _file_sha(scan) == before
    assert len(_occurrences(read_cases(ledger))[0]["observations"]) == 1


def _legacy_page(url: str) -> dict:
    """One complete pages.jsonl record for a legacy_directory.v1 import."""
    return {
        "url": url,
        "status_code": 200,
        "content_type": "text/html",
        "size_bytes": 1024,
        "response_time": 0.1,
        "redirect_url": "",
        "title": "synthetic",
        "meta_description": "",
        "h1": "",
        "h1_2": "",
        "h2": "",
        "canonical": "",
        "meta_robots": "",
        "x_robots": "",
        "og_title": "",
        "og_description": "",
        "og_image": "",
        "word_count": 0,
        "text_ratio": 0.0,
        "crawl_depth": 0,
        "content_encoding": "",
        "charset": "",
        "doctype": "",
        "viewport": "",
        "title_outside_head": False,
        "meta_description_outside_head": False,
        "canonical_outside_head": False,
        "directives_outside_head": False,
        "hreflang_outside_head": False,
        "head_count": 1,
        "body_count": 1,
        "head_not_first": False,
        "invalid_head_elements": "",
        "outlinks": 0,
        "external_outlinks": 0,
        "jsonld_blocks_found": 0,
        "jsonld_blocks_parsed": 0,
        "error": "",
        "error_kind": "",
        "cache_status": "",
        "representation": "legacy_fragment",
        "final_url": url,
        "redirect_chain": [],
    }


def _legacy_scan(path: Path, *, issues, page_urls, groups=None, partial=False) -> Path:
    """Build a legacy_import artifact (no retained bodies/responses) via import_run."""
    source = path.with_suffix("")
    source.mkdir()
    document = {
        "schema_version": "2.0",
        "tool": {"name": "seohead", "version": __version__},
        "run": {
            "input_mode": "crawl",
            "source": SITE,
            "generated_at": "2026-09-01T00:00:00Z",
            "crawl_config": {"speed": {"min_delay_seconds": 0}},
            "crawl_partial": partial,
        },
        "summary": {
            "totals": {},
            "by_severity": {"critical": 0, "warning": 0, "notice": 0},
            "by_check": {},
            "health_score": 100,
        },
        "issues": issues,
        "pages": [{"url": url, "status_code": 200} for url in page_urls],
        "groups": groups or [],
    }
    (source / "audit.json").write_text(json.dumps(document, indent=2))
    (source / "pages.jsonl").write_text(
        "".join(json.dumps(_legacy_page(url)) + "\n" for url in page_urls)
    )
    (source / "links.jsonl").write_text("")
    return import_run(source, path, producer_build=BUILD)


def _counts(ledger: Path) -> dict:
    return ledger_summary(ledger)["counts"]


def _history_text(ledger: Path) -> str:
    return json.dumps(read_cases(ledger), sort_keys=True)


def _occurrences(cases: dict) -> list[dict]:
    return [occurrence for finding in cases["findings"] for occurrence in finding["occurrences"]]


@pytest.mark.parametrize("v2", [False, True], ids=["scan.v1", "scan.v2"])
def test_repeating_one_scan_is_idempotent_and_never_touches_its_bytes(tmp_path, v2):
    ledger = _ledger(tmp_path)
    scan = _scan(
        tmp_path / "scan.sqlite",
        v2=v2,
        issues=[
            _issue("ISSUE-000001", "TITLE_MISSING", target=A, status_code=200),
            _issue("ISSUE-000002", "META_DESCRIPTION_MISSING", target=B),
        ],
    )
    digest_before, version_before = _file_sha(scan), _user_version(scan)

    first = ingest_scan(ledger, scan)
    history_before, summary_before = _history_text(ledger), _counts(ledger)
    second = ingest_scan(ledger, scan)

    assert first["new_source"] and first["ledger_revision"] >= 1
    assert second["already_recorded"] is True
    assert second["new_source"] is False
    assert second["ledger_revision"] == first["ledger_revision"]
    assert _history_text(ledger) == history_before
    assert _counts(ledger) == summary_before
    assert summary_before["finding"] == 2
    assert summary_before["occurrence"] == 2
    assert summary_before["distinct_affected_urls"] == 2
    assert summary_before["observation"] == 2
    assert _file_sha(scan) == digest_before
    assert _user_version(scan) == version_before
    opened = open_scan(scan)
    opened.close()


def test_distinct_checks_urls_subjects_and_representations(tmp_path):
    ledger = _ledger(tmp_path)
    scan = _scan(
        tmp_path / "scan.sqlite",
        issues=[
            _issue("ISSUE-000001", "CHECK_ONE", target=A),
            _issue("ISSUE-000002", "CHECK_ONE", target=B),
            _issue("ISSUE-000003", "CHECK_TWO", target=A),
            _issue("ISSUE-000004", "BROKEN_LINK", target=DEAD1, locations=[{"source_url": A}]),
            _issue("ISSUE-000005", "BROKEN_LINK", target=DEAD2, locations=[{"source_url": A}]),
            _issue("ISSUE-000006", "TITLE_MISSING", target=A, representation="static"),
            _issue("ISSUE-000007", "TITLE_MISSING", target=A, representation="rendered"),
        ],
    )
    ingest_scan(ledger, scan)
    counts = _counts(ledger)
    assert counts["finding"] == 6
    assert counts["occurrence"] == 9
    assert counts["affected_url"] == 8
    assert counts["distinct_affected_urls"] == 4
    keys = {occurrence["occurrence_key"] for occurrence in _occurrences(read_cases(ledger))}
    assert len(keys) == 9
    page_occurrences = [
        occurrence
        for occurrence in _occurrences(read_cases(ledger, url=A))
        if occurrence["subject_value"] == A
    ]
    # Six distinct cases share /a: two findings' primaries, two broken-link
    # sources under different subjects, and a static/rendered pair.
    assert len(page_occurrences) == 6


def test_changed_observation_appends_history_without_rekeying(tmp_path):
    ledger = _ledger(tmp_path)
    first_scan = _scan(
        tmp_path / "first.sqlite",
        issues=[
            _issue(
                "ISSUE-000001",
                "CHECK_ONE",
                target=A,
                status_code=200,
                message="first wording",
            )
        ],
    )
    later_scan = _scan(
        tmp_path / "later.sqlite",
        issues=[
            _issue(
                "ISSUE-000009",
                "CHECK_ONE",
                target=A,
                severity="critical",
                status_code=404,
                message="second wording",
            )
        ],
        generated_at="2026-10-02T00:00:00Z",
        config_overrides={"speed.min_delay_seconds": 0.25},
    )
    ingest_scan(ledger, first_scan)
    ingest_scan(ledger, later_scan)
    cases = read_cases(ledger, check="CHECK_ONE")
    assert len(cases["findings"]) == 1
    occurrence = cases["findings"][0]["occurrences"][0]
    observations = occurrence["observations"]
    assert [item["observation_revision"] for item in observations] == [1, 2]
    assert (
        observations[0]["source_scan"]["scan_uuid"] != observations[1]["source_scan"]["scan_uuid"]
    )
    assert observations[0]["payload_sha256"] != observations[1]["payload_sha256"]
    assert observations[0]["basis_changed"] is None
    assert observations[1]["basis_changed"] == {
        "config_fingerprint": True,
        "producer_version": False,
        "producer_revision": False,
    }
    projections = cases["findings"][0]["projections"]
    assert {row["severity"] for row in projections} == {"warning", "critical"}
    assert {row["status_code"] for row in projections} == {200, 404}
    assert occurrence["current_state"] == "detected"


def test_lifecycle_transitions_are_revision_safe_and_evidence_bound(tmp_path):
    ledger = _ledger(tmp_path)
    first = _scan(
        tmp_path / "first.sqlite",
        issues=[_issue("ISSUE-000001", "CHECK_ONE", target=A)],
    )
    later = _scan(
        tmp_path / "later.sqlite",
        issues=[_issue("ISSUE-000001", "CHECK_ONE", target=A, message="rechecked")],
        generated_at="2026-10-02T00:00:00Z",
    )
    ingest_scan(ledger, first)
    ingest_scan(ledger, later)
    occurrence = _occurrences(read_cases(ledger))[0]
    key = occurrence["occurrence_key"]
    revision = ledger_summary(ledger)["ledger_revision"]

    transition_occurrence(
        ledger,
        occurrence_key=key,
        state="verified",
        actor="reviewer",
        reason="baseline evidence reviewed",
        expected_revision=revision,
        decided_at="2026-10-03T00:00:00Z",
    )
    with pytest.raises(LedgerError, match="ledger revision changed"):
        transition_occurrence(
            ledger,
            occurrence_key=key,
            state="fix_reported",
            actor="reviewer",
            reason="stale writer",
            expected_revision=revision,
        )
    revision += 1
    transition_occurrence(
        ledger,
        occurrence_key=key,
        state="fix_reported",
        actor="operator",
        reason="fix claim is ready for a recheck",
        expected_revision=revision,
        decided_at="2026-10-03T00:01:00Z",
    )
    revision += 1
    transition_occurrence(
        ledger,
        occurrence_key=key,
        state="recheck_pending",
        actor="reviewer",
        reason="request a compatible recheck",
        expected_revision=revision,
        decided_at="2026-10-03T00:02:00Z",
    )
    revision += 1
    with pytest.raises(LedgerError, match="requires an observation_id"):
        transition_occurrence(
            ledger,
            occurrence_key=key,
            state="resolved",
            actor="operator",
            reason="a claim alone cannot resolve a case",
            expected_revision=revision,
        )
    result = transition_occurrence(
        ledger,
        occurrence_key=key,
        state="persisting",
        actor="reviewer",
        reason="later retained evidence still contains the case",
        expected_revision=revision,
        observation_id=occurrence["observations"][1]["observation_id"],
        decided_at="2026-10-03T00:03:00Z",
    )
    assert result["state"] == result["finding_state"] == "persisting"
    stored = _occurrences(read_cases(ledger))[0]
    assert [decision["state"] for decision in stored["decisions"]] == [
        "verified",
        "fix_reported",
        "recheck_pending",
        "persisting",
    ]
    summary = remediation_summary(ledger)
    assert summary["counts"]["persisting"] == 1
    assert summary["resolved_percent"] == 0.0
    assert summary["rechecked_percent"] == 100.0
    assert remediation_report(ledger) == remediation_report(ledger)


def test_case_reads_paginate_exact_findings_without_changing_coverage_totals(tmp_path):
    ledger = _ledger(tmp_path)
    scan = _scan(
        tmp_path / "scan.sqlite",
        issues=[
            _issue("ISSUE-000001", "CHECK_ONE", target=A),
            _issue("ISSUE-000002", "CHECK_TWO", target=B),
        ],
    )
    ingest_scan(ledger, scan)
    first = read_cases(ledger, limit=1, offset=0)
    second = read_cases(ledger, limit=1, offset=1)
    assert first["total"] == second["total"] == 2
    assert first["ledger_revision"] == second["ledger_revision"]
    assert first["findings"][0]["finding_key"] != second["findings"][0]["finding_key"]
    assert remediation_summary(ledger)["denominators"]["verified_original_occurrences"] == 2
    with pytest.raises(LedgerError, match="limit"):
        read_cases(ledger, limit=0)


def test_remediation_coverage_keeps_the_full_original_population_in_every_ratio(tmp_path):
    """A partial recheck cannot make unresolved cases disappear from either denominator."""
    ledger = _ledger(tmp_path)
    urls = [f"https://example.test/case-{index}" for index in range(600)]
    scan = _scan(
        tmp_path / "scan.sqlite",
        issues=[
            _issue(f"ISSUE-{index:06d}", "CHECK_ONE", target=url) for index, url in enumerate(urls)
        ],
        page_urls=urls,
    )
    ingest_scan(ledger, scan)
    con = open_ledger(ledger, write=True)
    try:
        rows = con.execute(
            "SELECT occurrence_id,finding_id FROM occurrence ORDER BY occurrence_id"
        ).fetchall()
        assert len(rows) == 600
        con.execute("BEGIN IMMEDIATE")
        for state, group in (
            ("resolved", rows[:300]),
            ("persisting", rows[300:550]),
            ("unverifiable", rows[550:]),
        ):
            con.executemany(
                "UPDATE occurrence SET current_state=? WHERE occurrence_id=?",
                [(state, row["occurrence_id"]) for row in group],
            )
            con.executemany(
                "UPDATE finding SET current_state=? WHERE finding_id=?",
                [(state, row["finding_id"]) for row in group],
            )
        con.execute("UPDATE ledger SET ledger_revision=ledger_revision+1 WHERE singleton=1")
        con.commit()
    finally:
        con.close()
    summary = remediation_summary(ledger)
    assert summary["denominators"] == {
        "verified_original_occurrences": 600,
        "remediation_cases": 600,
    }
    assert summary["resolved_percent"] == 50.0
    assert summary["rechecked_percent"] == 91.7
    assert summary["tasks"]["unassigned"]["status"] == "blocked"
    report = remediation_report(ledger, limit=1, offset=1)
    assert report["pagination"] == {"total": 600, "limit": 1, "offset": 1, "returned": 1}
    assert len(report["cases"]) == 1


def test_truncated_group_membership_withholds_full_scope_remediation_percentages(tmp_path):
    """Twenty-five represented members cannot stand in for a million-member group."""
    ledger = _ledger(tmp_path)
    members = [f"https://example.test/member-{index}" for index in range(25)]
    scan = _scan(
        tmp_path / "group.sqlite",
        issues=[
            _issue(
                "ISSUE-000001",
                "TITLE_MISSING",
                target=A,
                count=1_000_000,
                group_id="TITLE-GROUP",
                locations=[
                    {"source_url": url, "link_path": f"//main/a[{index + 1}]"}
                    for index, url in enumerate(members)
                ],
            )
        ],
        page_urls=[A, *members],
        groups=[
            {
                "group_id": "TITLE-GROUP",
                "check": "TITLE_MISSING",
                "count": 1_000_000,
                "urls": members,
            }
        ],
    )
    ingest_scan(ledger, scan)
    con = open_ledger(ledger, write=True)
    try:
        rows = con.execute("SELECT occurrence_id,finding_id FROM occurrence").fetchall()
        con.execute("BEGIN IMMEDIATE")
        con.executemany(
            "UPDATE occurrence SET current_state='resolved' WHERE occurrence_id=?",
            [(row["occurrence_id"],) for row in rows],
        )
        con.executemany(
            "UPDATE finding SET current_state='resolved' WHERE finding_id=?",
            [(row["finding_id"],) for row in rows],
        )
        con.execute("UPDATE ledger SET ledger_revision=ledger_revision+1 WHERE singleton=1")
        con.commit()
    finally:
        con.close()
    summary = remediation_summary(ledger)
    represented = len(rows)
    assert summary["scope"]["state"] == "partial"
    assert summary["scope"]["represented_occurrences"] == represented
    assert summary["scope"]["known_unrepresented_members"] == 1_000_000 - represented
    assert summary["represented_resolved_percent"] == 100.0
    assert summary["resolved_percent"] is None
    task = summary["tasks"]["unassigned"]
    assert task["status"] == "scope_incomplete"
    assert task["scope_state"] == "partial"
    assert task["represented_resolved_percent"] == 100.0
    assert task["resolved_percent"] is None
    assert remediation_report(ledger)["summary"]["scope"]["state"] == "partial"


def test_remediation_report_writes_deterministic_review_files_without_mutating_ledger(tmp_path):
    ledger = _ledger(tmp_path)
    scan = _scan(
        tmp_path / "scan.sqlite",
        issues=[_issue("ISSUE-000001", "CHECK_ONE", target=A)],
    )
    ingest_scan(ledger, scan)
    before = ledger.read_bytes()
    result = write_remediation_report(ledger, tmp_path / "report")
    data = Path(result["data"])
    report = Path(result["report"])
    assert data.is_file() and report.is_file()
    assert json.loads(data.read_text(encoding="utf-8"))["schema_version"] == "remediation-report.v1"
    assert "# Remediation evidence report" in report.read_text(encoding="utf-8")
    assert ledger.read_bytes() == before
    with pytest.raises(FileExistsError):
        write_remediation_report(ledger, tmp_path / "report")


def test_retained_verification_artifact_records_pending_case_outcome_atomically(tmp_path):
    from seohead.core.verification import source_identity
    from seohead.mcp.handlers import _load_audit

    ledger = _ledger(tmp_path)
    scan = _scan(
        tmp_path / "scan.sqlite",
        issues=[_issue("ISSUE-000001", "CHECK_ONE", target=A)],
    )
    ingest_scan(ledger, scan)
    occurrence = _occurrences(read_cases(ledger))[0]
    baseline = _load_audit(str(scan), "baseline")
    identity = source_identity(baseline)
    assert identity["audit_sha256"] != occurrence["observations"][0]["source_scan"]["audit_sha256"]
    revision = ledger_summary(ledger)["ledger_revision"]
    for state in ("verified", "fix_reported", "recheck_pending"):
        transition_occurrence(
            ledger,
            occurrence_key=occurrence["occurrence_key"],
            state=state,
            actor="reviewer",
            reason=f"move to {state}",
            expected_revision=revision,
        )
        revision += 1
    verification = tmp_path / "verification.json"
    verification.write_text(
        json.dumps(
            {
                "schema_version": "verification.v1",
                "baseline": {
                    "audit_sha256": identity["audit_sha256"],
                    "scan_uuid": identity["scan_uuid"],
                    "results_policy_fingerprint": identity["results_policy_fingerprint"],
                },
                "selection": {"finding_ids": ["ISSUE-000001"], "urls": [A]},
                "collection": {"state": "measured", "audits": [{"sha256": "b" * 64}]},
                "findings": [
                    {
                        "finding_id": "ISSUE-000001",
                        "check": "CHECK_ONE",
                        "url": A,
                        "status": "resolved",
                        "reason": "measured clean verdict",
                        "before": {"id": "ISSUE-000001", "check": "CHECK_ONE", "target_url": A},
                        "before_page": {"url": A, "status_code": 200},
                        "after": None,
                        "after_page": {"url": A, "status_code": 200},
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    recorded = record_verification(
        ledger, verification, actor="bounded-recheck", expected_revision=revision
    )
    assert recorded["recorded"] == 1
    stored = _occurrences(read_cases(ledger))[0]
    assert stored["current_state"] == "resolved"
    binding = stored["decisions"][-1]["verification"]
    assert stored["decisions"][-1]["observation_id"] is None
    assert binding["measured"] is True
    assert binding["artifact_sha256"] == recorded["verification_sha256"]
    assert binding["baseline_audit_sha256"] == identity["audit_sha256"]
    assert binding["scope_sha256"]
    report = remediation_report(ledger)
    assert report == remediation_report(ledger)
    assert (
        report["cases"][0]["decision"]["verification"]["artifact_sha256"]
        == recorded["verification_sha256"]
    )


def test_verification_refuses_same_uuid_when_audit_or_policy_differs(tmp_path):
    """Scan UUID is provenance, never authority to close an older ledger case."""
    from seohead.core.verification import source_identity
    from seohead.mcp.handlers import _load_audit

    ledger = _ledger(tmp_path)
    scan = _scan(
        tmp_path / "scan.sqlite",
        issues=[_issue("ISSUE-000001", "CHECK_ONE", target=A)],
    )
    ingest_scan(ledger, scan)
    occurrence = _occurrences(read_cases(ledger))[0]
    identity = source_identity(_load_audit(str(scan), "baseline"))
    revision = ledger_summary(ledger)["ledger_revision"]
    for state in ("verified", "fix_reported", "recheck_pending"):
        transition_occurrence(
            ledger,
            occurrence_key=occurrence["occurrence_key"],
            state=state,
            actor="reviewer",
            reason=f"move to {state}",
            expected_revision=revision,
        )
        revision += 1

    def artifact(path, *, audit_sha256, policy):
        path.write_text(
            json.dumps(
                {
                    "schema_version": "verification.v1",
                    "baseline": {
                        "audit_sha256": audit_sha256,
                        "scan_uuid": identity["scan_uuid"],
                        "results_policy_fingerprint": policy,
                    },
                    "selection": {"finding_ids": ["ISSUE-000001"], "urls": [A]},
                    "collection": {"state": "measured", "audits": [{"sha256": "b" * 64}]},
                    "findings": [
                        {
                            "finding_id": "ISSUE-000001",
                            "check": "CHECK_ONE",
                            "url": A,
                            "status": "resolved",
                            "reason": "measured clean verdict",
                            "before": {"id": "ISSUE-000001", "check": "CHECK_ONE", "target_url": A},
                            "before_page": {"url": A, "status_code": 200},
                            "after": None,
                            "after_page": {"url": A, "status_code": 200},
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )

    changed_audit = tmp_path / "changed-audit.json"
    artifact(changed_audit, audit_sha256="f" * 64, policy=identity["results_policy_fingerprint"])
    with pytest.raises(LedgerError, match="does not bind"):
        record_verification(ledger, changed_audit, actor="reviewer", expected_revision=revision)

    changed_policy = tmp_path / "changed-policy.json"
    artifact(changed_policy, audit_sha256=identity["audit_sha256"], policy="0" * 16)
    with pytest.raises(LedgerError, match="does not bind"):
        record_verification(ledger, changed_policy, actor="reviewer", expected_revision=revision)
    assert _occurrences(read_cases(ledger))[0]["current_state"] == "recheck_pending"


def test_ordinal_reorder_and_group_change_preserve_identity_and_membership(tmp_path):
    ledger = _ledger(tmp_path)
    groups_one = [
        {"group_id": "GRP-1", "check": "CHECK_ONE", "urls": [A], "count": 1},
        {"group_id": "GRP-2", "check": "CHECK_TWO", "urls": [B], "count": 1},
    ]
    groups_two = [
        {"group_id": "GRP-3", "check": "CHECK_TWO", "urls": [B], "count": 1},
        {"group_id": "GRP-4", "check": "CHECK_ONE", "urls": [A], "count": 1},
    ]
    scan_one = _scan(
        tmp_path / "one.sqlite",
        issues=[
            _issue("ISSUE-000001", "CHECK_ONE", target=A, group_id="GRP-1"),
            _issue("ISSUE-000002", "CHECK_TWO", target=B, group_id="GRP-2"),
        ],
        groups=groups_one,
    )
    scan_two = _scan(
        tmp_path / "two.sqlite",
        issues=[
            _issue("ISSUE-000001", "CHECK_TWO", target=B, group_id="GRP-3"),
            _issue("ISSUE-000002", "CHECK_ONE", target=A, group_id="GRP-4"),
        ],
        groups=groups_two,
        generated_at="2026-10-02T00:00:00Z",
    )
    ingest_scan(ledger, scan_one)
    ingest_scan(ledger, scan_two)
    cases = read_cases(ledger)
    assert len(cases["findings"]) == 2
    for finding in cases["findings"]:
        assert len(finding["occurrences"]) == 1
        assert [
            item["observation_revision"] for item in finding["occurrences"][0]["observations"]
        ] == [1, 2]
        ordinals = {item["issue_ordinal"] for item in finding["occurrences"][0]["observations"]}
        assert ordinals == {"ISSUE-000001", "ISSUE-000002"}
        memberships = {row["group_ref"] for row in finding["group_memberships"]}
        expected = {"GRP-1", "GRP-4"} if finding["check"] == "CHECK_ONE" else {"GRP-2", "GRP-3"}
        assert memberships == expected


def test_capped_locations_aggregate_counts_and_unavailable_evidence_are_data(tmp_path):
    ledger = _ledger(tmp_path)
    scan = _scan(
        tmp_path / "scan.sqlite",
        issues=[
            _issue(
                "ISSUE-000001",
                "BROKEN_LINK",
                target=DEAD1,
                count=600,
                locations=[{"source_url": A}],
            ),
            _issue("ISSUE-000002", "GRAPH_WIDE_CHECK", count=600),
        ],
    )
    ingest_scan(ledger, scan)
    cases = read_cases(ledger)
    assert len(cases["findings"]) == 2
    link = next(f for f in cases["findings"] if f["check"] == "BROKEN_LINK")
    templated = next(f for f in cases["findings"] if f["check"] == "GRAPH_WIDE_CHECK")
    projection = link["projections"][0]
    assert (projection["occurrences_count"], projection["coverage_state"]) == (600, "capped")
    assert projection["enumerated_count"] == 1
    # Only the two named members exist; the unenumerated remainder is never fabricated.
    assert len(link["occurrences"]) == 2
    assert len(link["affected_urls"]) == 2
    assert {item["discriminator_type"] for item in link["occurrences"]} == {
        "primary",
        "subject",
    }
    dead = next(o for o in link["occurrences"] if o["subject_value"] == DEAD1)
    source = next(o for o in link["occurrences"] if o["subject_value"] == A)
    assert dead["observations"][0]["evidence_state"] == "unavailable"
    assert dead["observations"][0]["evidence_reason"]
    assert source["observations"][0]["evidence_state"] == "measured"
    assert templated["subject_type"] == "scope"
    assert templated["subject_value"] == "site"
    assert templated["affected_urls"] == []
    assert templated["projections"][0]["coverage_state"] == "aggregate_only"
    assert templated["occurrences"][0]["representation"] == "scope"
    assert templated["occurrences"][0]["current_state"] == "detected"


def test_repeated_same_url_locations_keep_stable_link_path_occurrences(tmp_path):
    ledger = _ledger(tmp_path)
    scan = _scan(
        tmp_path / "scan.sqlite",
        issues=[
            _issue(
                "ISSUE-000001",
                "GENERIC_ANCHOR_TEXT",
                target=A,
                count=2,
                locations=[
                    {"source_url": A, "anchor": "Read more", "link_path": "/main/a[1]"},
                    {"source_url": A, "anchor": "Read more", "link_path": "/main/a[2]"},
                ],
            )
        ],
    )
    ingest_scan(ledger, scan)
    finding = read_cases(ledger, check="GENERIC_ANCHOR_TEXT")["findings"][0]
    projection = finding["projections"][0]

    assert projection["coverage_state"] == "enumerated"
    assert projection["enumerated_count"] == 2
    assert len(finding["affected_urls"]) == 2  # one URL, two membership roles
    occurrences = finding["occurrences"]
    assert len(occurrences) == 2
    assert {item["subject_value"] for item in occurrences} == {A}
    assert {item["discriminator_type"] for item in occurrences} == {"locator"}
    assert len({item["discriminator_value"] for item in occurrences}) == 2


def test_repeated_same_url_without_stable_locators_stays_aggregate(tmp_path):
    ledger = _ledger(tmp_path)
    scan = _scan(
        tmp_path / "scan.sqlite",
        issues=[
            _issue(
                "ISSUE-000001",
                "GENERIC_ANCHOR_TEXT",
                target=A,
                count=2,
                locations=[{"source_url": A, "anchor": "Read more"}] * 2,
            )
        ],
    )
    ingest_scan(ledger, scan)
    finding = read_cases(ledger, check="GENERIC_ANCHOR_TEXT")["findings"][0]

    assert finding["projections"][0]["coverage_state"] == "aggregate_only"
    assert finding["projections"][0]["enumerated_count"] == 1
    assert len(finding["occurrences"]) == 1


def test_later_scan_omission_never_resolves_baseline_cases(tmp_path):
    ledger = _ledger(tmp_path)
    first = _scan(
        tmp_path / "first.sqlite",
        issues=[
            _issue("ISSUE-000001", "CHECK_ONE", target=A),
            _issue("ISSUE-000002", "GRAPH_WIDE_CHECK", count=5),
        ],
    )
    later = _scan(
        tmp_path / "later.sqlite",
        page_urls=[HOME + "/"],
        issues=[_issue("ISSUE-000001", "GRAPH_WIDE_CHECK", count=5)],
        generated_at="not-a-timestamp",
        partial=True,
    )
    ingest_scan(ledger, first)
    ingest_scan(ledger, later)
    cases = read_cases(ledger)
    assert len(cases["findings"]) == 2
    missing = next(f for f in cases["findings"] if f["check"] == "CHECK_ONE")
    scoped = next(f for f in cases["findings"] if f["check"] == "GRAPH_WIDE_CHECK")
    assert missing["current_state"] == "detected"
    assert len(missing["occurrences"][0]["observations"]) == 1
    scoped_observations = scoped["occurrences"][0]["observations"]
    assert len(scoped_observations) == 2
    later_observation = scoped_observations[1]
    assert later_observation["source_scan"]["crawl_partial"] is True
    assert later_observation["observed_at_state"] == "unknown"
    assert later_observation["observed_at"] is None
    counts = _counts(ledger)
    assert counts["finding"] == 2
    assert counts["occurrence"] == 2


def test_legacy_import_records_imported_projection_and_partial_scope(tmp_path):
    ledger = _ledger(tmp_path)
    scan = _legacy_scan(
        tmp_path / "legacy.sqlite",
        page_urls=[HOME + "/", A],
        issues=[
            _issue(
                "ISSUE-000001",
                "BROKEN_LINK",
                target=DEAD1,
                count=2,
                locations=[{"source_url": A}],
            )
        ],
        partial=True,
    )
    result = ingest_scan(ledger, scan)
    cases = read_cases(ledger)
    finding = cases["findings"][0]
    source = next(o for o in finding["occurrences"] if o["subject_value"] == A)
    assert source["observations"][0]["evidence_state"] == "imported_projection"
    assert source["representation"] == "legacy_fragment"
    dead = next(o for o in finding["occurrences"] if o["subject_value"] == DEAD1)
    assert dead["observations"][0]["evidence_state"] == "unavailable"
    assert dead["observations"][0]["source_scan"]["crawl_partial"] is True
    summary = ledger_summary(ledger)
    assert summary["counts"]["source_scan"] == 1
    assert result["format_version"] == "scan.v1"
    assert result["evidence_revision"] == 1


def test_wrong_site_scan_is_refused_without_touching_the_ledger(tmp_path):
    ledger = _ledger(tmp_path)
    scan = _scan(
        tmp_path / "foreign.sqlite",
        start_url="https://other.test/",
        page_urls=["https://other.test/"],
        issues=[_issue("ISSUE-000001", "CHECK_ONE", target="https://other.test/a")],
    )
    before = _history_text(ledger)
    with pytest.raises(LedgerError, match="site"):
        ingest_scan(ledger, scan)
    assert _history_text(ledger) == before
    assert _counts(ledger)["source_scan"] == 0
    assert ledger_summary(ledger)["ledger_revision"] == 0


def test_conflicting_bytes_under_one_revision_are_refused(tmp_path):
    ledger = _ledger(tmp_path)
    scan = _scan(
        tmp_path / "scan.sqlite",
        issues=[_issue("ISSUE-000001", "CHECK_ONE", target=A)],
    )
    ingest_scan(ledger, scan)
    forgery = tmp_path / "forgery.sqlite"
    forgery.write_bytes(scan.read_bytes())
    con = sqlite3.connect(forgery)
    document = json.loads(con.execute("SELECT document_json FROM audit").fetchone()[0])
    document["issues"][0]["message"] = "rewritten history"
    raw = json.dumps(document, ensure_ascii=False, indent=2)
    con.execute(
        "UPDATE audit SET document_json=?, sha256=?",
        (raw, hashlib.sha256(raw.encode()).hexdigest()),
    )
    con.commit()
    con.close()
    before = _history_text(ledger)
    with pytest.raises(LedgerError, match="alternate truth"):
        ingest_scan(ledger, forgery)
    assert _history_text(ledger) == before
    opened = open_scan(forgery)
    opened.close()


def test_tampered_audit_digest_fails_source_validation(tmp_path):
    ledger = _ledger(tmp_path)
    scan = _scan(
        tmp_path / "scan.sqlite",
        issues=[_issue("ISSUE-000001", "CHECK_ONE", target=A)],
    )
    con = sqlite3.connect(scan)
    document = json.loads(con.execute("SELECT document_json FROM audit").fetchone()[0])
    document["issues"][0]["message"] = "rewritten history"
    con.execute("UPDATE audit SET document_json=?", (json.dumps(document, indent=2),))
    con.commit()
    con.close()
    with pytest.raises(ScanError):
        ingest_scan(ledger, scan)
    assert _counts(ledger)["source_scan"] == 0


def test_scan_without_audit_is_refused(tmp_path):
    ledger = _ledger(tmp_path)
    path = tmp_path / "scan.sqlite"
    meta = _metadata()
    with NativeScan.create(path, **meta) as scan:
        scan.finish_without_audit()
    with pytest.raises(ScanError, match="no current audit"):
        ingest_scan(ledger, path)


def test_unknown_and_future_ledger_versions_refuse_without_mutation(tmp_path):
    ledger = _ledger(tmp_path)
    con = sqlite3.connect(ledger)
    con.execute("PRAGMA user_version=6")
    con.commit()
    con.close()
    digest = _file_sha(ledger)
    for write in (False, True):
        with pytest.raises(LedgerError, match="user_version"):
            open_ledger(ledger, write=write)
        assert _file_sha(ledger) == digest


def test_canonical_audit_digest_rejects_nonhex_after_valid_prefix(tmp_path):
    ledger = _ledger(tmp_path)
    ingest_scan(
        ledger,
        _scan(tmp_path / "scan.sqlite", issues=[_issue("ISSUE-000001", "CHECK_ONE", target=A)]),
    )
    con = sqlite3.connect(ledger)
    con.execute("UPDATE source_scan SET canonical_audit_sha256=?", ("aa" + "Z" * 62,))
    con.commit()
    con.close()
    with pytest.raises(LedgerError, match="canonical audit digest"):
        open_ledger(ledger)


def test_v1_ledger_migrates_only_on_a_write_open_and_keeps_header_identity(tmp_path):
    """The additive verification tables do not make a read open mutate old evidence."""
    from seohead.storage import ledger as ledger_module

    path = tmp_path / "v1.sqlite"
    con = sqlite3.connect(path)
    con.executescript(ledger_module._schema())
    con.execute(
        "INSERT INTO ledger VALUES(1,?,?,?,?,?,?)",
        (str(uuid.uuid4()), "ledger.v1", "2026-10-01T00:00:00Z", "test", BUILD, 0),
    )
    con.commit()
    con.close()
    original = _file_sha(path)
    with pytest.raises(LedgerError, match="user_version 1"):
        open_ledger(path)
    assert _file_sha(path) == original
    upgraded = open_ledger(path, write=True)
    try:
        assert upgraded.execute("PRAGMA user_version").fetchone()[0] == 5
        assert upgraded.execute("SELECT format_version FROM ledger").fetchone()[0] == "ledger.v1"
        assert upgraded.execute("SELECT COUNT(*) FROM verification_artifact").fetchone()[0] == 0
    finally:
        upgraded.close()
    reopened = open_ledger(path)
    reopened.close()


def test_audit_v2_ingest_streams_large_issue_population_without_legacy_materialization(
    tmp_path, monkeypatch
):
    """A retained audit.v2 remains disk-backed while the ledger records every case."""
    from seohead.storage.audit_v2 import AuditV2Reader

    project = _project(tmp_path)
    scan_path = tmp_path / "large.sqlite"
    meta = _metadata()
    with NativeScan.create(scan_path, **meta) as scan:
        header = {
            "schema_version": "2.0",
            "run": {
                "source": SITE,
                "scan_uuid": scan.con.execute("SELECT scan_uuid FROM scan").fetchone()[0],
                "generated_at": "2026-10-01T00:00:00Z",
                "crawl_config": manifest(meta["config"]),
            },
            "summary": {"totals": {}, "by_severity": {}, "by_check": {}},
            "issues": [],
            "pages": [],
            "groups": [],
        }
        scan.save_audit_v2(
            header,
            {
                "/issues": (
                    _issue(
                        f"ISSUE-{index:06d}",
                        "TITLE_MISSING",
                        target=f"https://example.test/page-{index}",
                        group_id="GROUP-ONE" if index == 0 else None,
                    )
                    for index in range(10_001)
                ),
                "/pages": (
                    {
                        "url": f"https://example.test/page-{index}",
                        "status_code": 200,
                        "content_type": "text/html",
                        "metrics": {"representation": "static"},
                    }
                    for index in range(10_001)
                ),
                "/groups": [
                    {
                        "group_id": "GROUP-ONE",
                        "check": "TITLE_MISSING",
                        "count": 1,
                        "urls": ["https://example.test/page-0"],
                    }
                ],
            },
        )
        scan.finish_without_audit("synthetic large audit.v2")
    monkeypatch.setattr(
        AuditV2Reader,
        "materialize_legacy",
        lambda *_args, **_kwargs: pytest.fail("large audit.v2 was materialized"),
    )
    monkeypatch.setattr(
        Path,
        "read_bytes",
        lambda *_args, **_kwargs: pytest.fail("large scan was read into one bytes object"),
    )
    ledger = create_ledger(tmp_path / "ledger.sqlite", project_dir=project, producer_build=BUILD)
    result = ingest_scan(ledger, scan_path)
    assert result["recorded"]["findings"] == 10_001
    assert result["recorded"]["occurrences"] == 10_001
    assert result["recorded"]["group_memberships_state"] == "complete"
    assert ledger_summary(ledger)["counts"]["occurrence"] == 10_001

    occurrence = next(
        item
        for item in _occurrences(read_cases(ledger, limit=20))
        if item["subject_value"].endswith("page-1")
    )
    revision = ledger_summary(ledger)["ledger_revision"]
    for state in ("verified", "fix_reported", "recheck_pending"):
        transition_occurrence(
            ledger,
            occurrence_key=occurrence["occurrence_key"],
            state=state,
            actor="reviewer",
            reason=f"move to {state}",
            expected_revision=revision,
        )
        revision += 1
    after_path = tmp_path / "after.sqlite"
    with NativeScan.create(after_path, **meta) as scan:
        header = {
            "schema_version": "2.0",
            "run": {
                "source": SITE,
                "scan_uuid": scan.con.execute("SELECT scan_uuid FROM scan").fetchone()[0],
                "generated_at": "2026-10-02T00:00:00Z",
                "crawl_config": manifest(meta["config"]),
            },
            "summary": {
                "totals": {},
                "by_severity": {},
                "by_check": {},
                "check_coverage": {"checks_silent_ids": ["TITLE_MISSING"]},
            },
            "issues": [],
            "pages": [],
        }
        scan.save_audit_v2(
            header,
            {
                "/issues": [],
                "/pages": (
                    {
                        "url": f"https://example.test/page-{index}",
                        "status_code": 200,
                        "content_type": "text/html",
                        "metrics": {"representation": "static"},
                    }
                    for index in range(10_001)
                ),
            },
        )
        scan.finish_without_audit("synthetic after audit.v2")
    from seohead.mcp.handlers import remediation_recheck

    rechecked = remediation_recheck(
        ledger=str(ledger),
        baseline=str(scan_path),
        occurrence_keys=[occurrence["occurrence_key"]],
        actor="large-synthetic-recheck",
        expected_revision=revision,
        out_dir=str(tmp_path / "verification"),
        task_id="large-audit-v2",
        after=str(after_path),
    )
    assert rechecked["verification"]["summary"]["resolved"] == 1
    assert rechecked["recording"]["recorded"] == 1
    foreign = tmp_path / "foreign.sqlite"
    foreign.write_bytes(b"not a ledger")
    with pytest.raises(LedgerError):
        open_ledger(foreign)
    missing = tmp_path / "missing.sqlite"
    with pytest.raises(LedgerError):
        open_ledger(missing)
    assert not missing.exists()


def _v0_stub(path: Path, *, project_uuid: str, target: str) -> None:
    con = sqlite3.connect(path)
    con.execute(f"PRAGMA application_id={APPLICATION_ID}")
    con.execute("PRAGMA user_version=0")
    con.execute(
        "CREATE TABLE ledger_meta(singleton INTEGER PRIMARY KEY CHECK (singleton=1),"
        "format_version TEXT NOT NULL,ledger_uuid TEXT NOT NULL,project_uuid TEXT NOT NULL,"
        "site_target TEXT NOT NULL,writer_version TEXT NOT NULL,writer_revision TEXT NOT NULL,"
        "created_at TEXT NOT NULL)"
    )
    con.execute(
        "INSERT INTO ledger_meta VALUES(1,'ledger.v0',?,?,?,?,?,?)",
        (
            str(uuid.uuid4()),
            project_uuid,
            target,
            __version__,
            BUILD,
            "2026-09-01T00:00:00Z",
        ),
    )
    con.commit()
    con.close()


def test_v0_stub_migrates_on_explicit_write_and_passes_integrity(tmp_path):
    project = _project(tmp_path)
    ledger = tmp_path / "stub.sqlite"
    project_uuid = json.loads((project / "project.json").read_text())["project_uuid"]
    _v0_stub(ledger, project_uuid=project_uuid, target=SITE)
    with pytest.raises(LedgerError, match="user_version"):
        open_ledger(ledger)
    con = open_ledger(ledger, write=True)
    assert con.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
    assert con.execute("PRAGMA foreign_key_check").fetchone() is None
    assert con.execute("SELECT COUNT(*) FROM site WHERE role='primary'").fetchone()[0] == 1
    con.close()
    scan = _scan(
        tmp_path / "scan.sqlite",
        issues=[_issue("ISSUE-000001", "CHECK_ONE", target=A)],
    )
    assert ingest_scan(ledger, scan)["ok"]
    assert _counts(ledger)["finding"] == 1


def test_failed_migration_rolls_back_atomically(tmp_path):
    ledger = tmp_path / "bad-stub.sqlite"
    _v0_stub(ledger, project_uuid="not-a-uuid", target=SITE)
    digest = _file_sha(ledger)
    with pytest.raises(LedgerError):
        open_ledger(ledger, write=True)
    assert _file_sha(ledger) == digest
    assert _user_version(ledger) == 0
    con = sqlite3.connect(ledger)
    row = con.execute("SELECT format_version FROM ledger_meta").fetchone()
    con.close()
    assert row[0] == "ledger.v0"


def test_missing_source_artifact_is_recorded_and_restored(tmp_path):
    ledger = _ledger(tmp_path)
    scan = _scan(
        tmp_path / "scan.sqlite",
        issues=[_issue("ISSUE-000001", "CHECK_ONE", target=A)],
    )
    result = ingest_scan(ledger, scan)
    noted = note_source_missing(ledger, result["source_scan_id"], reason="pruned by retention")
    assert noted["ledger_revision"] == result["ledger_revision"] + 1
    cases = read_cases(ledger)
    observation = cases["findings"][0]["occurrences"][0]["observations"][0]
    assert observation["source_scan"]["artifact_state"] == "missing"
    again = ingest_scan(ledger, scan)
    # Re-ingest is observation-idempotent, but restoring 'present' is a real
    # change to the binding's availability flag, so the revision advances once.
    assert again["recorded"]["observations"] == 0
    assert again["ledger_revision"] == noted["ledger_revision"] + 1
    cases = read_cases(ledger)
    observation = cases["findings"][0]["occurrences"][0]["observations"][0]
    assert observation["source_scan"]["artifact_state"] == "present"


def test_create_ledger_binds_project_and_never_overwrites(tmp_path):
    project = _project(tmp_path)
    ledger = _ledger(tmp_path, project_dir=project)
    summary = ledger_summary(ledger)
    assert summary["format_version"] == "ledger.v1"
    assert summary["ledger_revision"] == 0
    assert summary["sites"][0]["role"] == "primary"
    assert summary["sites"][0]["host"] == "example.test"
    with pytest.raises(LedgerError, match="never overwrite"):
        _ledger(tmp_path, project_dir=project)
    with pytest.raises((LedgerError, ValueError)):
        create_ledger(
            tmp_path / "other.sqlite",
            project_dir=tmp_path / "missing-project",
            producer_build=BUILD,
        )
    with pytest.raises(LedgerError, match="Git SHA"):
        create_ledger(
            tmp_path / "third.sqlite",
            project_dir=project,
            producer_build="not-a-sha",
        )
    assert not (tmp_path / "third.sqlite").exists()


def test_identity_keys_are_stable_and_exclude_observation_fields(tmp_path):
    base = dict(
        project_uuid=str(uuid.uuid4()),
        site_host="example.test",
        check="CHECK_ONE",
        subject_type="url",
        subject_value=canonical_url(A + "/"),
    )
    key = finding_key(**base)
    assert key == finding_key(**base)
    assert key != finding_key(**{**base, "check": "CHECK_TWO"})
    assert key != finding_key(**{**base, "subject_value": canonical_url(B)})
    assert key != finding_key(**{**base, "subject_type": "scope", "subject_value": "site"})
    occurrence = dict(
        base, representation="static", discriminator_type="primary", discriminator_value=""
    )
    assert occurrence_key(**occurrence) == occurrence_key(**occurrence)
    assert occurrence_key(**occurrence) != occurrence_key(
        **{**occurrence, "representation": "rendered"}
    )
    assert occurrence_key(**occurrence) != occurrence_key(
        **{
            **occurrence,
            "discriminator_type": "subject",
            "discriminator_value": DEAD1,
        }
    )
    assert canonical_url(" https://EXAMPLE.test/a/ ") == A
    with pytest.raises(LedgerError):
        canonical_url("   ")
