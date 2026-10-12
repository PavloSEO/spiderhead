"""Image findings retain their source identity in list and streamed aggregation."""

from __future__ import annotations

import tempfile

import pytest

from seohead.sf.core.aggregate import _dedupe, _fingerprint
from seohead.sf.core.context import _DiskIssues
from seohead.sf.core.models import Issue

RESOURCE = "https://example.test/shared.png"
PAGE_A = "https://example.test/a"
PAGE_B = "https://example.test/b"


@pytest.fixture(params=("list", "disk"))
def deduplicate(request, tmp_path, monkeypatch):
    if request.param == "disk":
        monkeypatch.setattr(tempfile, "tempdir", str(tmp_path))

    def run(issues):
        if request.param == "list":
            return _dedupe(issues)
        stored = _DiskIssues()
        try:
            for issue in issues:
                stored.append(issue)
            return list(stored.iter_deduped_sorted())
        finally:
            stored.close()

    return run


def _image(source_page=PAGE_A, *, ordinal=0, document_id=1, check="IMG_MISSING_ALT_ATTRIBUTE"):
    details = {}
    if source_page is not None:
        details = {
            "source_page": source_page,
            "target_scope": "image_element",
            "document_id": document_id,
            "image_ordinal": ordinal,
        }
    return Issue(
        check=check,
        severity="warning",
        source="native-retained-image-markup",
        message="Image declaration needs review",
        target_url=RESOURCE,
        status_code=404 if check == "IMG_BROKEN" else 200,
        locations=[{"source_url": source_page or PAGE_A, "link_path": f"img[{ordinal + 1}]"}],
        details=details,
    )


@pytest.mark.parametrize("check", ("IMG_MISSING_ALT_ATTRIBUTE", "IMG_BROKEN", "IMG_OVER_KB"))
def test_shared_resource_keeps_source_bound_findings_and_stable_fingerprints(deduplicate, check):
    original = deduplicate([_image(PAGE_A, check=check), _image(PAGE_B, check=check)])
    assert len(original) == 2
    assert {issue.target_url for issue in original} == {RESOURCE}
    assert {issue.details["source_page"] for issue in original} == {PAGE_A, PAGE_B}
    fingerprints = {issue.details["source_page"]: _fingerprint(issue) for issue in original}
    assert len(set(fingerprints.values())) == 2

    replay = deduplicate(
        [
            _image(PAGE_B, check=check, document_id=90, ordinal=8),
            _image(PAGE_A, check=check, document_id=91, ordinal=9),
        ]
    )
    assert {issue.details["source_page"]: _fingerprint(issue) for issue in replay} == fingerprints


def test_repeated_elements_keep_occurrence_locations_and_one_source_identity(deduplicate):
    original = _fingerprint(_image())
    (merged,) = deduplicate([_image(ordinal=0), _image(ordinal=6)])
    assert merged.occurrences_count == 2
    assert {location["link_path"] for location in merged.locations} == {"img[1]", "img[7]"}
    assert {location["source_url"] for location in merged.locations} == {PAGE_A}
    assert _fingerprint(merged) == original


def test_capped_image_locations_keep_exact_counts_and_stable_fingerprint(deduplicate):
    first, second = _image(), _image(ordinal=200)
    first.locations = [{"source_url": PAGE_A, "link_path": f"img[{n + 1}]"} for n in range(200)]
    first.occurrences_count = 200
    second.locations = [
        {"source_url": PAGE_A, "link_path": f"img[{n + 1}]"} for n in range(200, 205)
    ]
    second.occurrences_count = 5
    fingerprint = _fingerprint(first)

    (merged,) = deduplicate([first, second])
    assert merged.occurrences_count == 205
    assert len(merged.locations) == 200
    assert merged.details["locations_omitted"] == 5
    assert _fingerprint(merged) == fingerprint


def test_legacy_image_without_source_page_keeps_old_dedupe_and_fingerprint(deduplicate):
    first, second = _image(None), _image(None, ordinal=6)
    second.locations[0]["source_url"] = PAGE_B
    (merged,) = deduplicate([first, second])
    assert merged.occurrences_count == 2
    assert {location["source_url"] for location in merged.locations} == {PAGE_A, PAGE_B}
    assert _fingerprint(merged) == "8faba97ddc3d"


def test_non_image_source_page_metadata_does_not_change_old_identity(deduplicate):
    issues = [
        Issue(
            check="BROKEN_INTERNAL_LINK",
            severity="critical",
            source="inlinks:All",
            message="Internal target returned an error",
            target_url="https://example.test/missing",
            status_code=404,
            locations=[{"source_url": page}],
            details={"source_page": page, "target_scope": "image_element"},
        )
        for page in (PAGE_A, PAGE_B)
    ]
    (merged,) = deduplicate(issues)
    assert merged.occurrences_count == 2
    assert {location["source_url"] for location in merged.locations} == {PAGE_A, PAGE_B}
    assert _fingerprint(merged) == "ee7eb85c7c7d"
