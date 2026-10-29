//! Compare plumbing shared by the inline diff and the file-backed engine: the error
//! type, the closed URL-correspondence declaration, finding identity, audit source
//! adapters and the declared-pair resolution that binds one baseline to one follow-up.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::OnceLock;

use serde_json::Value;
use spiderhead_parse::py_str::py_repr;
use spiderhead_store::evidence_contract::Obj;

use crate::pyurl::urlsplit;
use crate::pyvalue::str_or_dumps;
pub(crate) use crate::verification::AuditSource;

/// Two audits that cannot be compared without lying about the result.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CompareError(pub String);

/// Schema tag of the correspondence declaration.
pub(crate) const CORRESPONDENCE_SCHEMA: &str = "url-correspondence.v1";
/// Schema tag of the reviewed-release attachment.
pub(crate) const RELEASE_REVIEW_SCHEMA: &str = "release_review.v1";

const CORRESPONDENCE_KEYS: [&str; 3] = ["schema_version", "origin_map", "pairs"];
const PAIR_KEYS: [&str; 2] = ["before", "after"];

const DECLARATION_LIMIT: usize = 16 * 1024 * 1024;

/// A URL-correspondence declaration reduced to what pairing needs: ordered origin
/// rewrites and explicit before→after pairs (kept sorted, the only order used).
#[derive(Debug, Clone)]
pub(crate) struct Declaration {
    pub origin_map: Obj,
    pub pairs: BTreeMap<String, String>,
}

/// The declared pairs bound to the retained page rows, with every miss exposed.
pub(crate) struct ResolvedCorrespondence {
    pub before_to_after: BTreeMap<String, String>,
    pub origin_map: Obj,
    pub pairs: Vec<Obj>,
    pub before_pages: HashMap<String, Obj>,
    pub after_pages: HashMap<String, Obj>,
}

fn empty_obj() -> &'static Obj {
    static EMPTY: OnceLock<Obj> = OnceLock::new();
    EMPTY.get_or_init(Obj::new)
}

/// The header an audit source exposes: the document itself, or the reader's header
/// when it is an object.
pub fn header_obj<'a>(source: &'a AuditSource<'_>) -> &'a Obj {
    match source {
        AuditSource::Document(document) => document,
        AuditSource::Reader(reader) => reader.header.as_object().unwrap_or(empty_obj()),
    }
}

/// The `run` manifest of a header, or an empty map when it is absent or not an object.
pub fn run_of(header: &Obj) -> &Obj {
    header
        .get("run")
        .and_then(Value::as_object)
        .unwrap_or(empty_obj())
}

/// One collection of an audit source materialized: documents read the key directly,
/// readers refuse a missing collection by name instead of yielding nothing.
pub fn iter_rows(source: &AuditSource<'_>, name: &str) -> Result<Vec<Value>, CompareError> {
    match source {
        AuditSource::Reader(reader) => {
            let pointer = format!("/{name}");
            if !reader.collections.contains_key(&pointer) {
                return Err(CompareError(format!(
                    "audit.v2 is missing its {name} collection"
                )));
            }
            reader
                .iter_collection(&pointer)
                .map_err(|e| CompareError(e.to_string()))
        }
        AuditSource::Document(document) => Ok(document
            .get(name)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()),
    }
}

/// Whether an audit source carries the key at its top level.
pub fn has_key(source: &AuditSource<'_>, name: &str) -> bool {
    match source {
        AuditSource::Document(document) => document.contains_key(name),
        // reference tests `"pages" in audit` — membership of the key itself, so a
        // null value still counts as present.
        AuditSource::Reader(reader) => reader.header.get(name).is_some(),
    }
}

/// `(check, target_url)` — the same finding on the same page, across runs. An
/// audit-wide issue has no target_url and keys on `""`, which is never a real URL.
pub(crate) fn key_of(issue: &Obj) -> (String, String) {
    let check = issue
        .get("check")
        .filter(|v| !v.is_null())
        .map_or_else(String::new, str_or_dumps);
    let target = match issue
        .get("target_url")
        .filter(|v| crate::pyvalue::truthy(v))
    {
        Some(v) => str_or_dumps(v),
        None => String::new(),
    };
    (check, target)
}

