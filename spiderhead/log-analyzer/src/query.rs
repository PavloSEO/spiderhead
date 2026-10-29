//! Query language for log views: text matching with explicit case handling, LIKE-safe
//! "contains", a three-state sort, and the bot verification-status filter.
//!
//! Text expressions: an optional `c:` prefix makes the match case-sensitive (the default is
//! case-insensitive), then an optional operator -- `=` equals, `^` starts with, `~` or
//! nothing contains. `c:^/Blog` matches paths starting with exactly `/Blog`.

use std::collections::HashMap;

use rusqlite::functions::FunctionFlags;
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

use crate::error::{LogsError, Result};
use crate::ranges::{bot_statuses, RangeBook, VerificationStatus};
use crate::store::{open_log_source, query_rows};
use crate::summary::{paged, where_clause, Filter, VIEWS};

/// How a text value is compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextOp {
    /// Whole value equals the text.
    Equals,
    /// Value starts with the text.
    Prefix,
    /// Value contains the text.
    Contains,
}

/// One text condition on a column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMatch {
    pub op: TextOp,
    pub value: String,
    /// Exact-case comparison; off by default.
    pub case_sensitive: bool,
}

impl TextMatch {
    /// Parse a query expression (see the module docs); an empty text is an error.
    pub fn parse(expr: &str) -> Result<Self> {
        let (case_sensitive, rest) = match expr.strip_prefix("c:") {
            Some(rest) => (true, rest),
            None => (false, expr),
        };
        let (op, value) = match rest.chars().next() {
            Some('=') => (TextOp::Equals, &rest[1..]),
            Some('^') => (TextOp::Prefix, &rest[1..]),
            Some('~') => (TextOp::Contains, &rest[1..]),
            _ => (TextOp::Contains, rest),
        };
        if value.is_empty() {
            return Err(LogsError::value("text condition must not be empty"));
        }
        Ok(Self {
            op,
            value: value.to_string(),
            case_sensitive,
        })
    }

    /// SQL condition on `column` (which must be a trusted identifier) and its parameters.
    /// Needs [`register_fold`] on the connection for case-insensitive matches.
    pub fn clause(&self, column: &str) -> (String, Vec<SqlValue>) {
        let text = |s: String| SqlValue::Text(s);
        if self.case_sensitive {
            return match self.op {
                TextOp::Equals => (format!("{column}=?"), vec![text(self.value.clone())]),
                TextOp::Prefix => (
                    format!("substr({column},1,length(?))=?"),
                    vec![text(self.value.clone()), text(self.value.clone())],
                ),
                TextOp::Contains => (
                    format!("instr({column},?)>0"),
                    vec![text(self.value.clone())],
                ),
            };
        }
        let folded = self.value.to_lowercase();
        match self.op {
            TextOp::Equals => (format!("fold({column})=?"), vec![text(folded)]),
            TextOp::Prefix => (
                format!("fold({column}) LIKE ? ESCAPE '\\'"),
                vec![text(format!("{}%", like_escape(&folded)))],
            ),
            TextOp::Contains => (
                format!("fold({column}) LIKE ? ESCAPE '\\'"),
                vec![text(format!("%{}%", like_escape(&folded)))],
            ),
        }
    }
}

/// Escape `%`, `_` and the escape character so user text matches literally in LIKE.
pub fn like_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Register the Unicode-aware `fold` function used by case-insensitive matches.
pub fn register_fold(con: &Connection) -> Result<()> {
    con.create_scalar_function(
        "fold",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(ctx.get::<String>(0)?.to_lowercase()),
    )?;
    Ok(())
}

/// Click-cycle sort state of a column header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortState {
    /// The view's natural order.
    None,
    Asc,
    Desc,
}

impl SortState {
    /// State after one header click: none, ascending, descending, none.
    pub fn next(self) -> Self {
        match self {
            Self::None => Self::Asc,
            Self::Asc => Self::Desc,
            Self::Desc => Self::None,
        }
    }
}

/// Sort request: a column and its state.
#[derive(Debug, Clone)]
pub struct Sort {
    pub column: String,
    pub state: SortState,
}

/// Request for [`query_view`].
#[derive(Debug, Clone)]
pub struct ViewQuery {
    /// One of the summary views (`urls`, `bots`, `daily`, ...).
    pub view: String,
    pub filter: Filter,
    /// Condition on the URL, applied before grouping.
    pub url: Option<TextMatch>,
    /// Keep only bot rows with this verification status; human and unknown rows are excluded.
    pub status: Option<VerificationStatus>,
    pub sort: Option<Sort>,
    pub limit: i64,
    pub offset: i64,
}

impl Default for ViewQuery {
    fn default() -> Self {
        Self {
            view: "urls".into(),
            filter: Filter::default(),
            url: None,
            status: None,
            sort: None,
            limit: 100,
            offset: 0,
        }
    }
}

/// Everything needed to run a view: connection with helper functions, SQL and parameters.
pub(crate) struct Prepared {
    pub con: Connection,
    pub sql: String,
    pub params: Vec<SqlValue>,
    pub columns: Vec<String>,
}

