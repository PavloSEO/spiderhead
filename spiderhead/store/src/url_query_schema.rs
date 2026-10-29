//! Fixed query vocabulary for scan-url-query: constants, column allow-list, errors.
//!

use std::sync::OnceLock;

use indexmap::IndexMap;

/// Wire format tag of a query answer.
pub const FORMAT: &str = "spiderhead.scan-url-query.v1";
/// Rows per page when the caller names no limit.
pub const DEFAULT_LIMIT: i64 = 200;
/// Largest page.
pub const MAX_LIMIT: i64 = 200;
/// Most filters in one query.
pub const MAX_FILTERS: i64 = 20;
/// Most values in one `in`/`not_in` list.
pub const MAX_IN_VALUES: i64 = 100;
/// Longest text filter value, in characters.
pub const MAX_TEXT_VALUE: i64 = 2000;
/// Largest filtered set that may be sorted without an index: SQLite sorts it in a temp b-tree.
pub const SORT_ROW_CAP: i64 = 100_000;
/// Wall-clock budget of one page read.
pub const PAGE_BUDGET_SECONDS: f64 = 5.0;
/// Default budget of the exact-count query.
pub const DEFAULT_COUNT_BUDGET_SECONDS: f64 = 1.0;
/// SQLite VM steps between deadline checks.
pub const PROGRESS_OPS: i64 = 1000;
/// Severities an issue filter may name.
pub const ISSUE_SEVERITIES: &[&str] = &["critical", "warning", "notice", "opportunity"];
/// Most check ids in one issue filter.
pub const MAX_ISSUE_CHECKS: i64 = 50;
/// Check-id pattern, matched case-insensitively and anchored to the whole string.
pub const ISSUE_CHECK: &str = "[a-z0-9][a-z0-9_.-]{0,99}";

/// True when `value` is a well-formed issue check id.
pub fn is_issue_check(value: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(&format!("(?i)^(?:{ISSUE_CHECK})$")).expect("static pattern")
    });
    re.is_match(value)
}

/// A rejected or failed query, carrying the machine-readable reason.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct QueryError {
    /// Human-readable message.
    pub message: String,
    /// Machine-readable reason.
    pub reason_code: String,
    /// `invalid`, `unavailable` or `capped`.
    pub state: String,
}

impl QueryError {
    /// Error in the default `invalid` state.
    pub fn new(reason_code: &str, message: impl Into<String>) -> Self {
        Self::with_state(reason_code, message, "invalid")
    }

    /// Error with an explicit state.
    pub fn with_state(reason_code: &str, message: impl Into<String>, state: &str) -> Self {
        QueryError {
            message: message.into(),
            reason_code: reason_code.into(),
            state: state.into(),
        }
    }
}

/// One allow-listed query column: its SQL expression, value kind and index status.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Column {
    /// Fixed SQL expression.
    pub expr: String,
    /// `int`, `real`, `text`, `bool` or `class`.
    pub kind: String,
    /// True when an index serves ordering by this column.
    pub indexed: bool,
}

impl Column {
    /// Column over an arbitrary expression.
    pub fn new(expr: &str, kind: &str, indexed: bool) -> Column {
        Column {
            expr: expr.to_string(),
            kind: kind.to_string(),
            indexed,
        }
    }
}

/// Column of the `pages` table.
pub fn pages(name: &str, kind: &str, indexed: bool) -> Column {
    Column::new(&format!("p.{name}"), kind, indexed)
}

fn resource_count(extra: &str) -> String {
    format!(
        "(SELECT COUNT(*) FROM resource_graph_occurrences r WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id{extra})"
    )
}

const PHASES: [&str; 5] = ["dns", "connect", "tls", "ttfb", "download"];

