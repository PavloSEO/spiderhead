//! Handlers of the content checks: redirect rule generation, Markdown rendering,
//! boilerplate-template grouping and structured-data validation.

use serde_json::{json, Map, Value};

use spiderhead_parse::boilerplate_report::{boilerplate_consistency_report, BoilerplatePage};
use spiderhead_parse::markdown_extract::extract_markdown;
use spiderhead_parse::parser::{fetch_html, FetchOutcome, HtmlFetcher};
use spiderhead_parse::schema_org::{check_schema, SchemaSource};
use spiderhead_parse::types::ContentAreaConfig;

use crate::error::ToolError;

use super::parse::HttpFetcher;
use super::{kwargs, opt, opt_float, opt_int, opt_str};

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
pub(crate) fn fetched_html(url: &str) -> Result<Result<(String, String, u16), Value>, ToolError> {
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

/// `schema_build`: suggested connected Schema.org `@graph` over page facts.
pub fn schema_build(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let url = opt_str(&args, "url").filter(|u| !u.is_empty());
    let html = opt_str(&args, "html").filter(|h| !h.is_empty());
    if url.is_none() && html.is_none_or(str::is_empty) {
        return Err(ToolError::Failed("url or html required".to_owned()));
    }
    let override_type = opt_str(&args, "override_type").filter(|o| !o.is_empty());
    // Supplied HTML is analyzed offline; the fetcher is built only when it can
    // be consulted, so an offline call needs no network stack at all.
    let fetcher = if html.is_none() {
        Some(HttpFetcher::new().map_err(ToolError::Failed)?)
    } else {
        None
    };
    let result = spiderhead_checks::schema_build::build_schema(
        fetcher.as_ref().map(|f| f as &dyn HtmlFetcher),
        url,
        html,
        25.0,
        override_type,
    );
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

/// `semantic_inputs`: the reproducible normalized-input manifest for semantic
/// analysis — hashes, content-area strategy and language evidence per document,
/// never the normalized text itself.
pub fn semantic_inputs(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let items = match opt(&args, "items") {
        None => None,
        Some(value) => match value.as_array() {
            Some(list) => Some(list),
            None => {
                return Err(ToolError::Failed(
                    "items[] required (list of {url, html})".to_owned(),
                ))
            }
        },
    };
    let scan = opt_str(&args, "scan").filter(|scan| !scan.is_empty());
    let content_area = content_area(&args)?;
    match (items, scan) {
        (Some(_), Some(_)) => Err(ToolError::Failed(
            "items[] and scan are mutually exclusive".to_owned(),
        )),
        (_, Some(scan)) => {
            if content_area.is_some() {
                return Err(ToolError::Failed(
                    "scan input uses the crawl's recorded content_area config".to_owned(),
                ));
            }
            let corpus =
                spiderhead_store::corpus_inputs::scan_corpus(scan, "semantic").map_err(failed)?;
            let empty: Vec<Value> = Vec::new();
            let corpus_items = corpus
                .get("items")
                .and_then(Value::as_array)
                .unwrap_or(&empty);
            let coverage_state = corpus
                .get("coverage")
                .and_then(|coverage| coverage.get("state"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if coverage_state == "unavailable" {
                let mut document = Map::new();
                document.insert("ok".into(), json!(false));
                for (key, value) in spiderhead_store::corpus_inputs::corpus_public(&corpus, 0, "") {
                    document.insert(key, value);
                }
                return Ok(Value::Object(document));
            }
            let documents: Vec<Value> = corpus_items
                .iter()
                .map(|item| {
                    let record = item.as_object().cloned().unwrap_or_default();
                    spiderhead_parse::text_normalize::manifest_entry(&record)
                })
                .collect();
            let mut out = Map::new();
            out.insert("ok".into(), json!(true));
            out.insert("count".into(), json!(documents.len()));
            out.insert("documents".into(), Value::Array(documents.clone()));
            for (key, value) in
                spiderhead_store::corpus_inputs::corpus_public(&corpus, documents.len() as i64, "")
            {
                out.insert(key, value);
            }
            Ok(Value::Object(out))
        }
        (Some(list), None) => {
            if list.is_empty() {
                return Err(ToolError::Failed(
                    "items[] required (list of {url, html})".to_owned(),
                ));
            }
            let prepared =
                spiderhead_parse::text_normalize::prepare_items(list, content_area.as_ref())
                    .map_err(|error| ToolError::Failed(error.to_string()))?;
            let documents: Vec<Value> = prepared
                .iter()
                .map(|item| {
                    let record = item.as_object().cloned().unwrap_or_default();
                    spiderhead_parse::text_normalize::manifest_entry(&record)
                })
                .collect();
            let unavailable = documents
                .iter()
                .filter(|document| document["state"] == json!("unavailable"))
                .count();
            let empty = documents
                .iter()
                .filter(|document| document["state"] == json!("empty"))
                .count();
            Ok(json!({
                "ok": true,
                "count": documents.len(),
                "documents": documents,
                "normalization": spiderhead_parse::text_normalize::normalization_policy(
                    content_area.as_ref(),
                ),
                "coverage": {
                    "state": if unavailable > 0 { "partial" } else { "complete" },
                    "reason": if unavailable > 0 {
                        "items without a usable html body are unavailable"
                    } else {
                        ""
                    },
                    "eligible_documents": list.len(),
                    "prepared_documents": documents.len() - unavailable,
                    "analyzed_documents": documents.len() - unavailable,
                    "measured_empty_documents": empty,
                    "omitted_documents": unavailable,
                    "omission_reasons": if unavailable > 0 {
                        json!({"item supplies no html body": unavailable})
                    } else {
                        json!({})
                    },
                },
            }))
        }
        (None, None) => Err(ToolError::Failed(
            "items[] required (list of {url, html})".to_owned(),
        )),
    }
}

/// `ai_column`: plan a per-URL AI column over retained page evidence, then
/// validate caller-supplied values; this handler calls no model and transfers
/// nothing — the consent note in every plan names the caller as the sender.
pub fn ai_column(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let items = opt(&args, "items");
    let scan = opt(&args, "scan");
    if items.is_some() == scan.is_some() {
        return Err(failed("provide exactly one of items[] or scan"));
    }
    let prompt = match opt(&args, "prompt") {
        None => "",
        Some(Value::String(text)) => text.as_str(),
        Some(_) => return Err(failed("prompt is required")),
    };
    let urls: Option<&[Value]> = match opt(&args, "urls") {
        None => None,
        Some(Value::Array(list)) => Some(list),
        Some(_) => return Err(failed("urls must be a list of URLs")),
    };
    let column = opt_str(&args, "column").unwrap_or("ai_column");
    let max_pages = opt_int(&args, "max_pages").unwrap_or(100);
    let mut out = Map::new();
    let source_items: Vec<Value> = match (items, scan) {
        (_, Some(scan)) => {
            let corpus = spiderhead_store::corpus_inputs::scan_corpus(
                scan.as_str().unwrap_or_default(),
                "semantic",
            )
            .map_err(failed)?;
            let coverage_state = corpus
                .get("coverage")
                .and_then(|coverage| coverage.get("state"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if coverage_state == "unavailable" {
                out.insert("ok".into(), json!(false));
                for (key, value) in spiderhead_store::corpus_inputs::corpus_public(&corpus, 0, "") {
                    out.insert(key, value);
                }
                return Ok(Value::Object(out));
            }
            let items = corpus
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for (key, value) in spiderhead_store::corpus_inputs::corpus_public(&corpus, 0, "") {
                out.insert(key, value);
            }
            items
        }
        (Some(list), None) => match list.as_array() {
            Some(list) => list.clone(),
            None => return Err(failed("items must be a list of objects")),
        },
        (None, None) => unreachable!("the one-source gate rejects this pair"),
    };
    let plan = spiderhead_checks::ai_column::prepare_ai_column_plan(
        &source_items,
        prompt,
        urls,
        column,
        max_pages,
    )
    .map_err(failed)?;
    let Some(rows) = opt(&args, "rows") else {
        out.insert("ok".into(), json!(true));
        out.insert("plan".into(), plan);
        return Ok(Value::Object(out));
    };
    let Some(rows) = rows.as_array() else {
        return Err(failed("rows must be a list of objects"));
    };
    let result =
        spiderhead_checks::ai_column::apply_ai_column_results(&plan, rows).map_err(failed)?;
    if let Some(csv_path) = opt_str(&args, "csv_path").filter(|path| !path.is_empty()) {
        spiderhead_checks::ai_column::export_ai_column_csv(&result, csv_path).map_err(failed)?;
    }
    out.insert("ok".into(), json!(true));
    out.insert("plan_coverage".into(), plan["coverage"].clone());
    out.insert("result".into(), result);
    Ok(Value::Object(out))
}

/// `meta_description_drafts`: prepare a bounded draft plan or validate
/// caller-supplied drafts against it; no model call happens here — the
/// executor boundary stays with the caller.
pub fn meta_description_drafts(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let items = opt(&args, "items");
    let scan = opt(&args, "scan");
    if items.is_some() == scan.is_some() {
        return Err(failed("provide exactly one of items[] or scan"));
    }
    let context = opt(&args, "context");
    let batch_size = opt_int(&args, "batch_size").unwrap_or(20);
    let mut out = Map::new();
    let plan = match (items, scan) {
        (_, Some(scan)) => {
            let corpus = spiderhead_store::corpus_inputs::scan_corpus(
                scan.as_str().unwrap_or_default(),
                "semantic",
            )
            .map_err(failed)?;
            let coverage_state = corpus
                .get("coverage")
                .and_then(|coverage| coverage.get("state"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if coverage_state == "unavailable" {
                out.insert("ok".into(), json!(false));
                for (key, value) in spiderhead_store::corpus_inputs::corpus_public(&corpus, 0, "") {
                    out.insert(key, value);
                }
                return Ok(Value::Object(out));
            }
            let source_items = corpus
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let plan =
                spiderhead_checks::meta_description_drafts::prepare_draft_plan_from_normalized(
                    &source_items,
                    context,
                    batch_size,
                )
                .map_err(failed)?;
            for (key, value) in spiderhead_store::corpus_inputs::corpus_public(&corpus, 0, "") {
                out.insert(key, value);
            }
            plan
        }
        (Some(list), None) => {
            let source_items = match list.as_array() {
                Some(list) => list.clone(),
                None => return Err(failed("items must be a list of objects")),
            };
            spiderhead_checks::meta_description_drafts::prepare_draft_plan(
                &source_items,
                context,
                batch_size,
            )
            .map_err(failed)?
        }
        (None, None) => unreachable!("the one-source gate rejects this pair"),
    };
    let Some(drafts) = opt(&args, "drafts") else {
        out.insert("ok".into(), json!(true));
        out.insert("plan".into(), plan);
        return Ok(Value::Object(out));
    };
    let executor = opt(&args, "executor");
    let checkpoint_path = opt_str(&args, "checkpoint_path").unwrap_or_default();
    if !executor.map(Value::is_object).unwrap_or(false) || checkpoint_path.is_empty() {
        return Err(failed(
            "draft execution requires executor declaration and checkpoint_path",
        ));
    }
    let json_path = opt(&args, "json_path");
    let csv_path = opt(&args, "csv_path");
    if json_path.is_some() != csv_path.is_some() {
        return Err(failed("json_path and csv_path must be supplied together"));
    }
    let result = spiderhead_checks::meta_description_drafts::run_draft_plan(
        &plan,
        &spiderhead_checks::meta_description_drafts::DeclaredDraftExecutor::new(
            executor.unwrap_or(&Value::Null),
            drafts,
        ),
        &spiderhead_checks::meta_description_drafts::DraftCheckpoint::open(checkpoint_path)
            .map_err(|error| failed(error.to_string()))?,
    )
    .map_err(failed)?;
    if let (Some(json_path), Some(csv_path)) =
        (opt_str(&args, "json_path"), opt_str(&args, "csv_path"))
    {
        if !json_path.is_empty() && !csv_path.is_empty() {
            spiderhead_checks::meta_description_drafts::export_draft_review(
                &result, json_path, csv_path,
            )
            .map_err(failed)?;
        }
    }
    out.insert("ok".into(), json!(true));
    out.insert("plan_coverage".into(), plan["coverage"].clone());
    out.insert("result".into(), result);
    Ok(Value::Object(out))
}

/// `semantic_similarity`: group supplied embedding evidence; this handler
/// never loads or calls a model. Documents come from inline items or a
/// retained scan; vectors ride in `embeddings[]` and are cached locally.
pub fn semantic_similarity(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let items = opt(&args, "items");
    let scan = opt(&args, "scan");
    if items.is_some() == scan.is_some() {
        return Err(failed("provide exactly one of items[] or scan"));
    }
    let adapter = match opt(&args, "adapter") {
        Some(adapter) if adapter.is_object() => adapter,
        _ => return Err(failed("adapter declaration is required")),
    };
    let cache_path = opt_str(&args, "cache_path").unwrap_or_default();
    if cache_path.is_empty() {
        return Err(failed(
            "cache_path is required for local semantic embedding reuse",
        ));
    }
    let embeddings = match opt(&args, "embeddings") {
        Some(embeddings) if embeddings.is_array() => embeddings,
        _ => {
            return Err(failed(
                "embeddings[] is required; SEOHEAD does not call a model by default",
            ))
        }
    };
    let threshold = opt_float(&args, "threshold").unwrap_or(0.82);
    let max_candidate_comparisons = opt_int(&args, "max_candidate_comparisons").unwrap_or(250_000);

    let (mut documents, public): (Vec<Value>, Map<String, Value>) = match scan {
        Some(scan) => {
            let corpus = spiderhead_store::corpus_inputs::scan_corpus(
                scan.as_str().unwrap_or_default(),
                "semantic",
            )
            .map_err(failed)?;
            let coverage_state = corpus
                .get("coverage")
                .and_then(|coverage| coverage.get("state"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if coverage_state == "unavailable" {
                let mut out = Map::new();
                out.insert("ok".into(), json!(false));
                out.insert("groups".into(), json!([]));
                for (key, value) in spiderhead_store::corpus_inputs::corpus_public(&corpus, 0, "") {
                    out.insert(key, value);
                }
                return Ok(Value::Object(out));
            }
            let empty: Vec<Value> = Vec::new();
            let documents = corpus
                .get("items")
                .and_then(Value::as_array)
                .unwrap_or(&empty)
                .clone();
            let public = spiderhead_store::corpus_inputs::corpus_public(&corpus, 0, "");
            (documents, public)
        }
        None => {
            let Some(list) = items.and_then(Value::as_array) else {
                return Err(failed("items must be a list of objects"));
            };
            let prepared = spiderhead_parse::text_normalize::prepare_items(list, None)
                .map_err(|error| ToolError::Failed(error.to_string()))?;
            let public: Map<String, Value> = [
                (
                    "normalization",
                    spiderhead_parse::text_normalize::normalization_policy(None),
                ),
                (
                    "coverage",
                    json!({
                        "state": "complete",
                        "eligible_documents": list.len() as i64,
                        "prepared_documents": prepared.len() as i64,
                        "analyzed_documents": 0,
                        "omitted_documents": 0,
                        "omission_reasons": {},
                    }),
                ),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
            (prepared, public)
        }
    };

    // Caller-supplied vectors ride on the documents; a URL without one stays
    // pending for the declared adapter, which refuses with the contract error.
    let mut vectors: Map<String, Value> = Map::new();
    for entry in embeddings.as_array().expect("checked array") {
        let Some(object) = entry.as_object() else {
            continue;
        };
        let Some(url) = object.get("url").and_then(Value::as_str) else {
            continue;
        };
        vectors.insert(
            url.to_owned(),
            object.get("vector").cloned().unwrap_or(Value::Null),
        );
    }
    for document in documents.iter_mut() {
        if let Some(url) = document.get("url").and_then(Value::as_str) {
            if let Some(vector) = vectors.get(url) {
                document["embedding"] = vector.clone();
            }
        }
    }

    let cache = spiderhead_checks::semantic_similarity::EmbeddingCache::new(cache_path)
        .map_err(|error| failed(error.to_string()))?;
    let declared = spiderhead_checks::semantic_similarity::DeclaredEmbeddingAdapter::new(adapter);
    let mut result = spiderhead_checks::semantic_similarity::analyze_semantic_documents(
        &documents,
        Some(&declared),
        &cache,
        threshold,
        max_candidate_comparisons,
    )
    .map_err(failed)?;
    let object = result.as_object_mut().expect("result object");
    if let Some(coverage) = public.get("coverage") {
        object.insert("source_coverage".into(), coverage.clone());
    }
    if let Some(source) = public.get("source") {
        object.insert("source".into(), source.clone());
    }
    if let Some(normalization) = public.get("normalization") {
        object.insert("normalization".into(), normalization.clone());
    }
    Ok(result)
}

/// `keywords_cluster`: in-process TF-IDF clustering (K-Means / DBSCAN /
/// agglomerative). The clusterer's contract is never raising: problems land in
/// the document as `ok: false`, so this handler is always an `Ok`.
pub fn keywords_cluster(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    Ok(spiderhead_checks::clusterer::run(&Value::Object(args)))
}

/// The sitemap fetch identity: plain httpx, not the crawler's user agent.
const MARKDOWN_EXPORT_SITEMAP_USER_AGENT: &str = "python-httpx/0.28.1";

/// `markdown_export`: pages of one source (url / urls_file / sitemap) written
/// as frontmatter'd Markdown files (`mcp/handlers/checks.py::markdown_export`).
pub fn markdown_export(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let url = opt_str(&args, "url");
    let urls_file = opt_str(&args, "urls_file");
    let sitemap = opt_str(&args, "sitemap");
    spiderhead_checks::markdown_export::validate_sources(url, urls_file, sitemap)
        .map_err(failed)?;
    let out_dir = opt_str(&args, "out_dir").unwrap_or("markdown-export");
    let max_urls = opt_int(&args, "max_urls").unwrap_or(200) as usize;
    let delay = opt_float(&args, "delay").unwrap_or(0.5);

    let (targets, sitemap_errors) = if let Some(sitemap) = sitemap {
        let client =
            spiderhead_fetch::client::build_client(spiderhead_crawl::sitemap::TIMEOUT_SECONDS)
                .map_err(failed)?;
        let expanded = tokio::runtime::Runtime::new().map_err(failed)?.block_on(
            spiderhead_crawl::sitemap::crawl(
                sitemap,
                3,
                &client,
                MARKDOWN_EXPORT_SITEMAP_USER_AGENT,
                None,
            ),
        );
        (
            spiderhead_checks::markdown_export::sitemap_targets(&expanded),
            spiderhead_checks::markdown_export::sitemap_errors(&expanded),
        )
    } else if let Some(urls_file) = urls_file {
        (
            spiderhead_checks::markdown_export::read_urls_file(urls_file).map_err(failed)?,
            Vec::new(),
        )
    } else {
        (
            url.map(|url| vec![url.to_owned()]).unwrap_or_default(),
            Vec::new(),
        )
    };

    let fetcher = HttpFetcher::new().map_err(failed)?;
    let exported = jiff::Zoned::now()
        .with_time_zone(jiff::tz::TimeZone::UTC)
        .date()
        .to_string();
    spiderhead_checks::markdown_export::run(
        &targets,
        &fetcher,
        &spiderhead_checks::markdown_export::expanduser(out_dir),
        max_urls,
        delay,
        &exported,
        sitemap_errors,
    )
    .map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // from tests/test_markdown_export_handler.py::test_exactly_one_input_source_is_required
    #[test]
    fn markdown_export_requires_exactly_one_source() {
        let error = markdown_export(&json!({})).unwrap_err().to_string();
        assert!(error.contains("exactly one"), "{error}");
        let error = markdown_export(&json!({
            "url": "https://x.test/",
            "sitemap": "https://x.test/sitemap.xml",
        }))
        .unwrap_err()
        .to_string();
        assert!(error.contains("exactly one"), "{error}");
    }

    // from tests/test_clusterer_k.py::test_single_keyword_auto_k_returns_one_cluster
    // (handler boundary): a document with `ok: false` is an Ok result, not an Err.
    #[test]
    fn keywords_cluster_missing_keywords_is_a_false_document() {
        let out = keywords_cluster(&json!({"keywords": []})).unwrap();
        assert_eq!(out["ok"], json!(false));
        assert_eq!(out["error"], json!("The keyword list is missing or empty"));
        let out = keywords_cluster(&json!({"algorithm": "kmeans"})).unwrap();
        assert_eq!(out["ok"], json!(false));
    }

    // from tests/test_clusterer_k.py::test_single_keyword_auto_k_returns_one_cluster
    #[test]
    fn keywords_cluster_auto_k_single_keyword() {
        let out = keywords_cluster(&json!({
            "keywords": ["technical seo audit"],
            "algorithm": "kmeans",
            "auto_k": true,
            "language": "english",
        }))
        .unwrap();
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["count"], json!(1));
        assert_eq!(
            out["clusters"][0]["keywords"],
            json!(["technical seo audit"])
        );
    }

    fn raw(site: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../oracle/raw-scans")
            .join(site)
            .join("scan.sqlite")
    }

    // from tests/test_semantic_inputs.py::_LATIN_HTML
    fn latin_html() -> String {
        "<html lang=\"en\"><body><nav>shared navigation words</nav>\
         <main><h1>Catalog page</h1><p>The pump station catalogue page \
         describes delivery terms and stock levels in plain English.</p></main>\
         <footer>shared footer words</footer></body></html>"
            .to_owned()
    }

    // from tests/test_semantic_inputs.py::test_semantic_inputs_inline_items_and_unavailable_entries
    #[test]
    fn inline_items_and_unavailable_entries() {
        let result = semantic_inputs(&json!({
            "items": [
                {"url": "https://example.test/a", "html": latin_html()},
                {"url": "https://example.test/missing"},
                "not-an-object",
            ],
        }))
        .expect("ok");
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["count"], json!(3));
        let states: Vec<(&str, &str)> = result["documents"]
            .as_array()
            .expect("documents")
            .iter()
            .map(|document| {
                (
                    document["url"].as_str().expect("url"),
                    document["state"].as_str().expect("state"),
                )
            })
            .collect();
        assert!(states.contains(&("https://example.test/a", "normalized")));
        assert!(states.contains(&("https://example.test/missing", "unavailable")));
        let coverage = &result["coverage"];
        assert_eq!(coverage["state"], json!("partial"));
        assert_eq!(coverage["eligible_documents"], json!(3));
        assert_eq!(coverage["prepared_documents"], json!(1));
        assert_eq!(coverage["omitted_documents"], json!(2));
        assert!(!result
            .to_string()
            .contains("the pump station catalogue page"));
    }

    // from tests/test_semantic_inputs.py::test_semantic_inputs_rejects_ambiguous_arguments
    #[test]
    fn rejects_ambiguous_arguments() {
        let scan = raw("basic").to_string_lossy().into_owned();
        let error =
            semantic_inputs(&json!({"items": [{"url": "x", "html": "<p>t</p>"}], "scan": scan}))
                .unwrap_err();
        assert!(error.to_string().contains("mutually exclusive"));
        let error = semantic_inputs(&json!({
            "scan": scan,
            "content_area": {"root_selector": "main"},
        }))
        .unwrap_err();
        assert!(error.to_string().contains("content_area"));
        let error = semantic_inputs(&json!({})).unwrap_err();
        assert!(error.to_string().contains("items[]"));
    }

    // from tests/test_semantic_inputs.py::test_scan_semantic_inputs_stream_retained_bodies_without_refetch
    // (fixture shape: the retained raw scan feeds the same scan_corpus path)
    #[test]
    fn scan_inputs_manifest_never_carries_text() {
        let result = semantic_inputs(&json!({"scan": raw("basic").to_string_lossy()})).expect("ok");
        assert_eq!(result["ok"], json!(true));
        let documents = result["documents"].as_array().expect("documents");
        assert_eq!(result["count"], json!(documents.len()));
        assert!(documents
            .iter()
            .all(|document| document.get("normalized_sha256").is_some()));
        let coverage = &result["coverage"];
        assert_eq!(
            coverage["analyzed_documents"],
            json!(documents.len() as i64)
        );
        assert!(result.get("normalization").is_some());
        assert!(!result.to_string().contains("normalized_text"));
    }

    // from tests/test_ai_column.py::test_handler_requires_exactly_one_source
    #[test]
    fn ai_column_requires_exactly_one_source() {
        let error = ai_column(&json!({"prompt": "Classify"})).unwrap_err();
        assert_eq!(error.to_string(), "provide exactly one of items[] or scan");
        let error = ai_column(&json!({
            "items": [],
            "scan": "x.sqlite",
            "prompt": "Classify",
        }))
        .unwrap_err();
        assert_eq!(error.to_string(), "provide exactly one of items[] or scan");
    }

    // from tests/test_ai_column.py::test_plan_selects_urls_estimates_size_and_declares_consent_scope
    // (items branch: the caller supplies page evidence directly)
    #[test]
    fn ai_column_items_branch_plans_without_a_scan() {
        let out = ai_column(&json!({
            "items": [{
                "url": "https://example.test/a",
                "text": "Water pump equipment.",
                "title": "Industrial pumps",
                "headings": ["Industrial pumps"],
            }],
            "prompt": "Classify search intent",
            "urls": ["https://example.test/a"],
        }))
        .unwrap();
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["plan"]["estimate"]["eligible"], json!(1));
        assert_eq!(out["plan"]["consent"]["data_sent_by_seohead"], json!(false));
        assert_eq!(out["plan"]["coverage"]["state"], json!("complete"));
    }

    // from tests/test_ai_column.py::test_handler_plans_from_retained_scan_and_applies_supplied_values
    // (fixture shape: the retained raw scan feeds the same scan_corpus path)
    #[test]
    fn ai_column_plans_from_retained_scan_and_applies_supplied_values() {
        let dir = tempfile::tempdir().unwrap();
        let scan = raw("basic").to_string_lossy().into_owned();
        let plan =
            ai_column(&json!({"scan": scan, "prompt": "Classify search intent"})).expect("plan ok");
        assert_eq!(plan["ok"], json!(true));
        let page = plan["plan"]["pages"][0].clone();
        assert_eq!(page["state"], json!("eligible"));
        let csv = dir.path().join("column.csv");
        let result = ai_column(&json!({
            "scan": scan,
            "prompt": "Classify search intent",
            "rows": [{
                "url": page["url"],
                "source_sha256": page["source_reference"]["normalized_sha256"],
                "value": "informational",
            }],
            "csv_path": csv.to_string_lossy(),
        }))
        .expect("apply ok");

        assert_eq!(
            result["result"]["rows"][0]["column_value"],
            json!("informational")
        );
        // Every discovered page is planned; one supplied row leaves the rest missing.
        assert_eq!(
            result["plan_coverage"]["eligible"],
            plan["plan"]["coverage"]["eligible"]
        );
        assert_eq!(result["result"]["coverage"]["completed"], json!(1));
        assert_eq!(result["result"]["coverage"]["state"], json!("partial"));
        assert!(csv.exists());
    }

    // from tests/test_ai_column.py (handler rows must be a list, like apply's gate)
    #[test]
    fn ai_column_rejects_non_list_rows() {
        let error = ai_column(&json!({
            "items": [{"url": "https://example.test/a", "text": "Water pump equipment."}],
            "prompt": "Classify",
            "rows": "not-a-list",
        }))
        .unwrap_err();
        assert_eq!(error.to_string(), "rows must be a list of objects");
    }

    // from tests/test_semantic_content_interfaces.py::test_meta_draft_plan_and_execute_reuse_retained_corpus_metadata
    // (the same exactly-one-source gate as ai_column)
    #[test]
    fn meta_drafts_requires_exactly_one_source() {
        let error = meta_description_drafts(&json!({})).unwrap_err();
        assert_eq!(error.to_string(), "provide exactly one of items[] or scan");
        let error = meta_description_drafts(&json!({
            "items": [{"url": "https://example.test/a", "html": "<p>t</p>"}],
            "scan": "x.sqlite",
        }))
        .unwrap_err();
        assert_eq!(error.to_string(), "provide exactly one of items[] or scan");
    }

    // from tests/test_semantic_content_interfaces.py::test_meta_draft_plan_and_execute_reuse_retained_corpus_metadata
    // (fixture shape: the retained raw scan feeds the same scan_corpus path)
    #[test]
    fn meta_draft_plan_and_execute_reuse_retained_corpus_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let scan = raw("basic").to_string_lossy().into_owned();
        let plan = meta_description_drafts(&json!({
            "scan": scan,
            "context": {"min_chars": 1},
        }))
        .expect("plan ok");
        assert_eq!(plan["source"]["kind"], json!("scan.v1"));
        let page = plan["plan"]["batches"][0][0].clone();
        let result = meta_description_drafts(&json!({
            "scan": scan,
            "context": {"min_chars": 1},
            "drafts": [{
                "url": page["url"],
                "source_sha256": page["source_reference"]["normalized_sha256"],
                "proposed_description":
                    "Industrial water pumps for factory maintenance and delivery planning.",
            }],
            "executor": {
                "kind": "calling_agent",
                "contract_version": "meta_description_drafts.v1",
                "model_identity": null,
                "data_transfer": "caller_runtime",
            },
            "checkpoint_path": dir.path().join("drafts.sqlite").to_string_lossy(),
            "json_path": dir.path().join("review.json").to_string_lossy(),
            "csv_path": dir.path().join("review.csv").to_string_lossy(),
        }))
        .expect("execute ok");
        assert_eq!(
            result["result"]["rows"][0]["generation_state"],
            json!("completed")
        );
        assert!(dir.path().join("review.csv").exists());
    }

    // from tests/test_semantic_content_interfaces.py::test_semantic_similarity_consumes_supplied_vectors_without_model_call_and_reuses_scan
    // (gate order: source, adapter, cache_path, embeddings)
    #[test]
    fn semantic_similarity_gates() {
        let error = semantic_similarity(&json!({})).unwrap_err();
        assert_eq!(error.to_string(), "provide exactly one of items[] or scan");
        let error = semantic_similarity(&json!({"items": []})).unwrap_err();
        assert_eq!(error.to_string(), "adapter declaration is required");
        let error = semantic_similarity(&json!({
            "items": [],
            "adapter": {"kind": "local"},
        }))
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "cache_path is required for local semantic embedding reuse"
        );
        let error = semantic_similarity(&json!({
            "items": [],
            "adapter": {"kind": "local"},
            "cache_path": "/tmp/semantic.sqlite",
        }))
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "embeddings[] is required; SEOHEAD does not call a model by default"
        );
    }

    // from tests/test_semantic_content_interfaces.py::test_semantic_similarity_consumes_supplied_vectors_without_model_call_and_reuses_scan
    // (fixture shape: the retained raw scan plays the oracle's built scan; every
    // fixture URL carries a supplied vector, so the declared adapter is never asked)
    #[test]
    fn semantic_similarity_consumes_supplied_vectors_and_reuses_scan() {
        let dir = tempfile::tempdir().unwrap();
        let scan = raw("basic").to_string_lossy().into_owned();
        let corpus = spiderhead_store::corpus_inputs::scan_corpus(&scan, "semantic")
            .expect("fixture corpus");
        let urls: Vec<String> = corpus["items"]
            .as_array()
            .expect("items")
            .iter()
            .filter_map(|item| item["url"].as_str().map(str::to_owned))
            .collect();
        assert!(urls.len() >= 2);
        let embeddings: Vec<Value> = urls
            .iter()
            .map(|url| json!({"url": url, "vector": [1, 0]}))
            .collect();
        let result = semantic_similarity(&json!({
            "scan": scan,
            "adapter": {
                "kind": "local",
                "model_id": "fixture",
                "model_version": "1",
                "settings": {"dims": 2},
                "data_transfer": "none",
            },
            "embeddings": embeddings,
            "cache_path": dir.path().join("semantic.sqlite").to_string_lossy(),
            "threshold": 0.9,
        }))
        .expect("ok");
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["source"]["kind"], json!("scan.v1"));
        assert_eq!(
            result["normalization"]["normalizer_version"],
            json!("content_normalizer.v1")
        );
        assert_eq!(
            result["groups"][0]["kind"],
            json!("semantic_similarity_candidate")
        );
        assert!(dir.path().join("semantic.sqlite").exists());
    }

    // from tests/test_semantic_content_interfaces.py::test_semantic_similarity_consumes_supplied_vectors_without_model_call_and_reuses_scan
    // (inline-items branch: normalization policy rides on the result, no model call)
    #[test]
    fn semantic_similarity_inline_items_with_declared_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let result = semantic_similarity(&json!({
            "items": [
                {"url": "https://example.test/a", "html": "<p>pump stations for industrial water supply</p>"},
                {"url": "https://example.test/b", "html": "<p>industrial water supply pump equipment</p>"},
            ],
            "adapter": {
                "kind": "local",
                "model_id": "fixture",
                "model_version": "1",
                "settings": {"dims": 2},
                "data_transfer": "none",
            },
            "embeddings": [
                {"url": "https://example.test/a", "vector": [1, 0]},
                {"url": "https://example.test/b", "vector": [1, 0]},
            ],
            "cache_path": dir.path().join("semantic.sqlite").to_string_lossy(),
            "threshold": 0.9,
        }))
        .expect("ok");
        assert_eq!(result["ok"], json!(true));
        assert_eq!(
            result["normalization"]["normalizer_version"],
            json!("content_normalizer.v1")
        );
        assert_eq!(result["source_coverage"]["state"], json!("complete"));
        assert_eq!(result["groups"].as_array().map(Vec::len), Some(1));
        assert!(dir.path().join("semantic.sqlite").exists());
    }
}
