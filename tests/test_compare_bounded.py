"""Complete on-disk comparison conservation and targeted streaming verification."""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path

import pytest

from seohead.core.verification import classify, compact_after_source, select_source
from seohead.sf.core.compare import CompareError, compare
from seohead.sf.core.compare_store import compare_markdown, read_compare_page
from seohead.storage.audit_v2 import AuditV2Reader, write_audit_v2
from tests.test_scan_audit_v2 import _scan
from tests.test_verify_fixes import A, B, _audit, _issue


def _read(result, name):
    file = result["files"][name]
    raw = (Path(result["out_dir"]) / file["path"]).read_bytes()
    assert hashlib.sha256(raw).hexdigest() == file["sha256"]
    assert len(raw) == file["bytes"]
    rows = [json.loads(line) for line in raw.splitlines()]
    assert len(rows) == file["rows"]
    return rows


def _source(tmp_path, name, document):
    scan = tmp_path / f"{name}.sqlite"
    binding = _scan(scan)
    header = copy.deepcopy(document)
    collections = {f"/{key}": header.pop(key) for key in ("pages", "issues")}
    header.update(pages=[], issues=[])
    write_audit_v2(scan, header, collections, binding)
    return AuditV2Reader(scan)


def test_file_compare_conserves_every_row_and_matches_legacy(tmp_path):
    gone, new = "https://example.test/gone", "https://example.test/new"
    before = _audit(
        urls=(A, B, gone),
        issues=[
            _issue("i1", "TITLE_MISSING", A, suppressed=True),
            _issue("i2", "DESC_MISSING", A),
            _issue("i3", "TITLE_MISSING", gone),
            _issue("i4", "TITLE_TEMPLATED", None),
        ],
    )
    after = _audit(
        urls=(A, B, new),
        issues=[
            _issue("i5", "TITLE_MISSING", A, details={"changed": True}),
            _issue("i6", "DESC_MISSING", B),
            _issue("i7", "TITLE_MISSING", new),
        ],
    )
    legacy = compare(before, after)
    with _source(tmp_path, "before", before) as first, _source(tmp_path, "after", after) as second:
        result = compare(first, second, out_dir=tmp_path / "comparison")
        assert result["before"]["audit_sha256"] == first.sha256
        assert result["after"]["scan_uuid"] == second.binding["scan_uuid"]
    assert result["schema_version"] == "compare.v2"
    for bucket in ("entered", "left", "appeared", "disappeared"):
        assert _read(result, bucket) == legacy[bucket]
        assert result["summary"][bucket] == legacy["summary"][bucket]
    assert _read(result, "unchanged") == [
        {"before": before["issues"][0], "after": after["issues"][0]}
    ]
    assert result["conservation"] == {
        "before_issues": 4,
        "after_issues": 3,
        "before_accounted": 4,
        "after_accounted": 3,
        "state": "complete",
    }
    assert result["segment_breakdown"]["state"] == "unavailable"
    by_check = {row.pop("check"): row for row in _read(result, "by_check")}
    assert by_check == legacy["summary"]["by_check"]
    assert len(json.dumps(result)) < 20000
    assert sorted(p.name for p in Path(result["out_dir"]).iterdir()) == [
        "appeared.ndjson",
        "by_check.ndjson",
        "compare.json",
        "disappeared.ndjson",
        "entered.ndjson",
        "left.ndjson",
        "unchanged.ndjson",
    ]


def test_compare_v2_reports_declared_segment_count_deltas(tmp_path):
    before = _audit()
    after = _audit()
    before["segments"] = {"Blog": {"pages": 4, "issues": 2}}
    after["segments"] = {"Blog": {"pages": 5, "issues": 1}}
    result = compare(before, after, out_dir=tmp_path / "segmented")
    assert result["segment_breakdown"]["segments"]["Blog"] == {
        "state": "compared",
        "pages": {"before": 4, "after": 5, "delta": 1},
        "issues": {"before": 2, "after": 1, "delta": -1},
    }
    report = compare_markdown(result["manifest"])
    assert "# Saved scan comparison" in report
    assert "Blog: pages 4 → 5 (+1); findings 2 → 1 (-1)." in report