fn transport_columns_build() -> Vec<(String, Column)> {
    let mut out = Vec::new();
    for phase in PHASES {
        out.push((
            format!("{phase}_seconds"),
            Column::new(
                &format!(
                    "(SELECT t.{phase}_seconds FROM response_transport_meta t WHERE t.response_id={TRANSPORT_RESPONSE_ID})"
                ),
                "real",
                false,
            ),
        ));
        out.push((
            format!("{phase}_state"),
            Column::new(
                &format!(
                    "COALESCE((SELECT CASE WHEN t.{phase}_state='unknown' THEN 'unavailable' ELSE t.{phase}_state END FROM response_transport_meta t WHERE t.response_id={TRANSPORT_RESPONSE_ID}),'unavailable')"
                ),
                "text",
                false,
            ),
        ));
        out.push((
            format!("{phase}_reason"),
            Column::new(
                &format!(
                    "COALESCE((SELECT t.{phase}_reason FROM response_transport_meta t WHERE t.response_id={TRANSPORT_RESPONSE_ID}),'transport phase was not retained in this scan')"
                ),
                "text",
                false,
            ),
        ));
    }
    out
}

/// Transport-phase columns (`dns_seconds`, `dns_state`, ...), in definition order.
pub fn transport_columns() -> &'static IndexMap<String, Column> {
    static MAP: OnceLock<IndexMap<String, Column>> = OnceLock::new();
    MAP.get_or_init(|| transport_columns_build().into_iter().collect())
}

