//! Fallback for HTML above the schema validation size limit: fills an empty result without parsing.

use super::*;

pub(super) fn oversize(result: &mut SchemaCheck) {
    let reason = format!("HTML exceeds the {MAX_HTML_BYTES}-byte schema validation limit");
    result.vocabulary = Some(VocabularySummary {
        fetched: Json::Bool(false),
        types: 0,
        properties: 0,
    });
    result.blocks = Some(0);
    result.blocks_found = Some(0);
    result.blocks_invalid = Some(0);
    result.vocabularies = Some(Vec::new());
    result.parse_errors = Some(Vec::new());
    result.graph = Some(GraphShape {
        nodes: 0,
        with_id: 0,
        linked_by_id: 0,
        top_level: 0,
        islands: Vec::new(),
        is_graph: false,
    });
    result.entities = Some(Vec::new());
    result.rich_results = Some(Vec::new());
    result.rules_version = Some(nodes::rules_version());
    result.other_markup = Some(OtherMarkup {
        microdata: false,
        rdfa: false,
    });
    result.markup_counts = Some(MarkupCounts {
        microdata: 0,
        rdfa: 0,
    });
    result.markup_coverage = Some(
        ["microdata", "rdfa"]
            .iter()
            .map(|f| MarkupCoverage {
                format: (*f).to_owned(),
                status: "partial".to_owned(),
                reasons: Some(vec![reason.clone()]),
            })
            .collect(),
    );
    result.findings = Some(vec![format!(
        "Structured-data validation coverage is partial: {reason}"
    )]);
}

pub(super) fn empty_result() -> SchemaCheck {
    SchemaCheck {
        ok: true,
        error: None,
        url: None,
        final_url: None,
        status_code: None,
        vocabulary: None,
        blocks: None,
        blocks_found: None,
        blocks_invalid: None,
        vocabularies: None,
        parse_errors: None,
        graph: None,
        entities: None,
        rich_results: None,
        rules_version: None,
        other_markup: None,
        markup_counts: None,
        markup_coverage: None,
        findings: None,
    }
}