def test_file_compare_is_deterministic_and_never_replaces_output(tmp_path):
    before = _audit(issues=[_issue("b", "TITLE_MISSING", B), _issue("a", "TITLE_MISSING", A)])
    after = _audit()
    first = compare(before, after, out_dir=tmp_path / "first")
    before["issues"].reverse()
    second = compare(before, after, out_dir=tmp_path / "second")
    assert first["files"] == second["files"]
    with pytest.raises(FileExistsError):
        compare(before, after, out_dir=tmp_path / "first")


def test_compare_v2_reader_sorts_filters_and_bounds_rows(tmp_path):
    before = _audit(
        issues=[_issue("b", "TITLE_MISSING", B), _issue("a", "DESC_MISSING", A)]
    )
    result = compare(before, _audit(), out_dir=tmp_path / "comparison")
    page = read_compare_page(
        result["manifest"], "disappeared", limit=1, sort="url", direction="asc"
    )
    assert page["total"] == 2 and page["returned"] == 1 and page["has_more"] is True
    assert page["rows"][0]["target_url"] == A
    filtered = read_compare_page(
        result["manifest"], "disappeared", contains="b.test", limit=10
    )
    assert filtered["total"] == 1
    assert filtered["rows"][0]["target_url"] == B


def test_compare_url_never_calls_missing_url_a_fix():
    from seohead.mcp.handlers import compare_url

    before = _audit(urls=(A,), issues=[_issue("a", "TITLE_MISSING", A)])
    after = _audit(urls=(), issues=[])
    result = compare_url(before, after, A)
    assert result["state"] == "not_rechecked"
    assert result["findings"]["state"] == "not_rechecked"
    assert result["findings"]["left"] == []


@pytest.mark.parametrize("kind", ["page", "finding"])
def test_file_compare_rejects_duplicate_identity_without_partial_publication(tmp_path, kind):
    before = _audit(issues=[_issue("a", "TITLE_MISSING", A)])
    if kind == "page":
        before["pages"].append(before["pages"][0])
    else:
        before["issues"].append({**before["issues"][0], "id": "b"})
    with pytest.raises(CompareError, match="duplicate"):
        compare(before, _audit(), out_dir=tmp_path / "output")
    assert not (tmp_path / "output").exists()
    assert not list(tmp_path.glob(".compare-*"))


def test_legacy_duplicate_findings_are_not_silently_overwritten():
    before = _audit(issues=[_issue("a", "TITLE_MISSING", A), _issue("b", "TITLE_MISSING", A)])
    with pytest.raises(CompareError, match="duplicate"):
        compare(before, _audit())


def test_correspondence_streams_full_pair_facts_and_unmatched_rows(tmp_path):
    mapped, missing = "https://after.test/a", "https://example.test/missing"
    before = _audit(urls=(A,), issues=[_issue("a", "TITLE_MISSING", A)])
    after = _audit(urls=(mapped,), issues=[])
    declaration = {
        "schema_version": "url-correspondence.v1",
        "origin_map": {},
        "pairs": [
            {"before": A, "after": mapped},
            {"before": missing, "after": "https://after.test/missing"},
        ],
    }
    legacy = compare(before, after, correspondence=declaration)
    result = compare(before, after, correspondence=declaration, out_dir=tmp_path / "review")
    assert _read(result, "left") == legacy["left"]
    assert _read(result, "correspondence") == legacy["release_review"]["correspondence"]["pairs"]
    assert _read(result, "facts") == legacy["release_review"]["facts"]