/// reference `repr` of a `(check, target_url)` finding key, for error text parity.
pub(crate) fn repr_key(key: &(String, String)) -> String {
    format!("({}, {})", py_repr(&key.0), py_repr(&key.1))
}

/// `scheme://netloc` of a URL, verbatim from the split.
pub(crate) fn origin_of(url: &str) -> String {
    let parts = urlsplit(url);
    format!("{}://{}", parts.scheme, parts.netloc)
}

/// Whether the two URLs resolve to different hostnames.
pub(crate) fn host_changed(before: &str, after: &str) -> bool {
    urlsplit(before).hostname() != urlsplit(after).hostname()
}

/// Validate one `scheme://host` origin string and return its canonical form.
pub(crate) fn origin(value: &str, label: &str) -> Result<String, CompareError> {
    let parts = urlsplit(value);
    if !matches!(parts.scheme.as_str(), "http" | "https")
        || parts.netloc.is_empty()
        || !parts.path.is_empty()
        || !parts.query.is_empty()
        || !parts.fragment.is_empty()
        || parts.username().is_some()
        || parts.password().is_some()
    {
        return Err(CompareError(format!(
            "{label} must be an http(s) origin without a path, query, fragment, or userinfo"
        )));
    }
    Ok(format!("{}://{}", parts.scheme, parts.netloc))
}

/// Validate one absolute http(s) URL without userinfo.
pub(crate) fn validate_url(value: Option<&Value>, label: &str) -> Result<String, CompareError> {
    let text = match value {
        Some(Value::String(text)) if !text.is_empty() => text.as_str(),
        _ => {
            return Err(CompareError(format!(
                "{label} must be a non-empty absolute http(s) URL"
            )))
        }
    };
    let parts = urlsplit(text);
    if !matches!(parts.scheme.as_str(), "http" | "https")
        || parts.netloc.is_empty()
        || parts.username().is_some()
        || parts.password().is_some()
    {
        return Err(CompareError(format!(
            "{label} must be a non-empty absolute http(s) URL without userinfo"
        )));
    }
    Ok(text.to_owned())
}

/// One declared destination and the declaration kind that produced it.
pub(crate) fn mapped_url(
    url: &str,
    declaration: &Declaration,
) -> (Option<String>, Option<&'static str>) {
    let explicit = declaration.pairs.get(url);
    if let Some(target) = explicit {
        return (Some(target.clone()), Some("explicit_pair"));
    }
    let parts = urlsplit(url);
    if parts.netloc.is_empty() {
        return (None, None);
    }
    let origin = format!("{}://{}", parts.scheme, parts.netloc);
    let Some(target_origin) = declaration.origin_map.get(&origin) else {
        return (None, None);
    };
    let Some(target_origin) = target_origin.as_str() else {
        return (None, None);
    };
    (
        Some(format!("{target_origin}{}", &url[origin.len()..])),
        Some("origin_map"),
    )
}

/// Every page row keyed by its URL, refusing duplicates: pairing must be one-to-one.
pub(crate) fn page_rows(source: &AuditSource<'_>) -> Result<HashMap<String, Obj>, CompareError> {
    let mut rows: HashMap<String, Obj> = HashMap::new();
    for page in iter_rows(source, "pages")? {
        let Some(page) = page.as_object() else {
            continue;
        };
        let Some(url) = page.get("url").filter(|u| crate::pyvalue::truthy(u)) else {
            continue;
        };
        let Some(url) = url.as_str() else { continue };
        if rows.contains_key(url) {
            return Err(CompareError(format!(
                "url correspondence cannot pair duplicate page URL {}",
                py_repr(url)
            )));
        }
        rows.insert(url.to_owned(), page.clone());
    }
    Ok(rows)
}

