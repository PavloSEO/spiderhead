"""The coverage map must describe the code, not a memory of it.

A map that names a check id which no longer exists is worse than no map: it reads as coverage
and delivers none. These tests make a rename break the build rather than the document.
"""

from __future__ import annotations

import pathlib

from seohead.cli import COMMANDS
from seohead.mcp.tool_reference import load_seo_tools
from seohead.sf.core.registry import CHECKS
from seohead.sf.core.sf_coverage_reference import render
from seohead.sf.core.sf_issue_map import CATEGORIES, STATUSES, coverage_counts, entries

ROOT = pathlib.Path(__file__).resolve().parent.parent
TOTAL_PUBLISHED_ISSUES = 320


def _skill_names() -> set[str]:
    return {p.parent.name for p in (ROOT / ".claude" / "skills").glob("*/SKILL.md")}


def test_every_published_issue_is_accounted_for():
    assert len(entries()) == TOTAL_PUBLISHED_ISSUES


def test_every_entry_carries_exactly_one_known_status():
    for category, entry in entries():
        assert entry.status in STATUSES, (category, entry.name, entry.status)


def test_every_referenced_check_id_exists():
    for category, entry in entries():
        if entry.status != "check" and not (entry.status == "partial" and entry.refs):
            continue
        for ref in entry.refs:
            assert ref in CHECKS, (
                f"{category}/{entry.name} names a check that does not exist: {ref}"
            )


def test_every_referenced_tool_is_a_real_command_or_skill():
    commands = set(COMMANDS)
    skills = _skill_names()
    for category, entry in entries():
        if entry.status != "tool":
            continue
        for ref in entry.refs:
            assert ref in commands or ref in skills, (
                f"{category}/{entry.name} names {ref!r}, which is neither a CLI command nor a skill"
            )


def test_a_tool_entry_names_what_finds_it():
    for category, entry in entries():
        if entry.status == "tool":
            assert entry.refs, f"{category}/{entry.name} claims a tool finds it but names none"


def test_anything_not_a_plain_check_explains_itself():
    """A gap or a decision with no sentence behind it is a status nobody can argue with."""
    for category, entry in entries():
        if entry.status in ("gap", "out_of_scope", "partial"):
            assert entry.note.strip(), (
                f"{category}/{entry.name} has status {entry.status} and no note"
            )


def test_no_entry_claims_both_a_check_and_a_gap():
    for _category, entry in entries():
        if entry.status == "gap":
            assert not entry.refs


def test_the_counts_add_up():
    counts = coverage_counts()
    assert sum(counts.values()) == TOTAL_PUBLISHED_ISSUES
    # The map is only worth having while it is mostly coverage rather than mostly apology.
    in_scope = TOTAL_PUBLISHED_ISSUES - len(CATEGORIES["Accessibility"]) - len(CATEGORIES["AMP"])
    found = counts["check"] + counts["tool"]
    assert found > in_scope / 2, "over half of the in-scope catalogue should be found today"


def test_the_generated_document_is_current():
    committed = (ROOT / "docs" / "COVERAGE_SF_ISSUES.md").read_text(encoding="utf-8")
    assert committed == render(), (
        "docs/COVERAGE_SF_ISSUES.md is stale: run scripts/generate_sf_coverage.py"
    )


def test_the_document_names_every_in_scope_issue():
    committed = (ROOT / "docs" / "COVERAGE_SF_ISSUES.md").read_text(encoding="utf-8")
    for category, entry in entries():
        assert entry.name in committed, f"{category}/{entry.name} is missing from the document"


def test_seo_tool_names_are_not_confused_with_command_names():
    """The map names CLI commands; the MCP surface adds a seo_ prefix. Mixing the two would
    make a reference look valid while pointing at nothing a reader can run."""
    mcp_names = {tool.name for tool in load_seo_tools()}
    for _category, entry in entries():
        for ref in entry.refs:
            assert ref not in mcp_names, f"{ref} is an MCP tool name; name the CLI command"


def test_network_payload_entry_does_not_overclaim_html_only_coverage():
    """Declared-resource lower bounds do not prove a complete runtime payload."""
    entry = next(
        entry
        for category, entry in entries()
        if category == "PageSpeed" and entry.name == "Avoid Enormous Network Payloads"
    )
    assert entry.status == "partial"
    assert set(entry.refs) == {"HEAVY_PAGE_WEIGHT", "LARGE_HTML", "HTML_BLOAT"}
    assert "total page payload" in entry.note