def test_correspondence_collision_with_identity_mapped_page_is_rejected(tmp_path):
    before = _audit(urls=(A, B))
    after = _audit(urls=(B,))
    declaration = {
        "schema_version": "url-correspondence.v1",
        "origin_map": {},
        "pairs": [{"before": A, "after": B}],
    }
    with pytest.raises(CompareError, match="collision"):
        compare(before, after, correspondence=declaration, out_dir=tmp_path / "collision")


def test_partial_unknown_and_incompatible_bases_stay_visible(tmp_path):
    before = _audit(urls=(A,), issues=[_issue("a", "TITLE_MISSING", A)])
    after = _audit(urls=(), crawl_partial=True, crawl_valid=False)
    after["run"]["crawl_config"]["robots.enabled"] = False
    with pytest.raises(CompareError, match="settings differ"):
        compare(before, after, out_dir=tmp_path / "refused")
    result = compare(before, after, force=True, out_dir=tmp_path / "forced")
    assert result["summary"]["left"] == 1
    assert any("invalid" in item for item in result["warnings"])
    assert any("partial" in item for item in result["warnings"])
    assert any(row["state"] == "unknown" for row in result["compatibility"])
    assert any(row["state"] == "incompatible" for row in result["compatibility"])


def test_large_source_requires_explicit_output_without_iteration(tmp_path):
    class Source:
        def __init__(self):
            self.header = {"pages": {}, "issues": {}}
            self.collections = {"/pages": 10001, "/issues": 0}

        def iter_collection(self, name):
            pytest.fail("refusal must occur before reading any collection")

    with pytest.raises(CompareError, match="requires out_dir"):
        compare(Source(), Source())
    assert not list(tmp_path.iterdir())


def test_verification_streams_only_selected_rows_and_rejects_duplicate_pages(tmp_path):
    document = _audit(issues=[_issue("a", "TITLE_MISSING", A), _issue("b", "DESC_MISSING", B)])
    with _source(tmp_path, "baseline", document) as source:
        compact, selected, targets, identity = select_source(source, finding_ids=["a"])
        assert compact["pages"] == document["pages"][:1]
        assert selected == document["issues"][:1] and targets == [A]
        assert identity["audit_sha256"] == source.sha256
    document["pages"].append(document["pages"][0])
    with (
        _source(tmp_path, "bad", document) as source,
        pytest.raises(ValueError, match="duplicate page"),
    ):
        select_source(source, urls=[A])


@pytest.mark.parametrize("kind", ["page", "finding"])
def test_after_stream_duplicates_cannot_grow_selection_or_fake_clean(tmp_path, kind):
    selected = [_issue("a", "TITLE_MISSING", A)]
    document = _audit(issues=selected)
    document["pages" if kind == "page" else "issues"].append(
        document["pages" if kind == "page" else "issues"][0]
    )
    with (
        _source(tmp_path, "after", document) as source,
        pytest.raises(ValueError, match="duplicate"),
    ):
        compact_after_source(source, selected)


def test_invalid_after_does_not_clear_a_selected_finding():
    before = _audit(issues=[_issue("a", "TITLE_MISSING", A)])
    after = _audit(crawl_valid=False)
    result = classify(before, before["issues"], {A: after})
    assert result[0]["status"] == "not_verifiable"
    assert "invalid" in result[0]["reason"]


def test_verification_handler_uses_binding_identity_without_materialization(tmp_path):
    from seohead.mcp.handlers import verify_fixes

    before = _audit(issues=[_issue("a", "TITLE_MISSING", A)])
    after = _audit(generated_at="2026-10-02T00:00:00Z")
    with _source(tmp_path, "first", before) as first, _source(tmp_path, "second", after) as second:
        first.materialize_legacy = lambda **kwargs: pytest.fail("materialized baseline")
        second.materialize_legacy = lambda **kwargs: pytest.fail("materialized after")
        result = verify_fixes(
            baseline=first, after=second, urls=[A], out_dir=str(tmp_path / "verify")
        )
        assert result["summary"]["resolved"] == 1
        assert result["baseline"]["scan_uuid"] == first.binding["scan_uuid"]
        same = verify_fixes(baseline=first, after=first, urls=[A], out_dir=str(tmp_path / "same"))
        assert same["summary"]["not_verifiable"] == 1
        assert "same scan UUID" in same["collection"]["reason"]


