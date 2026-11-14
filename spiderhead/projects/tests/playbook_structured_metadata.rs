//! The `load_catalogue` monkeypatch becomes a real packaged catalogue row
//! (`article-writer` declares only name+description), which exercises the
//! same metadata-gap reporting path.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::json;
use spiderhead_projects::playbooks::{playbook_metadata, playbook_show};

#[test]
fn frontmatter_metadata_is_structured_without_inferring_missing_fields() {
    let content = "---
name: sample
description: >-
  A sample method for synthetic fixtures.
version: 2.1.0
---

# Sample

## Trigger
Run this for synthetic samples.
";
    let fields = playbook_metadata(content);
    assert_eq!(
        fields,
        json!({
            "description": "A sample method for synthetic fixtures.",
            "when": null,
            "requires": null,
            "version": "2.1.0",
        })
        .as_object()
        .unwrap()
        .clone()
    );
}

#[test]
fn playbook_show_reports_metadata_gaps_as_unavailable() {
    let result = playbook_show("article-writer", "skill").expect("show");
    assert!(!result["description"].is_null());
    assert!(result["when"].is_null());
    assert!(result["requires"].is_null());
    assert!(result["version"].is_null());
    assert_eq!(
        result["metadata_unavailable"],
        json!(["when", "requires", "version"])
    );
}
