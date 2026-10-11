"""Issue #59's acceptance criteria: every cited Lighthouse audit id is real,
none is claimed twice, and every entry names one of our own checks."""

from __future__ import annotations

from seohead.sf.core.lighthouse import LIGHTHOUSE_AUDIT_IDS, LIGHTHOUSE_MAP
from seohead.sf.core.registry import CHECKS


def test_every_mapped_check_is_a_real_registry_check():
    unknown = sorted(set(LIGHTHOUSE_MAP) - set(CHECKS))
    assert not unknown, f"LIGHTHOUSE_MAP cites checks the registry does not define: {unknown}"


def test_every_cited_audit_id_is_one_lighthouse_actually_defines():
    bad = {
        check_id: entry["audit_id"]
        for check_id, entry in LIGHTHOUSE_MAP.items()
        if entry["audit_id"] not in LIGHTHOUSE_AUDIT_IDS
    }
    assert not bad, f"cited an id Lighthouse's own source does not define: {bad}"


def test_no_lighthouse_audit_is_claimed_by_two_checks():
    seen: dict[str, str] = {}
    dupes: list[str] = []
    for check_id, entry in LIGHTHOUSE_MAP.items():
        audit_id = entry["audit_id"]
        if audit_id in seen:
            dupes.append(f"{audit_id}: {seen[audit_id]} and {check_id}")
        else:
            seen[audit_id] = check_id
    assert not dupes, f"the same Lighthouse audit is claimed twice: {dupes}"


def test_every_entry_carries_a_doc_url():
    for check_id, entry in LIGHTHOUSE_MAP.items():
        assert entry.get("doc_url", "").startswith("https://"), (
            f"{check_id} has no public documentation URL"
        )


def test_the_ground_truth_id_set_is_not_accidentally_empty():
    # A regression guard on the snapshot itself, not the mapping: if this ever
    # collapses to a handful of ids, every id-validity assertion above would
    # pass vacuously for the wrong reason.
    assert len(LIGHTHOUSE_AUDIT_IDS) > 100


def test_new_checks_are_new_and_correspondence_checks_already_existed():
    """Guards the issue's "do not duplicate, record correspondence instead" rule."""
    new_checks = {"MISSING_CHARSET", "MISSING_DOCTYPE", "VIEWPORT_MISSING", "NO_COMPRESSION"}
    correspondence_only = {"HTTP1_ONLY", "IMG_MISSING_DIMENSIONS"}
    supplied_lab = {
        "LCP_DISCOVERY",
        "NETWORK_DEPENDENCY_TREE",
        "LAYOUT_SHIFT_CULPRITS",
        "IMAGE_DELIVERY",
        "FORCED_REFLOW",
        "JS_EXECUTION_TIME",
    }
    assert new_checks | correspondence_only | supplied_lab == set(LIGHTHOUSE_MAP)
    for check_id in correspondence_only:
        assert "already covers" in LIGHTHOUSE_MAP[check_id]["note"].lower()
    for check_id in supplied_lab:
        assert "supplied" in LIGHTHOUSE_MAP[check_id]["note"].lower()