def test_bound_identity_conflict_is_not_accepted(tmp_path):
    from seohead.core.verification import source_identity

    document = _audit(scan_uuid="different-source")
    with (
        _source(tmp_path, "conflict", document) as source,
        pytest.raises(ValueError, match="identity differs"),
    ):
        source_identity(source)


def test_correspondence_declaration_limit_refuses_before_reading_all_bytes(tmp_path):
    declaration = tmp_path / "oversized.json"
    with declaration.open("wb") as stream:
        stream.truncate(16 * 1024 * 1024 + 1)
    with pytest.raises(CompareError, match="16 MiB"):
        compare(_audit(), _audit(), correspondence=declaration, out_dir=tmp_path / "output")


def test_gzip_reopen_preserves_all_rows_order_counts_and_deterministic_bytes(tmp_path):
    from seohead.sf.core.compare_store import iter_compare_rows

    before = _audit(issues=[_issue("b", "TITLE_MISSING", B), _issue("a", "TITLE_MISSING", A)])
    after = _audit(issues=[_issue("after", "TITLE_MISSING", A)])
    plain = compare(before, after, out_dir=tmp_path / "plain")
    gzip = compare(before, after, out_dir=tmp_path / "gzip", compression="gzip")
    repeat = compare(before, after, out_dir=tmp_path / "repeat", compression="gzip")
    assert gzip["conservation"] == plain["conservation"]
    assert gzip["summary"] == plain["summary"]
    assert gzip["files"] == repeat["files"]
    for name, entry in gzip["files"].items():
        expected = _read(plain, name)
        assert list(iter_compare_rows(gzip["manifest"], name)) == expected
        assert list(iter_compare_rows(plain["manifest"], name)) == expected
        assert entry["format"] == "ndjson.gz"
        assert entry["rows"] == len(expected)
        assert entry["uncompressed_bytes"] == plain["files"][name]["bytes"]
        stored = (Path(gzip["out_dir"]) / entry["path"]).read_bytes()
        assert len(stored) == entry["bytes"]
        assert hashlib.sha256(stored).hexdigest() == entry["sha256"]
    assert not list(Path(gzip["out_dir"]).glob("*.ndjson"))


@pytest.mark.parametrize(
    "damage", ["tamper", "truncate", "truncate-rehashed", "wrong-count", "wrong-decoded-bytes"]
)
def test_gzip_reader_rejects_corruption_and_forged_count_metadata(tmp_path, damage):
    from seohead.sf.core.compare_store import iter_compare_rows

    before = _audit(issues=[_issue("a", "TITLE_MISSING", A)])
    result = compare(before, before, out_dir=tmp_path / "compressed", compression="gzip")
    manifest_path = Path(result["manifest"])
    manifest = json.loads(manifest_path.read_text())
    entry = manifest["files"]["unchanged"]
    path = manifest_path.parent / entry["path"]
    raw = path.read_bytes()
    if damage.startswith("truncate"):
        path.write_bytes(raw[:-8])
        if damage == "truncate-rehashed":
            entry["bytes"] = len(raw) - 8
            entry["sha256"] = hashlib.sha256(raw[:-8]).hexdigest()
    elif damage == "tamper":
        path.write_bytes(raw[:12] + bytes([raw[12] ^ 1]) + raw[13:])
    elif damage == "wrong-count":
        entry["rows"] += 1
    else:
        entry["uncompressed_bytes"] += 1
    manifest_path.write_text(json.dumps(manifest))
    with pytest.raises(CompareError):
        list(iter_compare_rows(manifest_path, "unchanged"))


