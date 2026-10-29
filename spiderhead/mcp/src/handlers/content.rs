//! Handlers of the content checks: redirect rule generation, Markdown rendering,
//! boilerplate-template grouping and structured-data validation.

use serde_json::{json, Value};

use spiderhead_parse::boilerplate_report::{boilerplate_consistency_report, BoilerplatePage};
use spiderhead_parse::markdown_extract::extract_markdown;
use spiderhead_parse::parser::{fetch_html, FetchOutcome};
use spiderhead_parse::schema_org::{check_schema, SchemaSource};
use spiderhead_parse::types::ContentAreaConfig;

use crate::error::ToolError;

use super::parse::HttpFetcher;
use super::{kwargs, opt, opt_str};

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::Failed(error.to_string())
}

/// `redirects_generate`: web-server rules plus the local chain/loop analysis.
pub fn redirects_generate(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let items: Vec<Value> = opt(&args, "redirects")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let format = opt_str(&args, "fmt").unwrap_or("apache-rewrite-rule");
    let default_url = opt_str(&args, "default_url").unwrap_or("/");
    let custom_template = opt_str(&args, "custom_template").unwrap_or("");
    let rules = spiderhead_checks::redirect_rules::generate_rules(
        &items,
        format,
        default_url,
        custom_template,
    );
    let analysis = spiderhead_checks::redirect_rules::analyze_redirect_map(&items, default_url)
        .map_err(failed)?;
    Ok(json!({"rules": rules, "analysis": analysis}))
}

/// The content-area override of a call, when supplied.
fn content_area(
    args: &serde_json::Map<String, Value>,
) -> Result<Option<ContentAreaConfig>, ToolError> {
    match opt(args, "content_area") {
        None => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(failed),
    }
}

/// The fetched HTML of `url`, or the failure document handlers answer with.
fn fetched_html(url: &str) -> Result<Result<(String, String, u16), Value>, ToolError> {
    let fetcher = HttpFetcher::new().map_err(ToolError::Failed)?;
    Ok(match fetch_html(&fetcher, url, None) {
        FetchOutcome::Completed {
            final_url,
            status_code,
            html,
            ..
        } if (200..300).contains(&status_code) => Ok((html, final_url, status_code)),
        FetchOutcome::Completed { .. } => {
            Err(json!({"ok": false, "url": url, "error": "fetch failed"}))
        }
        FetchOutcome::Failed { error, .. } => Err(json!({"ok": false, "url": url, "error": error})),
    })
}

/// `markdown_extract`: content-area and full-document Markdown of a page.
pub fn markdown_extract(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let url = opt_str(&args, "url").filter(|u| !u.is_empty());
    let html = opt_str(&args, "html");
    if url.is_none() && html.is_none_or(str::is_empty) {
        return Err(ToolError::Failed("url or html required".to_owned()));
    }
    let area = content_area(&args)?;
    let rendered = |html: &str| -> Result<Value, ToolError> {
        serde_json::to_value(extract_markdown(html, area.as_ref()).map_err(failed)?).map_err(failed)
    };
    if let Some(html) = html {
        let mut document = json!({"ok": true});
        merge(&mut document, &rendered(html)?);
        return Ok(document);
    }
    let url = url.unwrap_or_default();
    match fetched_html(url)? {
        Err(document) => Ok(document),
        Ok((html, final_url, status_code)) => {
            let mut document = json!({
                "ok": true,
                "url": url,
                "final_url": final_url,
                "status_code": status_code,
            });
            merge(&mut document, &rendered(&html)?);
            Ok(document)
        }
    }
}

