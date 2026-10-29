//! `select`, `select_source`, and `compact_after_source`: bounded selection over
//! document and audit.v2 sources without materializing the corpus.

use super::*;
use spiderhead_formats::check_registry;

/// reference `_strings`: a list of nonempty strings, deduplicated preserving order.
fn strings(value: Option<&Value>, label: &str) -> Result<Vec<String>, VerifyError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Value::Array(items) = value else {
        return fail(format!("{label} must be a list of nonempty strings"));
    };
    if items
        .iter()
        .any(|i| !matches!(i, Value::String(s) if !s.is_empty()))
    {
        return fail(format!("{label} must be a list of nonempty strings"));
    }
    if items.len() > MAX_FINDINGS {
        return fail(format!("{label} exceeds {MAX_FINDINGS} selectors"));
    }
    let mut seen = HashSet::new();
    Ok(items
        .iter()
        .filter_map(|i| i.as_str())
        .filter(|s| seen.insert((*s).to_owned()))
        .map(str::to_owned)
        .collect())
}

/// `(finding_ids, urls, checks)` pulled from a saved verification view.
type ViewLists = (Vec<String>, Vec<String>, Vec<String>);

/// The view's three string lists, validated (`view must be a verification_view.v1 object`).
fn view_lists(view: Option<&Value>) -> Result<ViewLists, VerifyError> {
    let Some(view) = view else {
        return Ok((Vec::new(), Vec::new(), Vec::new()));
    };
    let Some(view) = view.as_object() else {
        return fail("view must be a verification_view.v1 object");
    };
    if view.get("schema_version") != Some(&Value::from("verification_view.v1")) {
        return fail("view must be a verification_view.v1 object");
    }
    Ok((
        strings(view.get("finding_ids"), "view.finding_ids")?,
        strings(view.get("urls"), "view.urls")?,
        strings(view.get("checks"), "view.checks")?,
    ))
}

/// `view names unknown checks: ...` when the view names ids outside the registry.
fn reject_unknown_checks(checks: &[String]) -> Result<(), VerifyError> {
    let registry = check_registry::builtin().map_err(|e| VerifyError(e.to_string()))?;
    let unknown: HashSet<String> = checks
        .iter()
        .filter(|c| registry.get(c).is_none())
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return fail(format!(
            "view names unknown checks: {}",
            sorted_join(&unknown)
        ));
    }
    Ok(())
}

/// `baseline findings name unknown checks: [...]` for ids outside the registry.
fn reject_invalid_checks<'a>(chosen: impl Iterator<Item = &'a Object>) -> Result<(), VerifyError> {
    let registry = check_registry::builtin().map_err(|e| VerifyError(e.to_string()))?;
    let mut invalid: Vec<Value> = Vec::new();
    for issue in chosen {
        let check = issue.get("check").cloned().unwrap_or(Value::Null);
        let known = check.as_str().is_some_and(|c| registry.get(c).is_some());
        if !known && !py_contains(&invalid, &check) {
            invalid.push(check);
        }
    }
    if !invalid.is_empty() {
        return fail(format!(
            "baseline findings name unknown checks: {}",
            sorted_list_repr(&invalid)
        ));
    }
    Ok(())
}