def test_gzip_requires_explicit_output_and_valid_compression():
    with pytest.raises(CompareError, match="requires out_dir"):
        compare(_audit(), _audit(), compression="gzip")
    with pytest.raises(CompareError, match="none or gzip"):
        compare(_audit(), _audit(), compression="zip")


def test_failed_gzip_export_never_publishes_a_partial_package(tmp_path, monkeypatch):
    import seohead.sf.core.compare_store as store

    original = store._export
    calls = 0

    def fail_mid_export(*args, **kwargs):
        nonlocal calls
        calls += 1
        if calls == 3:
            raise OSError("synthetic disk failure")
        return original(*args, **kwargs)

    monkeypatch.setattr(store, "_export", fail_mid_export)
    with pytest.raises(OSError, match="disk failure"):
        compare(_audit(), _audit(), out_dir=tmp_path / "failed", compression="gzip")
    assert not (tmp_path / "failed").exists()
    assert not list(tmp_path.glob(".compare-*"))


@pytest.mark.parametrize(
    "partial, expected", [(None, "unknown"), (False, "compatible"), (True, "compatible")]
)
def test_corpus_completeness_unknown_is_not_equal_to_measured_complete(tmp_path, partial, expected):
    from seohead.sf.core.evidence_contract import comparison_compatibility

    before = _audit()
    before["summary"]["saved_corpus_derivations"] = {"schema_version": "saved_corpus.v1"}
    before["summary"]["evidence_contract"] = {"scan_identity_state": "measured"}
    if partial is not None:
        before["run"]["corpus_partial"] = partial
    after = copy.deepcopy(before)
    basis = next(row for row in comparison_compatibility(before, after) if row["basis"] == "corpus")
    assert basis["state"] == expected
    for result in (
        compare(before, after),
        compare(before, after, out_dir=tmp_path / "files", compression="gzip"),
    ):
        row = next(row for row in result["compatibility"] if row["basis"] == "corpus")
        assert row["state"] == expected
        corpus_warnings = [warning for warning in result["warnings"] if "corpus" in warning]
        if partial is None:
            assert any("unknown" in warning for warning in corpus_warnings)
        elif partial:
            assert any("partial" in warning for warning in corpus_warnings)
        else:
            assert not corpus_warnings


def test_missing_corpus_flag_on_one_side_stays_unknown():
    from seohead.sf.core.evidence_contract import comparison_compatibility

    before = _audit()
    before["summary"]["saved_corpus_derivations"] = {"schema_version": "saved_corpus.v1"}
    after = copy.deepcopy(before)
    after["run"]["corpus_partial"] = False
    basis = next(row for row in comparison_compatibility(before, after) if row["basis"] == "corpus")
    assert basis["state"] == "unknown"
    assert basis["before"]["corpus_partial"] is None
    assert basis["after"]["corpus_partial"] is False


@pytest.mark.parametrize("field", ["checks_skipped", "checks_disabled"])
def test_disappearing_skipped_check_is_an_unverified_delta_with_retained_reason(tmp_path, field):
    before = _audit(issues=[_issue("stale", "SITEMAP_STALE_LASTMOD", A)])
    after = _audit(urls=())
    after["run"][field] = [
        {"id": "SITEMAP_STALE_LASTMOD", "reason": "retained evidence unavailable"}
    ]
    for result in (
        compare(before, after),
        compare(before, after, out_dir=tmp_path / "qualified", compression="gzip"),
    ):
        assert result["summary"]["disappeared"] == 1
        assert result["measurement_gaps"] == [
            {
                "side": "after",
                "check": "SITEMAP_STALE_LASTMOD",
                "state": field.removeprefix("checks_"),
                "reason": "retained evidence unavailable",
            }
        ]
        assert any(
            "SITEMAP_STALE_LASTMOD" in warning and "unverified" in warning
            for warning in result["warnings"]
        )
