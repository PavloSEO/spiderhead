//! `scan_link_inspect`: one bounded saved-scan link query (path, inlinks,
//! context, links views), shared by the CLI and the local MCP surface.

use std::path::Path;

use serde_json::{json, Map, Value};

use spiderhead_store::error::StoreError;
use spiderhead_store::{link_context, link_queries, url_links};

use crate::error::ToolError;

use super::{need_str, opt_float, opt_int, opt_str};

const DEFAULT_BODY: i64 = 5 * 1024 * 1024;
const PARAMS: [&str; 24] = [
    "input_path",
    "view",
    "seed",
    "target",
    "representation",
    "cursor",
    "link_id",
    "document_id",
    "offset",
    "limit",
    "max_bytes",
    "max_body_bytes",
    "max_nodes",
    "max_edges",
    "max_depth",
    "timeout_seconds",
    "url",
    "direction",
    "link_type",
    "follow",
    "status_class",
    "contains",
    "sort",
    "preset",
];

/// A refusal raised before or inside the query: the `ok: false` answer.
struct Refusal {
    message: String,
    reason_code: Option<String>,
}

impl Refusal {
    fn new(message: &str) -> Refusal {
        Refusal {
            message: message.to_owned(),
            reason_code: None,
        }
    }
}

fn from_store(error: StoreError) -> Refusal {
    let reason_code = match &error {
        StoreError::LinksError(e) => Some(e.reason_code.clone()),
        StoreError::QueryError(e) => Some(e.reason_code.clone()),
        _ => None,
    };
    let message = match &error {
        StoreError::LinksError(e) => e.message.clone(),
        StoreError::QueryError(e) => e.message.clone(),
        other => super::plain(other),
    };
    Refusal {
        message,
        reason_code,
    }
}

/// The preset's fixed link filters; an explicit conflicting filter is refused.
fn apply_preset(
    preset: &str,
    direction: &mut String,
    link_type: &mut String,
    status_class: &mut String,
) -> Result<(), Refusal> {
    let (want_direction, want_status) = match preset {
        "inlinks" => ("in", "all"),
        "broken_links" => ("out", "broken"),
        _ => return Err(Refusal::new("preset must be inlinks or broken_links")),
    };
    for (key, current, default, wanted) in [
        ("direction", &*direction, "out", want_direction),
        ("link_type", &*link_type, "all", "internal"),
        ("status_class", &*status_class, "all", want_status),
    ] {
        if current != default && current != wanted {
            return Err(Refusal::new(&format!(
                "preset {preset} conflicts with explicit {key}"
            )));
        }
    }
    *direction = want_direction.to_owned();
    *link_type = "internal".to_owned();
    *status_class = want_status.to_owned();
    Ok(())
}

/// `scan_link_inspect`: dispatch one of the four link views.
pub fn scan_link_inspect(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let input = need_str(&args, "input_path", "scan-link-inspect", &PARAMS)?.to_owned();
    let view = opt_str(&args, "view").unwrap_or("path").to_owned();
    if !["path", "inlinks", "context", "links"].contains(&view.as_str()) {
        return Ok(json!({
            "ok": false,
            "view": "invalid",
            "error": "view must be path, inlinks, context, or links",
        }));
    }
    match inspect(&args, &input, &view) {
        Ok(answer) => Ok(answer),
        Err(refusal) => {
            let mut failure = Map::new();
            failure.insert("ok".into(), json!(false));
            failure.insert("view".into(), json!(view));
            failure.insert("error".into(), json!(refusal.message));
            if let Some(code) = refusal.reason_code {
                failure.insert("reason_code".into(), json!(code));
            }
            Ok(Value::Object(failure))
        }
    }
}