/// Validate one page's vocabulary, entity graph and rich-result readiness. `document_base_url`
/// overrides the base used to resolve ids. The fetch path uses `fetcher`
/// uses `fetcher` (`None` is allowed only for [`SchemaSource::Html`]).
pub fn check_schema(
    source: SchemaSource<'_>,
    fetcher: Option<&dyn HtmlFetcher>,
    timeout: f64,
    document_base_url: Option<&str>,
) -> Result<SchemaCheck> {
    let mut result = empty_result();
    let mut requested_url = String::new();
    let html: String = match source {
        SchemaSource::Html(html) => html.to_owned(),
        SchemaSource::Url(url) => {
            requested_url = url.to_owned();
            let target = normalize_url(url);
            if target.is_empty() {
                return Ok(SchemaCheck {
                    ok: false,
                    error: Some(format!("Not a valid HTTP(S) URL: {}", py_repr(url))),
                    ..empty_result()
                });
            }
            let Some(fetcher) = fetcher else {
                return Ok(SchemaCheck {
                    ok: false,
                    error: Some("httpx is required".to_owned()),
                    ..empty_result()
                });
            };
            match fetcher.get(
                &target,
                &crate::parser::shared::default_headers(),
                timeout,
                MAX_REDIRECTS,
            ) {
                Ok(response) => {
                    result.url = Some(target);
                    result.final_url = Some(response.final_url);
                    result.status_code = Some(response.status_code);
                    response.text.chars().take(MAX_HTML_BYTES).collect()
                }
                Err(error) => {
                    return Ok(SchemaCheck {
                        ok: false,
                        url: Some(target),
                        error: Some(error),
                        ..empty_result()
                    })
                }
            }
        }
    };

    if html.len() > MAX_HTML_BYTES {
        oversize(&mut result);
        return Ok(result);
    }

    let vocab = load_vocab();
    let soup = parse_document(&html);
    let (blocks, parse_errors, blocks_found) = extract_blocks(&soup);
    let fetched_url = result
        .final_url
        .clone()
        .filter(|u| !u.is_empty())
        .unwrap_or(requested_url);
    let base = match document_base_url {
        Some(base) => base.to_owned(),
        None => html_document_base_url(DocumentSource::Tree(&soup), &fetched_url),
    };

    let mut flat: Vec<nodes::Node> = Vec::new();
    let mut vocabularies: Vec<VocabularyUse> = Vec::new();
    for (i, block) in blocks.iter().enumerate() {
        let (context, supported) = nodes::block_vocab(block);
        let before = flat.len();
        let normalized = normalize_jsonld_ids(block, &base)?;
        nodes::flatten(
            &normalized,
            &mut flat,
            &format!("jsonld[{}]", i + 1),
            false,
            None,
            0,
        );
        for n in &mut flat[before..] {
            n.insert("_vocab_context".into(), json!(context));
            n.insert("_vocab_supported".into(), json!(supported));
            n.insert("_format".into(), json!("jsonld"));
        }
        match vocabularies
            .iter_mut()
            .find(|v| v.context == context && v.supported == supported)
        {
            Some(existing) => existing.blocks += 1,
            None => vocabularies.push(VocabularyUse {
                context,
                supported,
                blocks: 1,
            }),
        }
    }
    let (microdata, microdata_count) = microdata::extract_microdata(&soup, &base)?;
    let (rdfa_entities, rdfa_count, rdfa_reasons) = rdfa::extract_rdfa(&soup, &base)?;
    let rdfa_any = !rdfa_entities.is_empty();
    for entity in microdata.iter().chain(rdfa_entities.iter()) {
        let path = entity
            .get("_path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        nodes::flatten(
            &Value::Object(entity.clone()),
            &mut flat,
            &path,
            false,
            None,
            0,
        );
    }
    for node in &flat {
        if node.get("_format").and_then(Value::as_str) == Some("jsonld") {
            continue;
        }
        let context = node
            .get("_vocab_context")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let supported = node
            .get("_vocab_supported")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if !vocabularies
            .iter()
            .any(|v| v.context == context && v.supported == supported)
        {
            vocabularies.push(VocabularyUse {
                context,
                supported,
                blocks: 0,
            });
        }
    }
    let known_ids: HashSet<String> = flat
        .iter()
        .filter_map(|n| n.get("@id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let entities: Vec<Json> = flat
        .iter()
        .map(|n| check::check_node(n, vocab, &known_ids))
        .collect();

    result.vocabulary = Some(VocabularySummary {
        fetched: vocab["fetched"].clone(),
        types: vocab["types"].as_object().map_or(0, |t| t.len() as u64),
        properties: vocab["properties"]
            .as_object()
            .map_or(0, |t| t.len() as u64),
    });
    result.blocks = Some(blocks.len() as u64);
    result.blocks_found = Some(blocks_found as u64);
    result.blocks_invalid = Some((blocks_found - blocks.len()) as u64);
    result.vocabularies = Some(vocabularies);
    result.parse_errors = Some(parse_errors.iter().map(|e| json!(e)).collect());
    result.graph = Some(nodes::graph_shape(&flat));
    result.entities = Some(entities);
    result.rich_results = Some(nodes::rich_results(&flat));
    result.rules_version = Some(nodes::rules_version());
    result.other_markup = Some(OtherMarkup {
        microdata: microdata_count > 0,
        rdfa: rdfa_count > 0 || !rdfa_reasons.is_empty() || rdfa_any,
    });
    result.markup_counts = Some(MarkupCounts {
        microdata: microdata_count as u64,
        rdfa: rdfa_count as u64,
    });
    result.markup_coverage = Some(vec![
        MarkupCoverage {
            format: "microdata".to_owned(),
            status: if microdata_count > 0 {
                "checked"
            } else {
                "absent"
            }
            .to_owned(),
            reasons: None,
        },
        MarkupCoverage {
            format: "rdfa".to_owned(),
            status: if !rdfa_reasons.is_empty() {
                "partial"
            } else if rdfa_count > 0 || rdfa_any {
                "checked"
            } else {
                "absent"
            }
            .to_owned(),
            reasons: Some(rdfa_reasons),
        },
    ]);
    result.findings = Some(findings::findings(&result));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a scheme prefix that grows under lowering ("İ" -> "i̇", 3 bytes) used to
    /// be sliced by the lowered byte length and panic; slicing must use the character count.
    #[test]
    pub(super) fn normalize_url_unicode_prefixes_do_not_panic() {
        assert_eq!(normalize_url("\u{130}"), "https://\u{130}");
        assert_eq!(normalize_url("\u{212a}\u{212a}:x"), "");
        assert_eq!(normalize_url("тест:x"), "");
        assert_eq!(normalize_url("example.com/a"), "https://example.com/a");
    }
}
