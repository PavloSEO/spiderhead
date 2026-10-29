//! The `parse` handler: fetch one or more URLs and extract their SEO data
//! (the reference suite over `checks.parser.parse_url`).

use std::time::Duration;

use serde_json::{json, Map, Value};

use spiderhead_parse::parser::{parse_url as parse_url_core, FetchResponse, HtmlFetcher};
use spiderhead_parse::ParseOptions;

use crate::error::ToolError;

/// The live HTTP transport behind [`HtmlFetcher`]: reqwest with redirect
/// following, one shared runtime.
pub struct HttpFetcher {
    runtime: tokio::runtime::Runtime,
}

impl HttpFetcher {
    /// A blocking fetcher over its own single-thread runtime.
    pub fn new() -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("http runtime build failed: {e}"))?;
        Ok(Self { runtime })
    }

    async fn get_async(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        timeout: f64,
        max_redirects: u32,
    ) -> Result<FetchResponse, String> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(max_redirects as usize))
            .timeout(Duration::from_secs_f64(timeout))
            .connect_timeout(Duration::from_secs_f64(timeout))
            .build()
            .map_err(|e| format!("http client build failed: {e}"))?;
        let mut request = client.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().await.map_err(|e| e.to_string())?;
        let final_url = response.url().to_string();
        let status_code = response.status().as_u16();
        let text = response.text().await.map_err(|e| e.to_string())?;
        Ok(FetchResponse {
            final_url,
            status_code,
            text,
            headers: Vec::new(),
        })
    }
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self::new().expect("http runtime must build")
    }
}

impl HtmlFetcher for HttpFetcher {
    fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        timeout: f64,
        max_redirects: u32,
    ) -> Result<FetchResponse, String> {
        self.runtime
            .block_on(self.get_async(url, headers, timeout, max_redirects))
    }
}

/// the reference `parse_html` result key order (fetch fields first, then the page
/// literal; opt-in inventories append).
const RESULT_KEYS: [&str; 54] = [
    "url",
    "final_url",
    "status_code",
    "ok",
    "head_links",
    "document_base_url",
    "title",
    "meta_description",
    "meta_description_count",
    "robots",
    "robots_meta",
    "robots_meta_scoped",
    "charset",
    "doctype",
    "viewport",
    "meta_refresh",
    "html_amp",
    "render_blocking",
    "canonical",
    "og",
    "twitter",
    "headings",
    "h1_alt_only_text",
    "heading_outline",
    "jsonld",
    "jsonld_invalid",
    "trust_signals",
    "duplicate_ids",
    "markup",
    "links",
    "link_placement",
    "forms",
    "text",
    "content_text",
    "content_area_strategy",
    "word_count",
    "readability",
    "lorem_ipsum_count",
    "frames",
    "position",
    "hreflang",
    "mobile_alternates",
    "amphtml",
    "images",
    "html_lang",
    "plugin_elements_count",
    "mobile_alternate_broken",
    "meta_fragment",
    "link_observation",
    "forms_omitted",
    "url_sources",
    "resource_declarations",
    "resource_declarations_omitted",
    "invalid_link_targets",
];

/// The parse handler: `url` or `urls[]` in, one extracted page per target out.
pub fn parse(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let targets: Vec<String> = match args.get("urls") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => match super::opt_str(&args, "url") {
            Some(url) => vec![url.to_owned()],
            None => Vec::new(),
        },
    };
    if targets.is_empty() {
        return Err(ToolError::Failed("url or urls[] required".to_owned()));
    }
    let options = args.get("options").and_then(Value::as_object);
    let mut parse_options = ParseOptions::default();
    if let Some(options) = options {
        for (key, value) in options {
            let Some(flag) = value.as_bool() else {
                continue;
            };
            match key.as_str() {
                "meta" => parse_options.meta = flag,
                "canonical" => parse_options.canonical = flag,
                "og" => parse_options.og = flag,
                "headings" => parse_options.headings = flag,
                "jsonld" => parse_options.jsonld = flag,
                "links" => parse_options.links = flag,
                "forms" => parse_options.forms = flag,
                "text" => parse_options.text = flag,
                "url_sources" => parse_options.url_sources = flag,
                "classify_links" => parse_options.classify_links = flag,
                _ => {}
            }
        }
    }
    let timeout = options
        .and_then(|o| o.get("timeout"))
        .and_then(Value::as_f64);
    let fetcher = HttpFetcher::new().map_err(ToolError::Failed)?;
    let mut results: Vec<Value> = Vec::with_capacity(targets.len());
    for target in &targets {
        let outcome = parse_url_core(&fetcher, target, &parse_options, timeout);
        let mut value = serde_json::to_value(&outcome).map_err(|error| {
            ToolError::Failed(format!("parse result is not serializable: {error}"))
        })?;
        if let Some(page) = value.as_object_mut() {
            *page = python_key_order(page);
        }
        results.push(value);
    }
    Ok(json!({"count": results.len(), "results": results}))
}

/// Re-assemble one result object in the reference `parse_url` key order.
fn python_key_order(page: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for key in RESULT_KEYS {
        let Some(value) = page.get(key) else {
            continue;
        };
        // `head_links` entries are reference dicts (rel, href, target, then the
        // attribute loop); the parser stores them in a sorted map.
        let value = if key == "head_links" {
            reorder_head_links(value)
        } else {
            value.clone()
        };
        out.insert(key.to_owned(), value);
    }
    for (key, value) in page {
        if !out.contains_key(key) {
            out.insert(key.clone(), value.clone());
        }
    }
    out
}

/// the reference `_meta.extract_head_links` entry key order.
fn reorder_head_links(value: &Value) -> Value {
    const LINK_KEYS: [&str; 7] = ["rel", "href", "target", "hreflang", "lang", "media", "type"];
    Value::Array(
        value
            .as_array()
            .map(|links| {
                links
                    .iter()
                    .map(|link| {
                        let Some(entry) = link.as_object() else {
                            return link.clone();
                        };
                        let mut ordered = Map::new();
                        for key in LINK_KEYS {
                            if let Some(item) = entry.get(key) {
                                ordered.insert(key.to_owned(), item.clone());
                            }
                        }
                        for (key, item) in entry {
                            if !ordered.contains_key(key) {
                                ordered.insert(key.clone(), item.clone());
                            }
                        }
                        Value::Object(ordered)
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_targets_is_named() {
        let error = parse(&json!({})).unwrap_err();
        assert_eq!(error.to_string(), "url or urls[] required");
    }

    // the reference parse_url key order: fetch identity first, page literal after.
    #[test]
    fn result_keys_keep_python_order() {
        let page: Map<String, Value> = [
            ("word_count", json!(3)),
            ("title", json!("T")),
            ("url", json!("http://a/")),
            ("final_url", json!("http://a/")),
            ("status_code", json!(200)),
            ("ok", json!(true)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        let ordered = python_key_order(&page);
        let keys: Vec<&str> = ordered.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "url",
                "final_url",
                "status_code",
                "ok",
                "title",
                "word_count"
            ]
        );
    }
}
