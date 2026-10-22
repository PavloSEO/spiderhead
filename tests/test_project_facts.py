"""Recording project stack facts, and the detection that can supply them.

Before this surface existed, ``project.json`` facts could only be typed in at
creation: the priority policy read a ``framework`` fact that nothing could ever
write from evidence. These tests pin the link and the precedence that keeps an
operator's decision above a detector's guess. Every detection here is injected;
no test makes a request.
"""

from __future__ import annotations

import json

import pytest

from seohead.projects.coverage import initialize_coverage
from seohead.projects.facts import DETECTED_PREFIX, project_facts
from seohead.projects.priorities import project_priorities
from seohead.projects.workspace import create_project


def _tools(*, technologies=None, robots=None, detection=None, calls=None):
    """Injected stand-ins for the shared single-page tools."""
    by_category: dict[str, list[dict[str, str]]] = {}
    for row in technologies or []:
        by_category.setdefault(row["category"], []).append(row)

    def robots_check(url, paths=None, **_kwargs):
        if calls is not None:
            calls.append(("robots_check", url))
        return robots if robots is not None else {"ok": True, "path_checks": []}

    def tech_detect(url, **_kwargs):
        if calls is not None:
            calls.append(("tech_detect", url))
        return detection if detection is not None else {"ok": True, "by_category": by_category}

    return {"robots_check": robots_check, "tech_detect": tech_detect}


@pytest.fixture
def project(tmp_path):
    root = tmp_path / "project"
    create_project(root, "https://example.test/")
    return root


def test_detection_records_stack_facts_that_priorities_then_read(project):
    tools = _tools(
        technologies=[
            {"category": "framework", "name": "Next.js", "evidence": "html: /_next/static"},
            {"category": "cms", "name": "WordPress", "evidence": "html: /wp-content/"},
        ]
    )

    result = project_facts(str(project), detect=True, apply=True, tools=tools)

    assert result["applied"] is True
    assert result["detection"]["state"] == "run"
    recorded = {fact["name"]: fact for fact in result["facts"]}
    assert recorded["framework"]["value"] == "Next.js"
    assert recorded["framework"]["provenance"].startswith(DETECTED_PREFIX)
    assert recorded["framework"]["observed_at"] == result["detection"]["observed_at"]
    assert json.loads((project / "project.json").read_text())["facts"] == result["facts"]

    initialize_coverage(project)
    decision = next(
        row
        for row in project_priorities(str(project), full=True)["decisions"]
        if row["id"] == "skill:workflow/js-render-check"
    )
    assert decision["after"] == {"priority": "P0", "priority_origin": "policy"}
    assert decision["consulted_facts"] == ["framework"]


def test_supplied_facts_are_recorded_as_operator_decisions(project):
    result = project_facts(
        str(project),
        facts=[
            {
                "name": "site_type",
                "value": "publisher",
                "provenance": "client brief",
                "observed_at": None,
            }
        ],
        apply=True,
    )

    assert result["applied"] is True
    assert result["changes"] == [
        {
            "name": "site_type",
            "action": "recorded",
            "value": "publisher",
            "provenance": "client brief",
            "reason": "operator-entered decision",
        }
    ]
    assert result["detection"]["state"] == "not_run"


def test_detection_never_overwrites_an_operator_entered_fact(tmp_path):
    root = tmp_path / "decided"
    create_project(
        root,
        "https://example.test/",
        facts=[{"name": "cms", "value": "WordPress", "provenance": "client", "observed_at": None}],
    )
    before = (root / "project.json").read_bytes()
    tools = _tools(
        technologies=[{"category": "cms", "name": "Drupal", "evidence": "header: x-drupal-cache"}]
    )

    result = project_facts(str(root), detect=True, apply=True, tools=tools)

    assert result["applied"] is False
    assert (root / "project.json").read_bytes() == before
    assert result["changes"] == [
        {
            "name": "cms",
            "action": "kept_operator",
            "value": "WordPress",
            "provenance": "client",
            "detected_value": "Drupal",
            "reason": "an operator-entered fact is a decision; detection is evidence",
        }
    ]


def test_a_later_detection_refreshes_its_own_earlier_evidence(project):
    tools = _tools(
        technologies=[{"category": "cms", "name": "Drupal", "evidence": "header: x-drupal-cache"}]
    )
    project_facts(str(project), detect=True, apply=True, tools=tools)

    again = project_facts(str(project), detect=True, apply=True, tools=tools)

    assert [change["action"] for change in again["changes"]] == ["confirmed"]
    assert [fact["value"] for fact in again["facts"]] == ["Drupal"]