/// Bind declared pairs to the retained page rows and expose every miss: matched,
/// declared-but-not-crawled on either side, and any collision.
pub(crate) fn resolve_correspondence(
    before: &AuditSource<'_>,
    after: &AuditSource<'_>,
    declaration: &Declaration,
) -> Result<ResolvedCorrespondence, CompareError> {
    let before_pages = page_rows(before)?;
    let after_pages = page_rows(after)?;
    let mut resolved: BTreeMap<String, String> = BTreeMap::new();
    let mut pair_rows: Vec<Obj> = Vec::new();
    let mut seen_after: HashMap<String, String> = HashMap::new();
    for before_url in before_pages.keys().collect::<BTreeSet<_>>() {
        let (Some(after_url), Some(kind)) = mapped_url(before_url, declaration) else {
            continue;
        };
        let mut row = Obj::new();
        row.insert("before_url".into(), Value::String(before_url.clone()));
        row.insert("after_url".into(), Value::String(after_url.clone()));
        row.insert(
            "correspondence_key".into(),
            Value::String(after_url.clone()),
        );
        row.insert("kind".into(), Value::String(kind.to_owned()));
        row.insert("before_origin".into(), Value::String(origin_of(before_url)));
        row.insert("after_origin".into(), Value::String(origin_of(&after_url)));
        row.insert(
            "host_changed".into(),
            Value::Bool(host_changed(before_url, &after_url)),
        );
        if !after_pages.contains_key(&after_url) {
            row.insert("state".into(), Value::String("after_not_crawled".into()));
            pair_rows.push(row);
            continue;
        }
        if let Some(other_before) = seen_after.get(&after_url) {
            if other_before != before_url {
                return Err(CompareError(format!(
                    "url correspondence maps both {} and {} to crawled page {}",
                    py_repr(other_before),
                    py_repr(before_url),
                    py_repr(&after_url)
                )));
            }
        }
        seen_after.insert(after_url.clone(), before_url.clone());
        resolved.insert(before_url.clone(), after_url.clone());
        row.insert("state".into(), Value::String("matched".into()));
        pair_rows.push(row);
    }

    // Explicit entries outside the saved baseline remain visible rather than
    // disappearing from a review just because one crawl did not contain them.
    let declared_before: BTreeSet<String> = pair_rows
        .iter()
        .filter_map(|r| r.get("before_url").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    for (before_url, after_url) in &declaration.pairs {
        if declared_before.contains(before_url.as_str()) {
            continue;
        }
        let mut row = Obj::new();
        row.insert("before_url".into(), Value::String(before_url.clone()));
        row.insert("after_url".into(), Value::String(after_url.clone()));
        row.insert(
            "correspondence_key".into(),
            Value::String(after_url.clone()),
        );
        row.insert("kind".into(), Value::String("explicit_pair".into()));
        row.insert("before_origin".into(), Value::String(origin_of(before_url)));
        row.insert("after_origin".into(), Value::String(origin_of(after_url)));
        row.insert(
            "host_changed".into(),
            Value::Bool(host_changed(before_url, after_url)),
        );
        row.insert("state".into(), Value::String("before_not_crawled".into()));
        pair_rows.push(row);
    }
    let mapped_targets: BTreeSet<&str> = before_pages
        .keys()
        .map(|url| resolved.get(url).map(String::as_str).unwrap_or(url))
        .collect();
    if mapped_targets.len() != before_pages.len() {
        return Err(CompareError(
            "url correspondence has a page collision with an unmapped baseline URL".into(),
        ));
    }
    Ok(ResolvedCorrespondence {
        before_to_after: resolved,
        origin_map: declaration.origin_map.clone(),
        pairs: pair_rows,
        before_pages,
        after_pages,
    })
}

/// Every issue keyed for diffing, optionally remapped through a baseline→follow-up
/// correspondence — including audit-wide ones, which key on an empty URL.
pub(crate) fn mapped_issues(
    source: &AuditSource<'_>,
    before_to_after: Option<&BTreeMap<String, String>>,
) -> Result<HashMap<(String, String), Obj>, CompareError> {
    let mut records: HashMap<(String, String), Obj> = HashMap::new();
    for issue in iter_rows(source, "issues")? {
        let Some(issue) = issue.as_object() else {
            continue;
        };
        let mut key = key_of(issue);
        if let Some(mapping) = before_to_after {
            if let Some(target) = mapping.get(&key.1) {
                key.1 = target.clone();
            }
        }
        if records.contains_key(&key) {
            return Err(CompareError(format!(
                "comparison has duplicate finding key {}",
                repr_key(&key)
            )));
        }
        records.insert(key, issue.clone());
    }
    Ok(records)
}

/// Every crawled URL, refusing duplicates: a URL seen twice is a crawl defect
/// the diff cannot paper over.
pub(crate) fn crawled_urls(source: &AuditSource<'_>) -> Result<BTreeSet<String>, CompareError> {
    let mut urls = BTreeSet::new();
    for page in iter_rows(source, "pages")? {
        let Some(page) = page.as_object() else {
            continue;
        };
        let Some(url) = page.get("url").filter(|u| crate::pyvalue::truthy(u)) else {
            continue;
        };
        let Some(url) = url.as_str() else { continue };
        if !urls.insert(url.to_owned()) {
            return Err(CompareError(format!(
                "comparison has duplicate page URL {}",
                py_repr(url)
            )));
        }
    }
    Ok(urls)
}

/// reference `strerror` for the errno values a declaration read can hit; anything
/// else falls back to the local error rendering.
pub(crate) fn strerror(code: i32) -> &'static str {
    match code {
        1 => "Operation not permitted",
        2 => "No such file or directory",
        9 => "Bad file descriptor",
        13 => "Permission denied",
        16 => "Resource busy",
        17 => "File exists",
        20 => "Not a directory",
        21 => "Is a directory",
        28 => "No space left on device",
        30 => "Read-only file system",
        39 => "Directory not empty",
        62 => "Too many levels of symbolic links",
        66 => "Directory not empty",
        _ => "",
    }
}

