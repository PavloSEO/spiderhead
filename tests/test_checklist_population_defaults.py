"""Partial agreed populations take defaults; refusals name the missing or unknown keys."""

from __future__ import annotations

import pytest

from seohead.projects.coverage import _population, coverage_status, initialize_coverage
from seohead.projects.workspace import create_project

SITE = "https://example.test/"


def test_sample_needs_only_kind_size_name_and_source():
    value = _population(
        {"kind": "sample", "size": 50, "name": "top sellers", "source": "memo"}, SITE
    )

    assert value["kind"] == "sample"
    assert value["size"] == 50
    assert value["urls"] == []
    assert value["reason"] is None
    assert value["templates"] == {}


def test_missing_source_is_named():
    with pytest.raises(ValueError, match=r"missing required key\(s\): source"):
        _population({"kind": "complete_set", "size": 10}, SITE)


def test_missing_kind_is_named():
    with pytest.raises(ValueError, match=r"missing required key\(s\): kind"):
        _population({"size": 10, "source": "memo"}, SITE)


def test_sample_without_size_or_urls_names_size():
    with pytest.raises(ValueError, match=r"missing required key\(s\): size \(or urls\)"):
        _population({"kind": "sample", "name": "x", "source": "memo"}, SITE)


def test_unknown_key_is_named_with_the_allowed_set():
    with pytest.raises(ValueError, match=r"unsupported key\(s\): colour.*allowed:"):
        _population({"kind": "complete_set", "size": 10, "source": "memo", "colour": "red"}, SITE)


def test_partial_population_in_a_checklist_plan_is_stored(tmp_path):
    root = tmp_path / "project"
    create_project(root, SITE)
    initialize_coverage(
        root,
        plan={
            "reviewer": "Lead auditor",
            "population": {"kind": "complete_set", "size": 300, "source": "crawl memo"},
        },
    )
    stored = coverage_status(root)["plan"]["population"]

    assert stored["kind"] == "complete_set"
    assert stored["size"] == 300
    assert stored["source"] == "crawl memo"


def test_nested_template_population_defaults_apply(tmp_path):
    value = _population(
        {
            "kind": "complete_set",
            "size": 10,
            "source": "memo",
            "templates": {"product": {"kind": "sample", "size": 4, "name": "p", "source": "memo"}},
        },
        SITE,
    )
    assert value["templates"]["product"]["size"] == 4
    assert value["templates"]["product"]["urls"] == []