/// The single allow-list mapping column names to SQL expressions, in definition order.
pub fn columns() -> &'static IndexMap<String, Column> {
    static MAP: OnceLock<IndexMap<String, Column>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m: IndexMap<String, Column> = IndexMap::new();
        let mut put = |name: &str, column: Column| {
            m.insert(name.to_string(), column);
        };
        put("url_id", pages("url_id", "int", true));
        put("url", Column::new("u.url", "text", true));
        put("page_ordinal", pages("page_ordinal", "int", true));
        put("status_code", pages("status_code", "int", false));
        put(
            "status_class",
            Column::new(
                "CASE WHEN p.status_code IS NULL THEN 'none' ELSE (p.status_code/100)||'xx' END",
                "class",
                false,
            ),
        );
        for (name, kind) in [
            ("content_type", "text"),
            ("size_bytes", "int"),
            ("response_time", "real"),
            ("redirect_url", "text"),
            ("final_url", "text"),
            ("title", "text"),
            ("meta_description", "text"),
            ("h1", "text"),
            ("h1_2", "text"),
            ("h2", "text"),
            ("canonical", "text"),
            ("final_canonical", "text"),
            ("meta_robots", "text"),
            ("x_robots", "text"),
            ("og_title", "text"),
            ("og_description", "text"),
            ("og_image", "text"),
            ("word_count", "int"),
            ("text_ratio", "real"),
            ("crawl_depth", "int"),
        ] {
            put(name, pages(name, kind, false));
        }
        put(
            "click_depth",
            Column::new("(SELECT cd.depth FROM temp.cd_depth cd WHERE cd.url_id=p.url_id)", "int", false),
        );
        for (name, kind) in [
            ("content_encoding", "text"),
            ("charset", "text"),
            ("outlinks", "int"),
            ("external_outlinks", "int"),
            ("meta_description_count", "int"),
            ("images_total", "int"),
            ("images_missing_alt_attr", "int"),
            ("jsonld_blocks_found", "int"),
            ("error", "text"),
            ("error_kind", "text"),
            ("cache_status", "text"),
            ("representation", "text"),
        ] {
            put(name, pages(name, kind, false));
        }
        put("title_length", Column::new("length(p.title)", "int", false));
        put("meta_description_length", Column::new("length(p.meta_description)", "int", false));
        put("h1_length", Column::new("length(p.h1)", "int", false));
        put(
            "inlinks",
            Column::new("(SELECT COUNT(*) FROM links l WHERE l.destination_url_id=p.url_id)", "int", false),
        );
        put(
            "issue_count",
            Column::new("COALESCE((SELECT n FROM temp.issue_url_counts c WHERE c.url_id=p.url_id), 0)", "int", false),
        );
        put(
            "title_duplicate",
            Column::new(
                "EXISTS (SELECT 1 FROM pages t WHERE t.url_id<>p.url_id AND trim(t.title)<>'' AND lower(t.title)=lower(p.title))",
                "bool",
                false,
            ),
        );
        put(
            "description_duplicate",
            Column::new(
                "EXISTS (SELECT 1 FROM pages t WHERE t.url_id<>p.url_id AND trim(t.meta_description)<>'' AND lower(t.meta_description)=lower(p.meta_description))",
                "bool",
                false,
            ),
        );
        put(
            "has_hreflang",
            Column::new(
                "CASE WHEN p.hreflang_json IS NULL THEN NULL ELSE p.hreflang_json NOT IN ('[]','{}','') END",
                "bool",
                false,
            ),
        );
        put(
            "in_sitemap",
            Column::new(
                "EXISTS (SELECT 1 FROM context_items s WHERE s.kind='sitemap_declared_url' AND CAST(json_extract(s.payload_json,'$.url_id') AS INTEGER)=p.url_id)",
                "bool",
                false,
            ),
        );
        put("canonical_self", Column::new("(p.canonical<>'' AND p.canonical=u.url)", "bool", false));
        put(
            "robots_blocked",
            Column::new(
                "EXISTS (SELECT 1 FROM context_items c WHERE c.kind='robots_blocked_url' AND c.item_key='url:' || p.url_id)",
                "bool",
                false,
            ),
        );
        put(
            "canonical_non_indexable",
            Column::new(
                "EXISTS (SELECT 1 FROM pages t JOIN urls tu ON tu.url_id=t.url_id WHERE tu.url=p.canonical AND COALESCE(t.status_code BETWEEN 200 AND 299 AND instr(lower(t.meta_robots||','||t.x_robots),'noindex')=0, 0)=0)",
                "bool",
                false,
            ),
        );
        put("canonical_other", Column::new("(p.canonical<>'' AND p.canonical<>u.url)", "bool", false));
        put("resource_count", Column::new(&resource_count(""), "int", false));
        put("resource_images", Column::new(&resource_count(" AND r.kind='image'"), "int", false));
        put(
            "resource_scripts",
            Column::new(&resource_count(" AND r.kind IN ('script','module')"), "int", false),
        );
        put(
            "resource_stylesheets",
            Column::new(&resource_count(" AND r.kind='stylesheet'"), "int", false),
        );
        put(
            "resource_bytes",
            Column::new(
                "(SELECT CASE WHEN COUNT(*)=0 THEN 0 WHEN COALESCE(SUM(f.state='complete'),0)<COUNT(*) THEN NULL ELSE SUM(f.bytes_received) END FROM (SELECT DISTINCT resolved_url FROM resource_graph_occurrences WHERE page_url_id=p.url_id AND source_document_id=p.document_id) q LEFT JOIN resource_graph_fetches f USING(resolved_url))",
                "int",
                false,
            ),
        );
        put(
            "resource_bytes_state",
            Column::new(
                "(SELECT CASE WHEN COUNT(*)=0 THEN 'exact' WHEN COALESCE(SUM(f.state='complete'),0)=COUNT(*) THEN 'exact' WHEN COALESCE(SUM(f.state='complete'),0)=0 THEN 'unavailable' ELSE 'partial' END FROM (SELECT DISTINCT resolved_url FROM resource_graph_occurrences WHERE page_url_id=p.url_id AND source_document_id=p.document_id) q LEFT JOIN resource_graph_fetches f USING(resolved_url))",
                "text",
                false,
            ),
        );
        put(
            "indexable",
            Column::new(
                "COALESCE(p.status_code BETWEEN 200 AND 299 AND instr(lower(p.meta_robots||','||p.x_robots),'noindex')=0, 0)",
                "bool",
                false,
            ),
        );
        for (name, column) in transport_columns_build() {
            m.insert(name, column);
        }
        m
    })
}

/// SQL expression of the response whose transport phases a page row reports.
pub const TRANSPORT_RESPONSE_ID: &str = "COALESCE((SELECT d.source_response_id FROM documents d WHERE d.document_id=p.document_id),(SELECT MAX(r.response_id) FROM responses r WHERE r.request_url_id=p.url_id AND r.purpose='page'))";