pub(crate) fn os_error_text(error: &std::io::Error, path: &Path) -> String {
    match error.raw_os_error() {
        Some(code) if !strerror(code).is_empty() => {
            format!("[Errno {code}] {}: '{}'", strerror(code), path.display())
        }
        _ => error.to_string(),
    }
}

/// the reference `'utf-8' codec can't decode …` text for one invalid byte.
fn utf8_error_text(bytes: &[u8], error: &std::str::Utf8Error) -> String {
    let position = error.valid_up_to();
    let byte = bytes.get(position).copied().unwrap_or(0);
    let reason = if error.error_len().is_some() {
        "invalid start byte"
    } else {
        "unexpected end of data"
    };
    format!("'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: {reason}")
}

/// First repeated object key in a JSON document, in the order the reference decoder
/// reports them: when an object closes, its pairs hook runs before any enclosing
/// object completes, so innermost frames win and, within one frame, the first
/// key that repeats in document order. serde silently keeps the last value, so
/// valid documents are screened by text after a successful parse.
pub(crate) fn first_duplicate_key(text: &str) -> Option<String> {
    struct Frame {
        is_object: bool,
        seen: Vec<String>,
    }
    enum Phase {
        Key,
        Colon,
        Value,
        Comma,
    }
    let bytes = text.as_bytes();
    let mut stack: Vec<Frame> = vec![Frame {
        is_object: false,
        seen: Vec::new(),
    }];
    let mut phases: Vec<Phase> = vec![Phase::Value];
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b' ' | b'\t' | b'\n' | b'\r' => index += 1,
            b'"' => {
                let mut cursor = index + 1;
                while cursor < bytes.len() {
                    match bytes[cursor] {
                        b'\\' => cursor += 2,
                        b'"' => break,
                        _ => cursor += 1,
                    }
                }
                let decoded = serde_json::from_str::<String>(&text[index..cursor + 1]).ok()?;
                if matches!(phases.last(), Some(Phase::Key)) {
                    stack.last_mut()?.seen.push(decoded);
                    *phases.last_mut()? = Phase::Colon;
                } else if matches!(phases.last(), Some(Phase::Value)) {
                    *phases.last_mut()? = Phase::Comma;
                }
                index = cursor + 1;
            }
            b'{' => {
                stack.push(Frame {
                    is_object: true,
                    seen: Vec::new(),
                });
                phases.push(Phase::Key);
                index += 1;
            }
            b'[' => {
                stack.push(Frame {
                    is_object: false,
                    seen: Vec::new(),
                });
                phases.push(Phase::Value);
                index += 1;
            }
            b'}' | b']' => {
                let frame = stack.pop()?;
                phases.pop()?;
                if frame.is_object {
                    for (position, key) in frame.seen.iter().enumerate() {
                        if frame.seen[..position].contains(key) {
                            return Some(key.clone());
                        }
                    }
                }
                index += 1;
            }
            b':' => {
                if matches!(phases.last(), Some(Phase::Colon)) {
                    *phases.last_mut()? = Phase::Value;
                }
                index += 1;
            }
            b',' => {
                let next = if stack.last()?.is_object {
                    Phase::Key
                } else {
                    Phase::Value
                };
                *phases.last_mut()? = next;
                index += 1;
            }
            _ => {
                // A literal (number, true, false, null): consume the token and
                // settle the value slot.
                while index < bytes.len()
                    && !matches!(
                        bytes[index],
                        b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r'
                    )
                {
                    index += 1;
                }
                if matches!(phases.last(), Some(Phase::Value)) {
                    *phases.last_mut()? = Phase::Comma;
                }
            }
        }
    }
    None
}