pub(crate) fn prepare(source: &str, q: &ViewQuery, book: &RangeBook, now: f64) -> Result<Prepared> {
    let group = VIEWS
        .iter()
        .find(|(name, _)| *name == q.view)
        .map(|(_, g)| *g)
        .ok_or_else(|| LogsError::value("unsupported log summary view"))?;
    let con = open_log_source(source)?;
    register_fold(&con)?;
    let (mut clause, mut params) = where_clause(&con, &q.filter)?;
    if q.view == "bot_ips" {
        clause.push_str(" AND family NOT IN ('human','unknown')");
    }
    if let Some(url) = &q.url {
        let (c, p) = url.clause("url");
        clause = format!("({clause}) AND {c}");
        params.extend(p);
    }
    if let Some(wanted) = q.status {
        let map: HashMap<(String, String), VerificationStatus> = bot_statuses(&con, book, now)?
            .into_iter()
            .map(|b| ((b.bot, b.ip), b.status))
            .collect();
        con.create_scalar_function("verif", 2, FunctionFlags::SQLITE_UTF8, move |ctx| {
            let (bot, ip): (String, String) = (ctx.get(0)?, ctx.get(1)?);
            Ok(map.get(&(bot, ip)).map(|s| s.code() as i64).unwrap_or(0))
        })?;
        clause = format!(
            "({clause}) AND family NOT IN ('human','unknown') AND verif(bot,ip)={}",
            wanted.code()
        );
    }
    let mut columns: Vec<String> = group.split(',').map(str::to_string).collect();
    columns.extend(["visits", "bytes", "last_visit"].map(String::from));
    let natural = format!("visits DESC,{group}");
    let order = match &q.sort {
        Some(s) if s.state != SortState::None => {
            if !columns.contains(&s.column) {
                return Err(LogsError::value(format!(
                    "cannot sort {} by {}",
                    q.view, s.column
                )));
            }
            let dir = if s.state == SortState::Asc {
                "ASC"
            } else {
                "DESC"
            };
            format!("{} {dir},{natural}", s.column)
        }
        _ => natural,
    };
    let sql = format!(
        "SELECT {group},SUM(visits) visits,SUM(bytes) bytes,MAX(last_visit) last_visit FROM visits WHERE {clause} GROUP BY {group} ORDER BY {order}"
    );
    Ok(Prepared {
        con,
        sql,
        params,
        columns,
    })
}

/// One filtered, sorted page of a view, with exact totals.
pub fn query_view(source: &str, q: &ViewQuery, book: &RangeBook, now: f64) -> Result<Value> {
    let p = prepare(source, q, book, now)?;
    let page = paged(&p.con, &p.sql, &p.params, q.limit, q.offset)?;
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("view".into(), json!(q.view));
    out.extend(page);
    Ok(Value::Object(out))
}

/// Column names and every row of a view.
pub(crate) type AllRows = (Vec<String>, Vec<Map<String, Value>>);

/// Every row of a view (no paging), for exports.
pub(crate) fn all_rows(source: &str, q: &ViewQuery, book: &RangeBook, now: f64) -> Result<AllRows> {
    let p = prepare(source, q, book, now)?;
    let rows = query_rows(&p.con, &p.sql, &p.params)?;
    Ok((p.columns, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions_parse_operator_and_case() {
        let m = TextMatch::parse("c:^/Blog").unwrap();
        assert_eq!(
            (m.op, m.case_sensitive, m.value.as_str()),
            (TextOp::Prefix, true, "/Blog")
        );
        let m = TextMatch::parse("=/a").unwrap();
        assert_eq!((m.op, m.case_sensitive), (TextOp::Equals, false));
        assert_eq!(TextMatch::parse("shop").unwrap().op, TextOp::Contains);
        assert!(TextMatch::parse("c:").is_err());
    }

    #[test]
    fn like_wildcards_match_literally() {
        assert_eq!(like_escape(r"50%_off\x"), r"50\%\_off\\x");
        let con = Connection::open_in_memory().unwrap();
        register_fold(&con).unwrap();
        con.execute_batch("CREATE TABLE t(url TEXT); INSERT INTO t VALUES('/a_b'),('/axb'),('/50%'),('/500'),('/Ünï');").unwrap();
        let count = |expr: &str| {
            let (c, p) = TextMatch::parse(expr).unwrap().clause("url");
            con.query_row(
                &format!("SELECT count(*) FROM t WHERE {c}"),
                rusqlite::params_from_iter(p),
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
        };
        assert_eq!(count("a_b"), 1);
        assert_eq!(count("50%"), 1);
        assert_eq!(count("ÜN"), 1);
        assert_eq!(count("c:ÜN"), 0);
        assert_eq!(count("c:^/Ü"), 1);
        assert_eq!(count("^/A"), 2);
        assert_eq!(count("c:^/A"), 0);
    }

    #[test]
    fn sort_cycles_through_three_states() {
        let s = SortState::None;
        assert_eq!(s.next(), SortState::Asc);
        assert_eq!(s.next().next(), SortState::Desc);
        assert_eq!(s.next().next().next(), SortState::None);
    }
}
