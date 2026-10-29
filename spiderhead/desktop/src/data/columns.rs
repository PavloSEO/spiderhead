//! Column allow-list of the URL table: name → SQL expression, value kind and
//! index flag. Port of the `COLUMNS` map in the reference suite
//! limited to what a `scan.v1` page table can answer — transport phases, resource
//! graph counters and click depth belong to `scan.v2`/graph slices and are absent.
//!
//! `issue_count` reads `temp.issue_url_counts`, built by
//! [`crate::data::session::ScanSession::materialize_issues`] before a query that
//! names the column runs — a measured zero, never a stand-in for a missing audit.

/// Value kind of a column; decides the allowed filter operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Integer column.
    Int,
    /// Floating point column.
    Real,
    /// Text column.
    Text,
    /// Boolean column (0/1 in SQL, `true`/`false` in JSON).
    Bool,
    /// Status-class pseudo-column (`"2xx"`..`"5xx"`, `"none"`).
    Class,
}

/// One allow-listed column.
#[derive(Debug, Clone, Copy)]
pub struct Column {
    /// SQL expression over `pages p`/`urls u`; never user text.
    pub expr: &'static str,
    /// Value kind for filter validation.
    pub kind: Kind,
    /// True when an index (or primary order) makes a whole-table sort cheap.
    pub indexed: bool,
}

const fn p(name: &'static str, kind: Kind, indexed: bool) -> Column {
    Column {
        expr: name,
        kind,
        indexed,
    }
}

/// The allow-list in declaration order. Expressions equal the reference verbatim so
/// row values and ordering match `scan-url-query` on the same file.
pub static COLUMNS: &[(&str, Column)] = &[
    ("url_id", p("p.url_id", Kind::Int, true)),
    ("url", p("u.url", Kind::Text, true)),
    ("page_ordinal", p("p.page_ordinal", Kind::Int, true)),
    // `pages_status` exists in every scan.v1; the query layer still checks.
    ("status_code", p("p.status_code", Kind::Int, false)),
    (
        "status_class",
        p(
            "CASE WHEN p.status_code IS NULL THEN 'none' ELSE (p.status_code/100)||'xx' END",
            Kind::Class,
            false,
        ),
    ),
    ("content_type", p("p.content_type", Kind::Text, false)),
    ("size_bytes", p("p.size_bytes", Kind::Int, false)),
    ("response_time", p("p.response_time", Kind::Real, false)),
    ("redirect_url", p("p.redirect_url", Kind::Text, false)),
    ("final_url", p("p.final_url", Kind::Text, false)),
    ("title", p("p.title", Kind::Text, false)),
    (
        "meta_description",
        p("p.meta_description", Kind::Text, false),
    ),
    ("h1", p("p.h1", Kind::Text, false)),
    ("h1_2", p("p.h1_2", Kind::Text, false)),
    ("h2", p("p.h2", Kind::Text, false)),
    ("canonical", p("p.canonical", Kind::Text, false)),
    ("final_canonical", p("p.final_canonical", Kind::Text, false)),
    ("meta_robots", p("p.meta_robots", Kind::Text, false)),
    ("x_robots", p("p.x_robots", Kind::Text, false)),
    ("og_title", p("p.og_title", Kind::Text, false)),
    ("og_description", p("p.og_description", Kind::Text, false)),
    ("og_image", p("p.og_image", Kind::Text, false)),
    ("word_count", p("p.word_count", Kind::Int, false)),
    ("text_ratio", p("p.text_ratio", Kind::Real, false)),
    ("crawl_depth", p("p.crawl_depth", Kind::Int, false)),
    (
        "content_encoding",
        p("p.content_encoding", Kind::Text, false),
    ),
    ("charset", p("p.charset", Kind::Text, false)),
    ("outlinks", p("p.outlinks", Kind::Int, false)),
    (
        "external_outlinks",
        p("p.external_outlinks", Kind::Int, false),
    ),
    (
        "meta_description_count",
        p("p.meta_description_count", Kind::Int, false),
    ),
    ("images_total", p("p.images_total", Kind::Int, false)),
    (
        "images_missing_alt_attr",
        p("p.images_missing_alt_attr", Kind::Int, false),
    ),
    (
        "jsonld_blocks_found",
        p("p.jsonld_blocks_found", Kind::Int, false),
    ),
    ("error", p("p.error", Kind::Text, false)),
    ("error_kind", p("p.error_kind", Kind::Text, false)),
    ("cache_status", p("p.cache_status", Kind::Text, false)),
    ("representation", p("p.representation", Kind::Text, false)),
    ("title_length", p("length(p.title)", Kind::Int, false)),
    (
        "meta_description_length",
        p("length(p.meta_description)", Kind::Int, false),
    ),
    ("h1_length", p("length(p.h1)", Kind::Int, false)),
    (
        "inlinks",
        p(
            "(SELECT COUNT(*) FROM links l WHERE l.destination_url_id=p.url_id)",
            Kind::Int,
            false,
        ),
    ),
    (
        "issue_count",
        p(
            "COALESCE((SELECT n FROM temp.issue_url_counts c WHERE c.url_id=p.url_id), 0)",
            Kind::Int,
            false,
        ),
    ),
    (
        "title_duplicate",
        p(
            "EXISTS (SELECT 1 FROM pages t WHERE t.url_id<>p.url_id \
             AND trim(t.title)<>'' AND lower(t.title)=lower(p.title))",
            Kind::Bool,
            false,
        ),
    ),
    (
        "description_duplicate",
        p(
            "EXISTS (SELECT 1 FROM pages t WHERE t.url_id<>p.url_id \
             AND trim(t.meta_description)<>'' \
             AND lower(t.meta_description)=lower(p.meta_description))",
            Kind::Bool,
            false,
        ),
    ),
    (
        "has_hreflang",
        p(
            "CASE WHEN p.hreflang_json IS NULL THEN NULL \
             ELSE p.hreflang_json NOT IN ('[]','{}','') END",
            Kind::Bool,
            false,
        ),
    ),
    (
        "in_sitemap",
        p(
            "EXISTS (SELECT 1 FROM context_items s WHERE s.kind='sitemap_declared_url' \
             AND CAST(json_extract(s.payload_json,'$.url_id') AS INTEGER)=p.url_id)",
            Kind::Bool,
            false,
        ),
    ),
    (
        "canonical_self",
        p("(p.canonical<>'' AND p.canonical=u.url)", Kind::Bool, false),
    ),
    (
        "robots_blocked",
        p(
            "EXISTS (SELECT 1 FROM context_items c WHERE c.kind='robots_blocked_url' \
             AND c.item_key='url:' || p.url_id)",
            Kind::Bool,
            false,
        ),
    ),
    (
        "canonical_non_indexable",
        p(
            "EXISTS (SELECT 1 FROM pages t JOIN urls tu ON tu.url_id=t.url_id \
             WHERE tu.url=p.canonical \
             AND COALESCE(t.status_code BETWEEN 200 AND 299 \
             AND instr(lower(t.meta_robots||','||t.x_robots),'noindex')=0, 0)=0)",
            Kind::Bool,
            false,
        ),
    ),
    (
        "canonical_other",
        p(
            "(p.canonical<>'' AND p.canonical<>u.url)",
            Kind::Bool,
            false,
        ),
    ),
    // 2xx without a noindex directive in meta robots or X-Robots-Tag.
    (
        "indexable",
        p(
            "COALESCE(p.status_code BETWEEN 200 AND 299 AND \
             instr(lower(p.meta_robots||','||p.x_robots),'noindex')=0, 0)",
            Kind::Bool,
            false,
        ),
    ),
];