def test_an_unavailable_detection_leaves_the_fact_absent_with_a_reason(project):
    tools = _tools(detection={"ok": False, "error": "connection refused"})

    result = project_facts(str(project), detect=True, apply=True, tools=tools)

    assert result["detection"]["state"] == "unavailable"
    assert "connection refused" in result["detection"]["reason"]
    assert result["facts"] == []
    assert result["applied"] is False
    assert json.loads((project / "project.json").read_text())["facts"] == []


def test_a_disallowed_target_is_not_fetched(project):
    calls: list[tuple[str, str]] = []
    tools = _tools(
        robots={"ok": True, "path_checks": [{"path": "/", "allowed": False}]},
        technologies=[{"category": "cms", "name": "Drupal", "evidence": "x"}],
        calls=calls,
    )

    result = project_facts(str(project), detect=True, tools=tools)

    assert result["detection"]["state"] == "unavailable"
    assert "robots.txt disallows" in result["detection"]["reason"]
    assert [name for name, _ in calls] == ["robots_check"]


def test_unreadable_robots_rules_are_a_refusal_to_fetch(project):
    calls: list[tuple[str, str]] = []
    tools = _tools(
        robots={"ok": False, "error": "robots.txt returned 503; rules could not be read"},
        calls=calls,
    )

    result = project_facts(str(project), detect=True, tools=tools)

    assert result["detection"]["state"] == "unavailable"
    assert "503" in result["detection"]["reason"]
    assert [name for name, _ in calls] == ["robots_check"]


def test_ambiguous_candidates_are_reported_instead_of_guessed(project):
    tools = _tools(
        technologies=[
            {"category": "cms", "name": "WordPress", "evidence": "html: /wp-content/"},
            {"category": "framework", "name": "Nuxt", "evidence": "html: /_nuxt/"},
            {"category": "framework", "name": "React", "evidence": "html: data-reactroot"},
        ]
    )

    result = project_facts(str(project), detect=True, apply=True, tools=tools)

    assert result["detection"]["state"] == "partial"
    assert [fact["name"] for fact in result["facts"]] == ["cms"]
    assert result["detection"]["unavailable"] == [
        {
            "name": "framework",
            "reason": "2 framework candidates matched (Nuxt, React); not guessed",
        }
    ]


def test_a_generator_version_label_is_not_a_second_candidate(project):
    tools = _tools(
        technologies=[
            {"category": "cms", "name": "WordPress", "evidence": "html: /wp-content/"},
            {"category": "cms", "name": "WordPress 6.4.2", "evidence": "meta name=generator"},
        ]
    )

    result = project_facts(str(project), detect=True, tools=tools)

    assert [(fact["name"], fact["value"]) for fact in result["facts"]] == [("cms", "WordPress")]


def test_no_signature_match_names_the_missing_category(project):
    result = project_facts(str(project), detect=True, tools=_tools())

    assert result["detection"]["state"] == "partial"
    assert result["facts"] == []
    assert [row["name"] for row in result["detection"]["unavailable"]] == ["cms", "framework"]
    assert "no cms signature matched" in result["detection"]["unavailable"][0]["reason"]


def test_preview_is_offline_and_writes_nothing(project):
    def refuse(**_kwargs):
        raise AssertionError("a preview without detection must not call a network tool")

    before = (project / "project.json").read_bytes()

    result = project_facts(
        str(project),
        facts=[{"name": "cms", "value": "MODX", "provenance": "client", "observed_at": None}],
        tools={"robots_check": refuse, "tech_detect": refuse},
    )

    assert result["applied"] is False
    assert result["detection"] == {
        "state": "not_run",
        "reason": "detection was not requested; no request was made",
        "facts": [],
        "unavailable": [],
    }
    assert (project / "project.json").read_bytes() == before


def test_supplied_facts_cannot_claim_detection_provenance(project):
    with pytest.raises(ValueError, match="claim detection provenance"):
        project_facts(
            str(project),
            facts=[
                {
                    "name": "cms",
                    "value": "WordPress",
                    "provenance": f"{DETECTED_PREFIX} at https://example.test/",
                    "observed_at": None,
                }
            ],
        )


def test_no_input_lists_the_saved_facts(project):
    result = project_facts(str(project))
    assert result["ok"] is True and result["state"] == "saved"
    assert isinstance(result["facts"], list) and result["count"] == len(result["facts"])


def test_detection_requires_its_injected_tools(project):
    with pytest.raises(ValueError, match="requires an injected robots_check tool"):
        project_facts(str(project), detect=True)