/// The document-side selection of `select()` (`chosen` issues, `targets` URLs).
pub fn select(
    baseline: &Object,
    finding_ids: Option<&Value>,
    urls: Option<&Value>,
    view: Option<&Value>,
) -> Result<(Vec<Object>, Vec<String>), VerifyError> {
    let issues = baseline.get("issues").and_then(Value::as_array);
    let pages = baseline.get("pages").and_then(Value::as_array);
    let (Some(issues), Some(pages)) = (issues, pages) else {
        return fail("baseline must be a saved audit with pages and issues");
    };
    let (view_ids, view_urls, checks) = view_lists(view)?;
    let ids: HashSet<String> = strings(finding_ids, "finding_ids")?
        .into_iter()
        .chain(view_ids)
        .collect();
    let wanted_urls: HashSet<String> = strings(urls, "urls")?
        .into_iter()
        .chain(view_urls)
        .collect();
    if ids.is_empty() && wanted_urls.is_empty() && checks.is_empty() {
        return fail("select at least one finding ID, saved view, or affected URL");
    }
    let known_ids: Vec<Value> = issues
        .iter()
        .filter_map(|i| i.as_object())
        .filter_map(|i| i.get("id"))
        .cloned()
        .collect();
    let unknown: HashSet<String> = ids
        .iter()
        .filter(|id| !py_contains(&known_ids, &Value::from(id.as_str())))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return fail(format!(
            "finding IDs are absent from baseline: {}",
            sorted_join(&unknown)
        ));
    }
    reject_unknown_checks(&checks)?;
    let baseline_urls: HashSet<&str> = pages
        .iter()
        .filter_map(|p| p.as_object())
        .filter_map(|p| p.get("url"))
        .filter_map(Value::as_str)
        .collect();
    let unknown_urls: HashSet<String> = wanted_urls
        .iter()
        .filter(|u| !baseline_urls.contains(u.as_str()))
        .cloned()
        .collect();
    if !unknown_urls.is_empty() {
        return fail(format!(
            "URLs are absent from baseline pages: {}",
            sorted_join(&unknown_urls)
        ));
    }
    let chosen: Vec<&Object> = issues
        .iter()
        .filter_map(|i| i.as_object())
        .filter(|i| {
            i.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| ids.contains(id))
                || i.get("target_url")
                    .and_then(Value::as_str)
                    .is_some_and(|u| wanted_urls.contains(u))
                || i.get("check")
                    .and_then(Value::as_str)
                    .is_some_and(|c| checks.iter().any(|v| v == c))
        })
        .collect();
    if chosen.is_empty() {
        return fail("selection matches no baseline findings");
    }
    reject_invalid_checks(chosen.iter().copied())?;
    if chosen.len() > MAX_FINDINGS {
        return fail(format!("selection exceeds {MAX_FINDINGS} findings"));
    }
    let mut seen = HashSet::new();
    let targets: Vec<String> = chosen
        .iter()
        .copied()
        .filter(|i| {
            i.get("target_url").is_some_and(|v| v.is_string())
                && i.get("target_url")
                    .and_then(Value::as_str)
                    .is_some_and(|u| baseline_urls.contains(u))
                && local_issue(i)
        })
        .filter_map(|i| i.get("target_url").and_then(Value::as_str))
        .filter(|u| seen.insert((*u).to_owned()))
        .map(str::to_owned)
        .collect();
    if targets.len() > MAX_URLS {
        return fail(format!("selection exceeds {MAX_URLS} URLs"));
    }
    unique_rows(&chosen, "finding")?;
    let target_set: HashSet<&str> = targets.iter().map(String::as_str).collect();
    let selected_pages: Vec<&Object> = pages
        .iter()
        .filter_map(|p| p.as_object())
        .filter(|p| {
            p.get("url")
                .and_then(Value::as_str)
                .is_some_and(|u| target_set.contains(u))
        })
        .collect();
    unique_rows(&selected_pages, "page")?;
    Ok((chosen.into_iter().cloned().collect(), targets))
}