/// Grid columns `scan.open` preselects — the canvas layout's default set
/// (`url_id` rides along for row identity but is never drawn). The column
/// menu replaces this whole list; `DEFAULT_COLUMNS` stays the engine's
/// fallback for a `table.page` call that names no columns.
pub static UI_DEFAULT_COLUMNS: &[&str] = &[
    "url_id",
    "url",
    "status_code",
    "indexable",
    "title",
    "crawl_depth",
    "inlinks",
    "word_count",
    "response_time",
];

/// Columns the table shows before the user picks any.
pub static DEFAULT_COLUMNS: &[&str] = &[
    "url_id",
    "url",
    "status_code",
    "content_type",
    "crawl_depth",
    "title",
    "h1",
    "word_count",
    "size_bytes",
    "response_time",
    "indexable",
];

/// Look a column up by name; `None` rejects the name like the reference allow-list.
pub fn column(name: &str) -> Option<&'static Column> {
    COLUMNS.iter().find(|(n, _)| *n == name).map(|(_, c)| c)
}

/// Physical `pages` columns the schema check requires before serving queries.
pub fn required_page_columns() -> Vec<&'static str> {
    COLUMNS
        .iter()
        .filter_map(|(_, c)| {
            c.expr
                .strip_prefix("p.")
                .filter(|rest| rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        })
        .collect()
}

/// Largest filtered set that may be sorted without an index (temp b-tree).
pub const SORT_ROW_CAP: i64 = 100_000;
/// Page fetch budget for the interactive table.
pub const PAGE_BUDGET_SECONDS: f64 = 5.0;
/// Default budget of the filtered-total count.
pub const DEFAULT_COUNT_BUDGET_SECONDS: f64 = 1.0;
/// SQLite VM steps between deadline checks.
pub const PROGRESS_OPS: i32 = 1000;
/// Maximum rows a single `table.page` may return (reference parity).
pub const MAX_LIMIT: i64 = 200;
/// Maximum AND-combined filters in one query.
pub const MAX_FILTERS: usize = 20;
/// Maximum values of `in`/`not_in`.
pub const MAX_IN_VALUES: usize = 100;
/// Maximum length of a bound text value.
pub const MAX_TEXT_VALUE: usize = 2000;
/// Issue severities the issue filter accepts.
pub const ISSUE_SEVERITIES: [&str; 4] = ["critical", "warning", "notice", "opportunity"];
/// Maximum issue-check ids in one issue filter.
pub const MAX_ISSUE_CHECKS: usize = 50;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_column_name_is_unique() {
        let mut names: Vec<&str> = COLUMNS.iter().map(|(n, _)| *n).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
    }

    #[test]
    fn indexed_columns_match_python() {
        let indexed: Vec<&str> = COLUMNS
            .iter()
            .filter(|(_, c)| c.indexed)
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(indexed, ["url_id", "url", "page_ordinal"]);
    }

    #[test]
    fn required_columns_are_plain_pages_names() {
        let required = required_page_columns();
        assert!(required.contains(&"title"));
        // Only plain `p.<name>` expressions qualify, like the reference `_REQUIRED_PAGE_COLUMNS`.
        assert!(!required.contains(&"hreflang_json"));
        assert!(!required.contains(&"title length"));
    }
}
