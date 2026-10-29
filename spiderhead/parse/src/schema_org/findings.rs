//! Human-readable findings of a schema check.

use serde_json::Value;

use super::{Json, SchemaCheck};
use crate::py_json::str_or_dumps;

fn plural_join(items: &[Json], take: usize) -> String {
    items
        .iter()
        .take(take)
        .map(str_or_dumps)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Human-readable findings for the check result.
pub(super) fn findings(r: &SchemaCheck) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(status) = r.status_code {
        if !(200..300).contains(&status) {
            out.push(format!(
                "The page returned HTTP {status}: what follows describes the error page, \
                 not the requested URL"
            ));
        }
    }
    let parse_errors = r.parse_errors.clone().unwrap_or_default();
    out.extend(parse_errors.iter().map(str_or_dumps));
    for coverage in r.markup_coverage.iter().flatten() {
        for reason in coverage.reasons.iter().flatten() {
            out.push(format!(
                "{} validation coverage is partial: {reason}",
                coverage.format
            ));
        }
    }
    let blocks = r.blocks.unwrap_or(0);
    let entities = r.entities.clone().unwrap_or_default();
    if blocks == 0 {
        let found = r.blocks_found.unwrap_or(0);
        if found > 0 {
            out.push(format!(
                "All {found} JSON-LD block(s) on the page are invalid, so every search engine \
                 discards the structured data this page believes it publishes"
            ));
        } else {
            out.push("The page contains no JSON-LD blocks".to_owned());
        }
        if entities.is_empty() {
            return out;
        }
    }
    let count = |key: &str| -> usize {
        entities
            .iter()
            .map(|e| e[key].as_array().map_or(0, Vec::len))
            .sum()
    };
    let (errors, warnings) = (count("errors"), count("warnings"));
    if errors > 0 {
        out.push(format!("Vocabulary errors: {errors}"));
    }
    if warnings > 0 {
        out.push(format!("Vocabulary warnings: {warnings}"));
    }
    if let Some(g) = &r.graph {
        if g.top_level > 1 && !g.is_graph {
            out.push(format!(
                "{} top-level entities are marked up, but none are linked through @id; \
                 this is a set of isolated blocks rather than a graph",
                g.top_level
            ));
        } else if !g.islands.is_empty() {
            out.push(format!(
                "Entities outside the connected graph: {} ({})",
                g.islands.len(),
                plural_join(&g.islands, 3)
            ));
        }
    }
    let rich = r.rich_results.clone().unwrap_or_default();
    let list = |v: &Json| -> String {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    for rr in &rich {
        let kind = rr["type"].as_str().unwrap_or("");
        if rr["missing_required"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            out.push(format!(
                "{kind}: missing required rich-result fields: {}",
                list(&rr["missing_required"])
            ));
        }
        if rr["missing_required_any_of"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            out.push(format!(
                "{kind}: {} needs at least one of {}",
                rr["google_feature"].as_str().unwrap_or(""),
                list(&rr["missing_required_any_of"])
            ));
        }
        if let Some(note) = rr.get("note").and_then(Value::as_str) {
            out.push(format!("{kind}: {note}"));
        }
    }
    if rich.iter().any(|rr| rr["eligible"].as_bool() == Some(true)) {
        out.push(
            "Rich-result checks above verify only locally observable structure \
             (required properties present for a type Google documents); they are not \
             a guarantee that Google will display a rich result for this page"
                .to_owned(),
        );
    }
    out
}