/// `select_source`: the bounded compact audit without materializing an audit.v2 corpus.
pub fn select_source(
    source: &AuditSource<'_>,
    finding_ids: Option<&Value>,
    urls: Option<&Value>,
    view: Option<&Value>,
) -> Result<(Object, Vec<Object>, Vec<String>, Object), VerifyError> {
    let AuditSource::Reader(reader) = *source else {
        let AuditSource::Document(document) = *source else {
            unreachable!()
        };
        let (selected, targets) = select(document, finding_ids, urls, view)?;
        let identity = source_identity(source)?;
        return Ok(((*document).clone(), selected, targets, identity));
    };
    let Some(header) = reader.header.as_object() else {
        return fail("audit.v2 source has no validated header/collection index");
    };
    if header.get("schema_version") != Some(&Value::from("2.0")) {
        return fail("baseline must be an audit.json schema_version 2.0 document");
    }
    if !reader.collections.contains_key("/issues") || !reader.collections.contains_key("/pages") {
        return fail("audit.v2 source lacks required /issues or /pages collections");
    }
    let (view_ids, view_urls, checks) = view_lists(view)?;
    let ids: HashSet<String> = strings(finding_ids, "finding_ids")?
        .into_iter()
        .chain(view_ids)
        .collect();
    let wanted_urls: HashSet<String> = strings(urls, "urls")?
        .into_iter()
        .chain(view_urls)
        .collect();
    if ids.is_empty() && wanted_urls.is_empty() && checks.is_empty() {
        return fail("select at least one finding ID, saved view, or affected URL");
    }
    reject_unknown_checks(&checks)?;

    let mut chosen: Vec<Object> = Vec::new();
    let mut found_ids: HashSet<String> = HashSet::new();
    for item in source.iter_collection("/issues")? {
        let Some(item) = item.as_object() else {
            return fail("audit.v2 issue collection contains a non-object");
        };
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            if ids.contains(id) {
                found_ids.insert(id.to_owned());
            }
        }
        let matched = item
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| ids.contains(id))
            || item
                .get("target_url")
                .and_then(Value::as_str)
                .is_some_and(|u| wanted_urls.contains(u))
            || item
                .get("check")
                .and_then(Value::as_str)
                .is_some_and(|c| checks.iter().any(|v| v == c));
        if matched {
            chosen.push(item.clone());
            if chosen.len() > MAX_FINDINGS {
                return fail(format!("selection exceeds {MAX_FINDINGS} findings"));
            }
        }
    }
    let unknown: HashSet<String> = ids.difference(&found_ids).cloned().collect();
    if !unknown.is_empty() {
        return fail(format!(
            "finding IDs are absent from baseline: {}",
            sorted_join(&unknown)
        ));
    }
    if chosen.is_empty() {
        return fail("selection matches no baseline findings");
    }
    reject_invalid_checks(chosen.iter())?;
    let mut seen_targets = HashSet::new();
    let targets: Vec<String> = chosen
        .iter()
        .filter(|i| i.get("target_url").is_some_and(|v| v.is_string()) && local_issue(i))
        .filter_map(|i| i.get("target_url").and_then(Value::as_str))
        .filter(|u| seen_targets.insert((*u).to_owned()))
        .map(str::to_owned)
        .collect();
    if targets.len() > MAX_URLS {
        return fail(format!("selection exceeds {MAX_URLS} URLs"));
    }
    let selected_page_urls: HashSet<String> = chosen
        .iter()
        .filter_map(|i| i.get("target_url"))
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let needed_pages: HashSet<String> = wanted_urls.union(&selected_page_urls).cloned().collect();
    unique_rows(&chosen.iter().collect::<Vec<_>>(), "finding")?;
    let mut pages: Vec<Value> = Vec::new();
    let mut seen_pages: HashSet<String> = HashSet::new();
    for page in source.iter_collection("/pages")? {
        if let Some(page) = page.as_object() {
            if let Some(url) = page.get("url").and_then(Value::as_str) {
                if needed_pages.contains(url) {
                    if !seen_pages.insert(url.to_owned()) {
                        return fail("selected baseline has duplicate page URLs");
                    }
                    pages.push(Value::Object(page.clone()));
                }
            }
        }
    }
    let missing_urls: HashSet<String> = wanted_urls
        .iter()
        .filter(|u| !seen_pages.contains(*u))
        .cloned()
        .collect();
    if !missing_urls.is_empty() {
        return fail(format!(
            "URLs are absent from baseline pages: {}",
            sorted_join(&missing_urls)
        ));
    }
    if targets.iter().any(|t| !seen_pages.contains(t)) {
        return fail("selected finding has no affected baseline page to recrawl");
    }
    let mut compact = header.clone();
    compact.insert(
        "issues".into(),
        Value::Array(chosen.iter().cloned().map(Value::Object).collect()),
    );
    compact.insert("pages".into(), Value::Array(pages));
    let identity = source_identity(source)?;
    setdefault_scan_uuid(&mut compact, &identity);
    Ok((compact, chosen, targets, identity))
}

