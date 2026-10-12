"""Playbook cards expose only fields explicitly declared by the source."""

from seohead.projects import runtime


def test_frontmatter_metadata_is_structured_without_inferring_missing_fields():
    content = """---
name: sample
description: >-
  A sample method for synthetic fixtures.
version: 2.1.0
---

# Sample

## Trigger
Run this for synthetic samples.
"""
    fields = runtime._playbook_metadata(content)
    assert fields == {
        "description": "A sample method for synthetic fixtures.",
        "when": None,
        "requires": None,
        "version": "2.1.0",
    }


def test_playbook_show_reports_metadata_gaps_as_unavailable(monkeypatch):
    row = {
        "id": "skill:workflow/sample",
        "kind": "skill",
        "title": "Sample",
        "definition_hash": "a" * 64,
        "source": "synthetic.md",
        "content": "---\nname: sample\n---\n\n# Sample\n",
    }
    monkeypatch.setattr(runtime, "load_catalogue", lambda: {row["id"]: row})
    result = runtime.playbook_show("sample")
    assert result["description"] is None
    assert result["when"] is None and result["requires"] is None and result["version"] is None
    assert result["metadata_unavailable"] == ["when", "requires", "version"]