/// Columns returned when the caller names none.
pub const DEFAULT_COLUMNS: &[&str] = &[
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

/// Sorted for a stable order.
pub const REQUIRED_PAGE_COLUMNS: &[&str] = &[
    "cache_status",
    "canonical",
    "charset",
    "content_encoding",
    "content_type",
    "crawl_depth",
    "error",
    "error_kind",
    "external_outlinks",
    "final_canonical",
    "final_url",
    "h1",
    "h1_2",
    "h2",
    "images_missing_alt_attr",
    "images_total",
    "jsonld_blocks_found",
    "meta_description",
    "meta_description_count",
    "meta_robots",
    "og_description",
    "og_image",
    "og_title",
    "outlinks",
    "page_ordinal",
    "redirect_url",
    "representation",
    "response_time",
    "size_bytes",
    "status_code",
    "text_ratio",
    "title",
    "url_id",
    "word_count",
    "x_robots",
];

/// Status class name to half-open status range.
pub const STATUS_CLASSES: &[(&str, &[i64])] = &[
    ("1xx", &[100, 200]),
    ("2xx", &[200, 300]),
    ("3xx", &[300, 400]),
    ("4xx", &[400, 500]),
    ("5xx", &[500, 600]),
];

/// Sorted for a stable order.
pub const RESOURCE_KINDS: &[&str] = &[
    "css_import",
    "css_url",
    "document",
    "font",
    "image",
    "manifest",
    "media",
    "module",
    "script",
    "stylesheet",
];

/// Return a fixed SQL expression for a scheme-and-authority URL prefix.
pub fn origin_expression(value: &str) -> String {
    let scheme = format!("instr({value},'://')");
    let remainder = format!("substr({value},{scheme}+3)");
    let slash = format!("instr({remainder},'/')");
    let query = format!("instr({remainder},'?')");
    let fragment = format!("instr({remainder},'#')");
    let authority_end = format!(
        "CASE WHEN {slash}>0 THEN {scheme}+1+{slash} \
         WHEN {query}>0 THEN {scheme}+1+{query} \
         WHEN {fragment}>0 THEN {scheme}+1+{fragment} \
         ELSE length({value}) END"
    );
    format!("lower(substr({value},1,{authority_end}))")
}

/// SQL expression of the scheme-and-authority prefix of the page URL.
pub const PAGE_ORIGIN: &str = "lower(substr(u.url,1,CASE WHEN instr(substr(u.url,instr(u.url,'://')+3),'/')>0 THEN instr(u.url,'://')+1+instr(substr(u.url,instr(u.url,'://')+3),'/') WHEN instr(substr(u.url,instr(u.url,'://')+3),'?')>0 THEN instr(u.url,'://')+1+instr(substr(u.url,instr(u.url,'://')+3),'?') WHEN instr(substr(u.url,instr(u.url,'://')+3),'#')>0 THEN instr(u.url,'://')+1+instr(substr(u.url,instr(u.url,'://')+3),'#') ELSE length(u.url) END))";

/// SQL expression of the scan start URL.
pub const START_URL: &str = "(SELECT start_url FROM scan WHERE singleton=1)";

/// SQL expression of the scheme-and-authority prefix of the scan start URL.
pub const START_ORIGIN: &str = "lower(substr((SELECT start_url FROM scan WHERE singleton=1),1,CASE WHEN instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'/')>0 THEN instr((SELECT start_url FROM scan WHERE singleton=1),'://')+1+instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'/') WHEN instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'?')>0 THEN instr((SELECT start_url FROM scan WHERE singleton=1),'://')+1+instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'?') WHEN instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'#')>0 THEN instr((SELECT start_url FROM scan WHERE singleton=1),'://')+1+instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'#') ELSE length((SELECT start_url FROM scan WHERE singleton=1)) END))";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_expression_matches_the_constants() {
        assert_eq!(origin_expression("u.url"), PAGE_ORIGIN);
        assert_eq!(origin_expression(START_URL), START_ORIGIN);
    }

    #[test]
    fn required_page_columns_are_the_plain_page_columns() {
        let mut plain: Vec<&str> = columns()
            .values()
            .filter_map(|c| c.expr.strip_prefix("p."))
            .filter(|rest| rest.chars().all(|ch| ch.is_alphanumeric() || ch == '_'))
            .collect();
        plain.sort_unstable();
        plain.dedup();
        assert_eq!(plain, REQUIRED_PAGE_COLUMNS);
    }

    #[test]
    fn transport_columns_are_part_of_the_allow_list() {
        assert_eq!(transport_columns().len(), 15);
        assert!(transport_columns()
            .keys()
            .all(|k| columns().contains_key(k)));
        assert!(is_issue_check("A11Y_IMAGE_ALT_MISSING"));
        assert!(!is_issue_check("-bad"));
    }
}