/// `compact.setdefault("run", {}).setdefault("scan_uuid", identity["scan_uuid"])`.
fn setdefault_scan_uuid(compact: &mut Object, identity: &Object) {
    let run = compact
        .entry("run".to_owned())
        .or_insert_with(|| Value::Object(Object::new()));
    if let Value::Object(run) = run {
        run.entry("scan_uuid".to_owned())
            .or_insert_with(|| identity.get("scan_uuid").cloned().unwrap_or(Value::Null));
    }
}

pub fn compact_after_source(
    source: &AuditSource<'_>,
    selected: &[Object],
) -> Result<(Object, Object), VerifyError> {
    if selected.len() > MAX_FINDINGS {
        return fail(format!("selection exceeds {MAX_FINDINGS} findings"));
    }
    unique_rows(&selected.iter().collect::<Vec<_>>(), "finding")?;
    let AuditSource::Reader(reader) = *source else {
        let AuditSource::Document(document) = *source else {
            unreachable!()
        };
        let identity = source_identity(source)?;
        return Ok(((*document).clone(), identity));
    };
    let pairs: Vec<(Value, Value)> = selected
        .iter()
        .map(|item| {
            (
                item.get("check").cloned().unwrap_or(Value::Null),
                item.get("target_url").cloned().unwrap_or(Value::Null),
            )
        })
        .filter(|(c, u)| c.is_string() && u.is_string())
        .collect();
    let wanted_urls: Vec<Value> = pairs.iter().map(|(_, u)| u.clone()).collect();
    let Some(header) = reader.header.as_object() else {
        return fail("after audit.v2 source has no validated header/collection index");
    };
    if !reader.collections.contains_key("/issues") || !reader.collections.contains_key("/pages") {
        return fail("after audit.v2 source lacks required /issues or /pages collections");
    }
    let mut issues = Vec::new();
    let mut issue_keys: Vec<(Value, Value)> = Vec::new();
    for item in source.iter_collection("/issues")? {
        let Some(item) = item.as_object() else {
            return fail("after issue collection contains a non-object");
        };
        let key = (
            item.get("check").cloned().unwrap_or(Value::Null),
            item.get("target_url").cloned().unwrap_or(Value::Null),
        );
        if pairs
            .iter()
            .any(|(c, u)| py_eq(c, &key.0) && py_eq(u, &key.1))
        {
            if issue_keys
                .iter()
                .any(|(c, u)| py_eq(c, &key.0) && py_eq(u, &key.1))
            {
                return fail("selected after audit has duplicate finding keys");
            }
            issue_keys.push(key);
            issues.push(Value::Object(item.clone()));
        }
    }
    let mut pages = Vec::new();
    let mut page_urls: Vec<Value> = Vec::new();
    for page in source.iter_collection("/pages")? {
        let Some(page) = page.as_object() else {
            return fail("after page collection contains a non-object");
        };
        let url = page.get("url").cloned().unwrap_or(Value::Null);
        if py_contains(&wanted_urls, &url) {
            if py_contains(&page_urls, &url) {
                return fail("selected after audit has duplicate page URLs");
            }
            page_urls.push(url);
            pages.push(Value::Object(page.clone()));
        }
    }
    let mut compact = header.clone();
    compact.insert("issues".into(), Value::Array(issues));
    compact.insert("pages".into(), Value::Array(pages));
    let identity = source_identity(source)?;
    setdefault_scan_uuid(&mut compact, &identity);
    Ok((compact, identity))
}