/// `boilerplate_report`: group pages by header/nav/footer hash.
pub fn boilerplate_report(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    if opt(&args, "pages").is_some() && opt(&args, "scan").is_some() {
        return Err(ToolError::Failed(
            "pages[] and scan are mutually exclusive".to_owned(),
        ));
    }
    if opt(&args, "scan").is_some() {
        return Err(ToolError::NotYetPorted("boilerplate_report".to_owned()));
    }
    let pages: Vec<BoilerplatePage> = match opt(&args, "pages").and_then(Value::as_array) {
        Some(list) if !list.is_empty() => list
            .iter()
            .map(|page| serde_json::from_value(page.clone()).map_err(failed))
            .collect::<Result<_, _>>()?,
        _ => {
            return Err(ToolError::Failed(
                "pages[] required (list of {url, html} or {url, hash})".to_owned(),
            ))
        }
    };
    serde_json::to_value(boilerplate_consistency_report(&pages).map_err(failed)?).map_err(failed)
}

/// `schema_check`: vocabulary, entity graph and rich-result readiness of one page.
pub fn schema_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let url = opt_str(&args, "url").filter(|u| !u.is_empty());
    let html = opt_str(&args, "html").filter(|h| !h.is_empty());
    let fetcher = HttpFetcher::new().map_err(ToolError::Failed)?;
    let result = match (url, html) {
        (None, None) => return Err(ToolError::Failed("url or html required".to_owned())),
        (Some(url), _) => check_schema(SchemaSource::Url(url), Some(&fetcher), 20.0, None),
        (None, Some(html)) => check_schema(SchemaSource::Html(html), None, 20.0, None),
    };
    serde_json::to_value(result.map_err(failed)?).map_err(failed)
}

fn merge(into: &mut Value, from: &Value) {
    if let (Some(out), Some(object)) = (into.as_object_mut(), from.as_object()) {
        for (key, value) in object {
            out.insert(key.clone(), value.clone());
        }
    }
}

/// `citability_check`: score whether content can support a cited answer.
pub fn citability_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let url = opt_str(&args, "url").filter(|u| !u.is_empty());
    let text = opt_str(&args, "text");
    if url.is_none() && text.is_none_or(str::is_empty) {
        return Err(ToolError::Failed("url or text required".to_owned()));
    }
    if let Some(text) = text {
        return Ok(spiderhead_checks::citability::score_citability(text));
    }
    let url = url.unwrap_or_default();
    let area = content_area(&args)?;
    match fetched_html(url)? {
        Err(document) => Ok(document),
        Ok((html, _, _)) => {
            let markdown = extract_markdown(&html, area.as_ref()).map_err(failed)?;
            let mut document = json!({"url": url});
            merge(
                &mut document,
                &spiderhead_checks::citability::score_citability(&markdown.content_markdown),
            );
            Ok(document)
        }
    }
}

/// `social_meta_check`: Open Graph / Twitter Card fields a stable preview needs.
pub fn social_meta_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let url = opt_str(&args, "url").filter(|u| !u.is_empty());
    let (og, twitter) = (opt(&args, "og"), opt(&args, "twitter"));
    if url.is_none() && og.is_none() && twitter.is_none() {
        return Err(ToolError::Failed("url or og/twitter required".to_owned()));
    }
    let empty = serde_json::Map::new();
    if og.is_some() || twitter.is_some() {
        return Ok(spiderhead_checks::social_meta::check_social_meta(
            og.and_then(Value::as_object).unwrap_or(&empty),
            twitter.and_then(Value::as_object).unwrap_or(&empty),
        ));
    }
    let url = url.unwrap_or_default();
    let parsed = super::dispatch(
        "parse",
        &json!({
            "url": url,
            "options": {
                "meta": false, "canonical": false, "og": true, "headings": false,
                "jsonld": false, "links": false, "text": false,
            },
        }),
    )?;
    let page = &parsed["results"][0];
    if page["ok"] != true {
        return Ok(json!({
            "ok": false,
            "url": url,
            "error": page.get("error").cloned().unwrap_or(json!("parse failed")),
        }));
    }
    let mut document = spiderhead_checks::social_meta::check_social_meta(
        page["og"].as_object().unwrap_or(&empty),
        page["twitter"].as_object().unwrap_or(&empty),
    );
    document["url"] = json!(url);
    Ok(document)
}