/// Read the deliberately small, closed URL-pairing declaration: a JSON object or
/// the path of a JSON file. It is intentionally not a redirect matcher and never
/// looks at page text or titles — a release reviewer declares the relationship.
pub(crate) fn load_correspondence(value: &Value) -> Result<Declaration, CompareError> {
    let parsed: Value = if let Some(path_text) = value.as_str() {
        let path = Path::new(path_text);
        let mut content = Vec::new();
        match std::fs::File::open(path) {
            Ok(file) => {
                use std::io::Read;
                let mut limited = file.take((DECLARATION_LIMIT + 1) as u64);
                if let Err(error) = limited.read_to_end(&mut content) {
                    return Err(CompareError(format!(
                        "could not read url correspondence: {}",
                        os_error_text(&error, path)
                    )));
                }
            }
            Err(error) => {
                return Err(CompareError(format!(
                    "could not read url correspondence: {}",
                    os_error_text(&error, path)
                )))
            }
        }
        if content.len() > DECLARATION_LIMIT {
            return Err(CompareError(
                "url correspondence exceeds the 16 MiB declaration limit".into(),
            ));
        }
        let text = match std::str::from_utf8(&content) {
            Ok(text) => text.to_owned(),
            Err(error) => {
                return Err(CompareError(format!(
                    "url correspondence is not valid JSON: {}",
                    utf8_error_text(&content, &error)
                )))
            }
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(parsed) => {
                if let Some(key) = first_duplicate_key(&text) {
                    return Err(CompareError(format!(
                        "url correspondence repeats object key {}",
                        py_repr(&key)
                    )));
                }
                parsed
            }
            Err(error) => {
                return Err(CompareError(format!(
                    "url correspondence is not valid JSON: {}",
                    error
                )))
            }
        }
    } else {
        value.clone()
    };
    let Some(document) = parsed.as_object() else {
        return Err(CompareError(
            "url correspondence must be an object or a JSON file path".into(),
        ));
    };
    let mut extra: Vec<&str> = document
        .keys()
        .filter(|k| !CORRESPONDENCE_KEYS.contains(&k.as_str()))
        .map(String::as_str)
        .collect();
    extra.sort_unstable();
    let mut missing: Vec<&str> = CORRESPONDENCE_KEYS
        .iter()
        .filter(|k| !document.contains_key(**k))
        .copied()
        .collect();
    missing.sort_unstable();
    if !extra.is_empty() || !missing.is_empty() {
        let mut details: Vec<String> = Vec::new();
        if !extra.is_empty() {
            details.push(format!("unknown keys: {}", extra.join(", ")));
        }
        if !missing.is_empty() {
            details.push(format!("missing keys: {}", missing.join(", ")));
        }
        return Err(CompareError(format!(
            "url correspondence has a closed schema ({})",
            details.join("; ")
        )));
    }
    if document
        .get("schema_version")
        .map(|v| !matches!(v, Value::String(s) if s == CORRESPONDENCE_SCHEMA))
        .unwrap_or(true)
    {
        return Err(CompareError(format!(
            "url correspondence schema_version must be {}",
            py_repr(CORRESPONDENCE_SCHEMA)
        )));
    }

    let Some(raw_origins) = document.get("origin_map").and_then(Value::as_object) else {
        return Err(CompareError(
            "url correspondence origin_map must be an object".into(),
        ));
    };
    if raw_origins.len() > 10_000 {
        return Err(CompareError(
            "url correspondence exceeds 10000 origin declarations".into(),
        ));
    }
    let mut origins = Obj::new();
    // Object keys are always strings; only the value can be another JSON type.
    for (before, after) in raw_origins {
        let Some(after) = after.as_str() else {
            return Err(CompareError(
                "url correspondence origin_map keys and values must be strings".into(),
            ));
        };
        let before_origin = origin(before, "origin_map key")?;
        let after_origin = origin(after, "origin_map value")?;
        if *before != before_origin || after != after_origin {
            return Err(CompareError(
                "url correspondence origins must be written without a trailing slash".into(),
            ));
        }
        origins.insert(before_origin, Value::String(after_origin));
    }

    let Some(raw_pairs) = document.get("pairs").and_then(Value::as_array) else {
        return Err(CompareError(
            "url correspondence pairs must be a list".into(),
        ));
    };
    if raw_pairs.len() > 10_000 {
        return Err(CompareError(
            "url correspondence exceeds 10000 explicit pairs; use origin_map for an origin migration"
                .into(),
        ));
    }
    let mut pairs: BTreeMap<String, String> = BTreeMap::new();
    let mut reverse_pairs: BTreeMap<String, String> = BTreeMap::new();
    for (index, raw_pair) in raw_pairs.iter().enumerate() {
        let pair = raw_pair
            .as_object()
            .filter(|p| p.len() == PAIR_KEYS.len() && PAIR_KEYS.iter().all(|k| p.contains_key(*k)));
        let Some(pair) = pair else {
            return Err(CompareError(format!(
                "url correspondence pairs[{index}] must contain exactly before and after"
            )));
        };
        let before = validate_url(pair.get("before"), &format!("pairs[{index}].before"))?;
        let after = validate_url(pair.get("after"), &format!("pairs[{index}].after"))?;
        if pairs.contains_key(&before) {
            return Err(CompareError(format!(
                "url correspondence maps {} more than once",
                py_repr(&before)
            )));
        }
        if let Some(other) = reverse_pairs.get(&after) {
            return Err(CompareError(format!(
                "url correspondence maps both {} and {} to {}",
                py_repr(other),
                py_repr(&before),
                py_repr(&after)
            )));
        }
        pairs.insert(before.clone(), after.clone());
        reverse_pairs.insert(after, before);
    }
    Ok(Declaration {
        origin_map: origins,
        pairs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_object_keys_are_named() {
        // the reference object_pairs_hook fires per object: the same key in two
        // different objects is two declarations, not a repeat.
        assert_eq!(
            first_duplicate_key(r#"{"a": 1, "b": {"c": 2, "a": 3}}"#),
            None
        );
        assert_eq!(
            first_duplicate_key(r#"{"a": 1, "b": 2, "a": 3}"#),
            Some("a".into())
        );
        assert_eq!(first_duplicate_key(r#"{"a": 1, "b": 2}"#), None);
        assert_eq!(first_duplicate_key(r#"{"a": "a", "b": "a"}"#), None);
        assert_eq!(
            first_duplicate_key(r#"{"pairs": [{"before": "x", "before": "y"}]}"#),
            Some("before".into())
        );
    }

    #[test]
    fn origins_reject_paths_and_userinfo() {
        assert!(origin("https://a.example", "x").is_ok());
        assert!(origin("https://a.example/", "x").is_err());
        assert!(origin("ftp://a.example", "x").is_err());
        assert!(origin("https://u@a.example", "x").is_err());
        assert!(validate_url(Some(&Value::String("https://a.example/p".into())), "x").is_ok());
        assert!(validate_url(Some(&Value::String("".into())), "x").is_err());
        assert!(validate_url(None, "x").is_err());
    }

    #[test]
    fn mapped_url_prefers_explicit_pairs() {
        let mut origins = Obj::new();
        origins.insert(
            "https://old.example".into(),
            Value::String("https://new.example".into()),
        );
        let mut pairs = BTreeMap::new();
        pairs.insert(
            "https://old.example/special".into(),
            "https://new.example/other".into(),
        );
        let declaration = Declaration {
            origin_map: origins,
            pairs,
        };
        let (target, kind) = mapped_url("https://old.example/special", &declaration);
        assert_eq!(target.as_deref(), Some("https://new.example/other"));
        assert_eq!(kind, Some("explicit_pair"));
        let (target, kind) = mapped_url("https://old.example/page", &declaration);
        assert_eq!(target.as_deref(), Some("https://new.example/page"));
        assert_eq!(kind, Some("origin_map"));
        let (target, kind) = mapped_url("https://elsewhere.example/", &declaration);
        assert_eq!(target, None);
        assert_eq!(kind, None);
    }
}