fn inspect(args: &Map<String, Value>, input: &str, view: &str) -> Result<Value, Refusal> {
    if input.is_empty() {
        return Err(Refusal::new("scan is required"));
    }
    let text = |name: &str, default: &str| opt_str(args, name).unwrap_or(default).to_owned();
    let url = opt_str(args, "url");
    let contains = opt_str(args, "contains");
    let preset = opt_str(args, "preset");
    let mut direction = text("direction", "out");
    let mut link_type = text("link_type", "all");
    let follow = text("follow", "all");
    let mut status_class = text("status_class", "all");
    let sort = text("sort", "order");
    let links_only = url.is_some()
        || direction != "out"
        || link_type != "all"
        || follow != "all"
        || status_class != "all"
        || contains.is_some()
        || sort != "order"
        || preset.is_some();
    if links_only && view != "links" {
        return Err(Refusal::new(
            "url, direction and link filters belong to view=links",
        ));
    }
    let max_bytes = opt_int(args, "max_bytes").unwrap_or(1_048_576);
    if !(4096..=8 * 1024 * 1024).contains(&max_bytes) {
        return Err(Refusal::new("max_bytes must be 4096..8388608"));
    }
    let representation = text("representation", "all");
    let seed = opt_str(args, "seed");
    let target = opt_str(args, "target");
    let cursor = opt_str(args, "cursor");
    let link_id = opt_int(args, "link_id");
    let document_id = opt_int(args, "document_id");
    let offset = opt_int(args, "offset").unwrap_or(0);
    let limit = opt_int(args, "limit").unwrap_or(100);
    let max_body = opt_int(args, "max_body_bytes").unwrap_or(DEFAULT_BODY);
    let max_nodes = opt_int(args, "max_nodes").unwrap_or(10_000);
    let max_edges = opt_int(args, "max_edges").unwrap_or(200_000);
    let max_depth = opt_int(args, "max_depth").unwrap_or(20);
    let timeout = opt_float(args, "timeout_seconds").unwrap_or(15.0);
    let default_budgets =
        (max_nodes, max_edges, max_depth, max_body) == (10_000, 200_000, 20, DEFAULT_BODY);
    let path = Path::new(input);
    let mut result = match view {
        "links" => {
            let Some(url) = url.filter(|u| !u.is_empty()) else {
                return Err(Refusal::new("links view requires a url"));
            };
            if let Some(preset) = preset {
                apply_preset(preset, &mut direction, &mut link_type, &mut status_class)?;
            }
            let has_ids = [seed, target, cursor].iter().any(Option::is_some)
                || link_id.is_some()
                || document_id.is_some();
            if has_ids || !default_budgets {
                return Err(Refusal::new(
                    "links view does not accept seed, target, cursor, IDs or budgets",
                ));
            }
            url_links::url_links(
                path,
                url,
                &direction,
                &representation,
                &link_type,
                &follow,
                &status_class,
                contains,
                &sort,
                offset,
                limit,
                timeout,
                url_links::COUNT_CAP,
                url_links::SCAN_CAP,
            )
        }
        "path" => {
            let (Some(seed), Some(target)) = (
                seed.filter(|s| !s.is_empty()),
                target.filter(|s| !s.is_empty()),
            ) else {
                return Err(Refusal::new("path view requires seed and target URLs"));
            };
            if cursor.is_some()
                || link_id.is_some()
                || document_id.is_some()
                || offset != 0
                || limit != 100
            {
                return Err(Refusal::new(
                    "path view does not accept cursor, link/document ID, offset or limit",
                ));
            }
            if max_body != DEFAULT_BODY {
                return Err(Refusal::new("path view does not read a body"));
            }
            link_queries::shortest_observed_path(
                path,
                seed,
                target,
                &representation,
                max_nodes,
                max_edges,
                max_depth,
                timeout,
            )
        }
        "inlinks" => {
            let Some(target) = target.filter(|s| !s.is_empty()) else {
                return Err(Refusal::new("inlinks view requires a target URL"));
            };
            if seed.is_some() || link_id.is_some() || document_id.is_some() || offset != 0 {
                return Err(Refusal::new(
                    "inlinks view does not accept seed, link/document ID or offset",
                ));
            }
            if !default_budgets {
                return Err(Refusal::new(
                    "inlinks view does not accept path or body budgets",
                ));
            }
            link_queries::reverse_inlinks(
                path,
                target,
                &representation,
                cursor,
                limit,
                max_bytes,
                timeout,
            )
        }
        _ => {
            if link_id.is_some() == document_id.is_some() {
                return Err(Refusal::new(
                    "context view requires exactly one of link_id or document_id",
                ));
            }
            if seed.is_some() || target.is_some() || cursor.is_some() {
                return Err(Refusal::new(
                    "context view does not accept seed, target or cursor",
                ));
            }
            if (max_nodes, max_edges, max_depth, timeout) != (10_000, 200_000, 20, 15.0) {
                return Err(Refusal::new("context view does not accept path budgets"));
            }
            let context = if let Some(link_id) = link_id {
                if offset != 0 || limit != 100 {
                    return Err(Refusal::new(
                        "single-link context does not accept offset or limit",
                    ));
                }
                link_context::context_for_link(path, link_id, max_body, max_bytes)
            } else {
                link_context::contexts_for_document(
                    path,
                    document_id.unwrap_or(0),
                    offset,
                    limit,
                    max_body,
                    max_bytes,
                )
            };
            let context = context.map_err(from_store)?;
            if representation != "all"
                && context.get("representation").and_then(Value::as_str) != Some(&representation)
            {
                return Err(Refusal::new(
                    "context representation differs from the requested filter",
                ));
            }
            Ok(context)
        }
    }
    .map_err(from_store)?;
    let mut answer = Map::new();
    answer.insert("ok".into(), json!(true));
    answer.insert("view".into(), json!(view));
    let scan_uuid = result.get("scan_uuid").cloned().unwrap_or(Value::Null);
    let revision = result
        .get("evidence_revision")
        .cloned()
        .unwrap_or(Value::Null);
    answer.append(&mut result);
    if let Some(preset) = preset {
        answer.insert("preset".into(), json!(preset));
    }
    let answer = Value::Object(answer);
    let size = spiderhead_core::common::python_json(&answer).len() as i64;
    if size > max_bytes {
        return Ok(json!({
            "ok": false,
            "view": view,
            "state": "limit_reached",
            "reason": "output_byte_limit_exceeded",
            "scan_uuid": scan_uuid,
            "evidence_revision": revision,
            "max_bytes": max_bytes,
            "bytes_required": size,
        }));
    }
    Ok(answer)
}
