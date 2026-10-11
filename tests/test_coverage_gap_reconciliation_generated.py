import re
from collections import Counter
from pathlib import Path

from scripts.generate_coverage_gap_reconciliation import (
    OUT,
    evidence_refs,
    priority_rows,
    render,
    rows,
    status,
)
from seohead.cli import COMMANDS
from seohead.mcp.handlers import HANDLERS
from seohead.sf.core.registry import CHECKS

CHECK_ID = re.compile(r"[A-Z][A-Z0-9_]+$")


def test_reconciliation_covers_every_canonical_gap_row_and_is_current():
    assert len(rows()) == 100
    assert OUT.read_text(encoding="utf-8") == render()


def test_reconciliation_uses_the_actual_mode_column_for_every_state():
    assert Counter(status(row) for row in rows()) == {
        "covered_registry_or_tool": 56,
        "missing": 29,
        "out_of_scope": 6,
        "partial": 9,
    }
    assert [row.row_id for row in priority_rows()] == [
        "14.6",
        "1.1",
        "2.6",
        "4.1",
        "4.4",
        "7.9",
        "12.1",
        "12.2",
        "12.3",
        "12.5",
    ]


def test_every_covered_row_names_a_real_registry_check_or_shared_public_tool():
    checks_reference = (Path(__file__).resolve().parents[1] / "docs" / "CHECKS.md").read_text(
        encoding="utf-8"
    )
    for row in rows():
        if status(row) != "covered_registry_or_tool":
            continue
        refs = evidence_refs(row)
        named_check_ids = {ref for ref in refs if CHECK_ID.fullmatch(ref)}
        checks = {ref for ref in refs if ref in CHECKS}
        commands = {ref for ref in refs if ref in COMMANDS}
        assert checks or commands, row.row_id
        assert named_check_ids <= set(CHECKS)
        assert checks <= set(CHECKS)
        assert all(f"`{check_id}`" in checks_reference for check_id in checks)
        assert commands <= set(COMMANDS)
        assert {command.replace("-", "_") for command in commands} <= set(HANDLERS)


def test_reviewed_measurement_rows_keep_their_evidence_limits():
    reviewed = {row.row_id: row for row in rows()}
    assert status(reviewed["7.12"]) == "covered_registry_or_tool"
    assert "HREFLANG_OUTSIDE_HEAD" in evidence_refs(reviewed["7.12"])
    for row_id, check_id in (("3.4", "A11Y_FORM_LABEL_MISSING"), ("3.5", "A11Y_HEADING_ORDER")):
        assert status(reviewed[row_id]) == "partial"
        assert check_id in evidence_refs(reviewed[row_id])
    for row_id in ("1.2", "1.3", "4.1"):
        assert status(reviewed[row_id]) == "partial"
    assert "field-data coverage" in reviewed["1.2"].mode
    assert "field-data coverage" in reviewed["1.3"].mode
    assert (
        "description and meta-robots/noindex differences remain unmeasured" in reviewed["4.1"].mode
    )
