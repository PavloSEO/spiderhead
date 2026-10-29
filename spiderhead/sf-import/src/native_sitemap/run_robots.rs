//! `_run_robots`: internally-linked robots-blocked URLs and blocked rendering
//! dependencies, over the selected-links CTE.

use serde_json::Value;
use spiderhead_parse::py_url::urlsplit;

use crate::context::{AddOpts, AuditContext, ContextError};
use crate::normalize::Object;

use super::robots_policy::blocked;
use super::scan_rows::{gather_scan_rows, ScanRows};

pub(crate) fn run_robots(
    ctx: &mut AuditContext,
    robots: Option<&Object>,
    start_url: &str,
) -> Result<(), ContextError> {
    let ScanRows {
        links,
        resources,
        has_resource_refs,
        has_complete_inventory,
    } = gather_scan_rows(ctx)?;
    let Ok(start) = urlsplit(start_url) else {
        return Ok(());
    };
    let start_netloc = start.netloc.to_lowercase();
    let mut explicit = false;
    for (source, target, status, has_payload) in links {
        let same_netloc = urlsplit(&target)
            .map(|t| t.netloc.to_lowercase() == start_netloc)
            .unwrap_or(false);
        if !same_netloc {
            continue;
        }
        if has_payload || blocked(robots, &target, start_url) {
            explicit = explicit || has_payload;
            let mut details = Object::new();
            details.insert("source_url".into(), Value::from(source.clone()));
            details.insert("target_url".into(), Value::from(target.clone()));
            details.insert(
                "status_code".into(),
                status.map(Value::from).unwrap_or(Value::Null),
            );
            details.insert(
                "reason".into(),
                Value::from("internal link targets a robots-blocked URL"),
            );
            details.insert("evidence_source".into(), Value::from("retained_scan"));
            let mut location = Object::new();
            location.insert("source_url".into(), Value::from(source));
            location.insert("destination_url".into(), Value::from(target.clone()));
            let mut evidence = Object::new();
            evidence.insert("retained_robots".into(), Value::from(true));
            ctx.add(
                "INTERNALLY_LINKED_ROBOTS_BLOCKED",
                AddOpts {
                    target_url: Some(target),
                    status_code: status,
                    details,
                    locations: vec![location],
                    evidence,
                    ..AddOpts::default()
                },
            )?;
        }
    }
    for (source, target, kind, capture_state, reason) in resources {
        if capture_state == "excluded_robots" || blocked(robots, &target, start_url) {
            let mut details = Object::new();
            details.insert("source_url".into(), Value::from(source.clone()));
            details.insert("target_url".into(), Value::from(target));
            details.insert("kind".into(), Value::from(kind));
            details.insert(
                "reason".into(),
                Value::from("rendering dependency blocked by retained robots.txt"),
            );
            details.insert(
                "capture_reason".into(),
                reason.map(Value::from).unwrap_or(Value::Null),
            );
            details.insert("evidence_source".into(), Value::from("retained_scan"));
            let mut evidence = Object::new();
            evidence.insert("retained_resource_declaration".into(), Value::from(true));
            ctx.add(
                "ROBOTS_BLOCKS_RESOURCES",
                AddOpts {
                    target_url: Some(source),
                    details,
                    evidence,
                    ..AddOpts::default()
                },
            )?;
        }
    }
    if robots.is_none() {
        if !explicit {
            ctx.skip(
                "INTERNALLY_LINKED_ROBOTS_BLOCKED",
                "no retained fetched robots.txt policy",
            );
        }
        ctx.skip(
            "ROBOTS_BLOCKS_RESOURCES",
            "no retained fetched robots.txt policy or blocked dependency",
        );
    } else if !has_resource_refs && !has_complete_inventory {
        ctx.skip(
            "ROBOTS_BLOCKS_RESOURCES",
            "no retained script or stylesheet dependency inventory",
        );
    }
    Ok(())
}
