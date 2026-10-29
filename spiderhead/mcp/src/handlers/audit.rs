//! The bulk site audit: the composite over this server's own live checks.
//!
//! `site_audit` delegates to `audit_site` with the same handler
//! registry that serves `tools/call`; every tool call therefore goes through
//! the same dispatch the surface already exposes, and a tool this build has
//! not ported simply does not run.

use serde_json::{json, Map, Value};

use spiderhead_checks::site_audit::{audit_site, AuditOptions, SiteTools, ToolCall};

use super::{dispatch, kwargs, opt, opt_bool, opt_str, ToolError};

/// The registry names the audit composes; a missing name means the tool does
/// not run (a missing handler skips the call the same way).
const PORTED_TOOLS: [&str; 12] = [
    "domain_profile",
    "robots_check",
    "sitemap_crawl",
    "cdn_check",
    "tech_detect",
    "security_check",
    "ai_bots_check",
    "llms_txt_check",
    "regions_check",
    "parse",
    "schema_check",
    "social_meta_check",
];

/// The audit's tool seam over this crate's `dispatch`.
struct LiveSiteTools;

impl SiteTools for LiveSiteTools {
    fn has(&self, tool: &str) -> bool {
        PORTED_TOOLS.contains(&tool)
    }

    fn call(&self, tool: &str, call: &ToolCall) -> Result<Value, String> {
        // The keyword arguments each tool is invoked with depend on the tool:
        // domain-level tools take the host, the regional
        // check honors the render flag and a bounded sample, and the sitemap
        // crawl is pointed at the declared sitemap when one was selected.
        let mut args = Map::new();
        match tool {
            "domain_profile" => {
                args.insert("domain".to_string(), json!(call.domain));
            }
            "regions_check" => {
                args.insert("url".to_string(), json!(call.url));
                args.insert("limit".to_string(), json!(8));
                args.insert("render".to_string(), json!(call.render));
            }
            _ => {
                let target = if tool == "sitemap_crawl" && !call.sitemap_url.is_empty() {
                    call.sitemap_url
                } else {
                    call.url
                };
                args.insert("url".to_string(), json!(target));
            }
        }
        dispatch(tool, &Value::Object(args)).map_err(|error| error.to_string())
    }
}

/// The bulk site audit over this server's live checks.
pub fn site_audit(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    // A missing or empty URL is rejected up front.
    let url = opt_str(&args, "url")
        .filter(|url| !url.is_empty())
        .ok_or_else(|| ToolError::Failed("url required (site home page)".to_string()))?;
    let urls: Vec<String> = opt(&args, "urls")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let limit = opt(&args, "limit").and_then(Value::as_i64).unwrap_or(25);
    let concurrency = opt(&args, "concurrency")
        .and_then(Value::as_i64)
        .unwrap_or(5);
    let render = opt_bool(&args, "render").unwrap_or(false);
    let skip: Vec<String> = opt(&args, "skip")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let crux_evidence = opt(&args, "crux_evidence");

    audit_site(
        url,
        &AuditOptions {
            urls: &urls,
            limit,
            concurrency,
            render,
            skip: &skip,
            crux_evidence,
        },
        &LiveSiteTools,
    )
    .map_err(ToolError::Failed)
}

/// `segment_diff`: which pages of one declared segment have a counterpart in
/// another. The audit's own recorded `scope.segments` / `scope.segments_only`
/// rebuild the crawl `Scope` here; only its `segment_for`/`rejection` lookups
/// reach the analyzer, which never imports the crawl module.
pub fn segment_diff(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let audit = args
        .get("audit")
        .filter(|value| !value.is_null())
        .cloned()
        .ok_or_else(|| ToolError::Failed("audit required".to_owned()))?;
    let source = opt_str(&args, "source");
    let target = opt_str(&args, "target");
    let (Some(source), Some(target)) = (source, target) else {
        return Err(ToolError::Failed(
            "source and target segment names required".to_owned(),
        ));
    };
    let doc = spiderhead_store::inputs::load_audit_document(&audit, "audit", None)
        .map_err(super::failed)?;
    let empty = Value::Object(Map::new());
    let run = doc.get("run").unwrap_or(&empty);
    let crawl_config = run.get("crawl_config").unwrap_or(&empty);
    let segments_cfg = crawl_config
        .get("scope.segments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let segments_only: Vec<String> = crawl_config
        .get("scope.segments_only")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let mut scope_config = std::collections::BTreeMap::new();
    scope_config.insert("segments".to_owned(), json!(segments_cfg));
    scope_config.insert("segments_only".to_owned(), json!(segments_only));
    let scope = spiderhead_store::crawl_records::spider::Scope::from_config(Some(&scope_config));
    let source_url = run.get("source").and_then(Value::as_str).unwrap_or("");
    let start_host = spiderhead_store::ledger::urlparts::urlsplit(source_url)
        .hostname()
        .unwrap_or_default();
    let pages = doc
        .get("pages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let crawl_partial = run
        .get("crawl_partial")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let segment_for = |url: &str| scope.segment_for(url);
    let rejection = |url: &str| scope.rejection(url, &start_host);
    spiderhead_sf_import::segment_diff::diff_segments(
        &pages,
        source,
        target,
        &segments_cfg,
        &segment_for,
        if start_host.is_empty() {
            None
        } else {
            Some(&rejection)
        },
        &segments_only,
        crawl_partial,
    )
    .map_err(|error| ToolError::Failed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_url_is_a_handler_failure() {
        for args in [json!({}), json!({"url": ""}), json!({"url": null})] {
            let error = site_audit(&args).unwrap_err();
            assert_eq!(error.to_string(), "url required (site home page)");
        }
    }

    #[test]
    fn audit_result_shape_is_the_composite_document() {
        // A live dispatch would reach the network; the handler seam itself is
        // what this unit pins down. An unreachable host fails the audit before
        // any tool call (input validation happens first), so the document
        // still comes back structured rather than as a handler error.
        let result = site_audit(&json!({"url": "not a URL"})).expect("structured result");
        assert_eq!(result["ok"], json!(false));
        assert!(result["error"]
            .as_str()
            .is_some_and(|text| text.contains("does not look like a domain")));
    }
}
