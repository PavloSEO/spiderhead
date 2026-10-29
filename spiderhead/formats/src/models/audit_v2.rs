//! The audit.v2 projection of an [`AuditResult`]: a JSON header plus ordered, re-iterable
//! collections that a streaming writer can consume one row at a time.

use super::{AuditResult, Object};
use serde_json::Value;

/// One row iterator of a collection.
pub type RowIter<'a> = Box<dyn Iterator<Item = Value> + 'a>;

/// A collection of rows that can be iterated more than once, without materializing it.
pub struct Rows<'a> {
    make: Box<dyn Fn() -> RowIter<'a> + 'a>,
}

impl<'a> Rows<'a> {
    /// Wrap a factory that starts a fresh pass over the rows on every call.
    pub fn new(make: impl Fn() -> RowIter<'a> + 'a) -> Self {
        Self {
            make: Box::new(make),
        }
    }

    /// Start a new pass over the rows.
    pub fn iter(&self) -> RowIter<'a> {
        (self.make)()
    }
}

/// The header and the ordered collections of one audit.v2 document.
pub struct AuditV2Parts<'a> {
    /// The JSON header, with each collection's place left empty.
    pub header: Object,
    /// JSON pointer of each collection, in document order, with its rows.
    pub collections: Vec<(String, Rows<'a>)>,
}

/// Escape one JSON pointer token (RFC 6901: `~` first, then `/`).
pub fn escape_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Undo [`escape_pointer_token`] (RFC 6901: `~1` first, then `~0`).
fn unescape_pointer_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

/// Restore one collection into its compatibility location in `document`.
///
/// Missing intermediate objects are created. audit.v2 pointers only ever name object paths,
/// so no list-index branch is needed.
pub(super) fn set_collection(document: &mut Object, pointer: &str, rows: Vec<Value>) {
    let path = pointer.strip_prefix('/').unwrap_or(pointer);
    let parts: Vec<String> = path.split('/').map(unescape_pointer_token).collect();
    let Some((key, parents)) = parts.split_last() else {
        return;
    };
    let mut current = document;
    for part in parents {
        let entry = current
            .entry(part.clone())
            .or_insert_with(|| Value::Object(Object::new()));
        let Value::Object(map) = entry else {
            return;
        };
        current = map;
    }
    current.insert(key.clone(), Value::Array(rows));
}

impl AuditResult {
    /// Split the result into the audit.v2 header and its collections.
    ///
    /// Sitemap reconciliation lists move out of the header into their own collections
    /// (`/summary/sitemap/<name>`), so a large sitemap does not inflate the header; the
    /// header keeps each such key with an empty list.
    pub fn audit_v2_parts(&self) -> AuditV2Parts<'_> {
        let mut run = self.run.clone();
        run.insert(
            "checks_skipped".into(),
            Value::Array(
                self.skipped
                    .iter()
                    .map(|s| Value::Object(s.to_json()))
                    .collect(),
            ),
        );
        run.insert(
            "checks_disabled".into(),
            Value::Array(
                self.disabled
                    .iter()
                    .map(|d| Value::Object(d.to_json()))
                    .collect(),
            ),
        );

        let mut summary = self.summary.clone();
        let mut collections: Vec<(String, Rows<'_>)> = vec![
            (
                "/issues".into(),
                Rows::new(move || Box::new(self.issues.iter().map(|i| Value::Object(i.to_json())))),
            ),
            (
                "/pages".into(),
                Rows::new(move || Box::new(self.pages.iter().map(|p| Value::Object(p.to_json())))),
            ),
            (
                "/groups".into(),
                Rows::new(move || Box::new(self.groups.iter().map(|g| Value::Object(g.to_json())))),
            ),
        ];

        let mut suppressed_header = None;
        if !self.suppressed_issues.is_empty() {
            suppressed_header = Some(Value::Array(Vec::new()));
            collections.push((
                "/suppressed_issues".into(),
                Rows::new(move || Box::new(self.suppressed_issues.iter().cloned())),
            ));
        }
        if let (Some(Value::Object(original)), Some(Value::Object(copy))) =
            (self.summary.get("sitemap"), summary.get_mut("sitemap"))
        {
            for (name, values) in original {
                let Value::Array(list) = values else {
                    continue;
                };
                copy.insert(name.clone(), Value::Array(Vec::new()));
                let pointer = format!("/summary/sitemap/{}", escape_pointer_token(name));
                collections.push((pointer, Rows::new(move || Box::new(list.iter().cloned()))));
            }
        }

        let mut tool = Object::new();
        tool.insert("name".into(), Value::String("Export Analyzer".into()));
        // The stamp is the toolkit release version, not a
        // crate version; saved scans reject a tool version that disagrees.
        tool.insert(
            "version".into(),
            Value::String(spiderhead_core::user_agents::PRODUCT_VERSION.into()),
        );
        tool.insert("generated_by".into(), Value::String("SEOHead".into()));
        let mut header = Object::new();
        header.insert("schema_version".into(), Value::String("2.0".into()));
        header.insert("tool".into(), Value::Object(tool));
        header.insert("run".into(), Value::Object(run));
        header.insert("summary".into(), Value::Object(summary));
        header.insert("issues".into(), Value::Array(Vec::new()));
        header.insert("pages".into(), Value::Array(Vec::new()));
        header.insert("groups".into(), Value::Array(Vec::new()));
        if let Some(placeholder) = suppressed_header {
            header.insert("suppressed_issues".into(), placeholder);
        }
        AuditV2Parts {
            header,
            collections,
        }
    }
}
