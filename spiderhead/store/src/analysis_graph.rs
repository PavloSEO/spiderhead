//! SQL graph projections with injected normalization and check-independent facts.
//!

use crate::analysis_paths::PathSession;
use crate::analysis_score::{compute_scores, ScoreView};
use crate::error::StoreError;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{Map, Value};
use spiderhead_core::graph::{AnchorGroup, DuplicateLinkGroup, InlinkCompositionRow};
use std::sync::atomic::{AtomicU64, Ordering};

/// Process-wide counter that gives each graph a unique TEMP table prefix.
static NEXT_GRAPH_ID: AtomicU64 = AtomicU64::new(1);

/// Static and rendered link occurrences combined as a reusable SQL CTE.
pub fn selected_links_cte(name: &str) -> String {
    format!(
        "WITH {name} AS ( \
         SELECT l.* FROM links l WHERE l.evidence_representation IN ('static','legacy_unknown') \
         UNION ALL \
         SELECT r.* FROM links r JOIN pages p ON p.url_id=r.source_url_id \
         WHERE r.evidence_representation=p.representation \
         AND p.representation IN ('rendered','legacy_fragment') \
         AND NOT EXISTS (SELECT 1 FROM links s WHERE s.source_url_id=r.source_url_id \
                         AND s.evidence_representation IN ('static','legacy_unknown') AND s.destination_url_id=r.destination_url_id) \
         AND NOT EXISTS (SELECT 1 FROM links later WHERE later.source_url_id=r.source_url_id \
                         AND later.evidence_representation=r.evidence_representation \
                         AND later.destination_url_id=r.destination_url_id \
                         AND (later.ordinal>r.ordinal OR (later.ordinal=r.ordinal AND later.link_id>r.link_id))) \
         ) "
    )
}

/// Strip the fragment (the part after `#`) from a URL.
fn defrag(url: &str) -> &str {
    url.split_once('#').map_or(url, |(head, _)| head)
}

/// Network location of a URL; empty when there is no `//` authority.
fn netloc(url: &str) -> &str {
    let rest = match url.split_once(':') {
        Some((scheme, rest))
            if !scheme.is_empty()
                && scheme
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) =>
        {
            rest
        }
        _ => url,
    };
    match rest.strip_prefix("//") {
        Some(after) => {
            let end = after.find(['/', '?', '#']).unwrap_or(after.len());
            &after[..end]
        }
        None => "",
    }
}

/// Keep raw occurrences, calculation nodes and topology in TEMP SQLite.
///
/// Call [`AnalysisGraph::close`] when done; TEMP tables are dropped there.
pub struct AnalysisGraph<'a> {
    con: &'a Connection,
    normalize: Box<dyn Fn(&str) -> String + 'a>,
    site_host: String,
    prefix: String,
    nodes: String,
    edges: String,
    topology: String,
    tables: Vec<String>,
    ready: bool,
    closed: bool,
    scores_ready: bool,
    scores: Option<ScoreView<'a>>,
    paths: Option<PathSession<'a>>,
    prior_query_only: bool,
}

impl<'a> AnalysisGraph<'a> {
    /// Whether resource-type rows are stored (never, in this projection).
    pub const HAS_RESOURCE_TYPE: bool = false;

    /// Start a graph over `con`. Fails unless `temp_store` is file-backed (value 1).
    pub fn new(
        con: &'a Connection,
        normalize: impl Fn(&str) -> String + 'a,
        site_host: &str,
    ) -> Result<Self, StoreError> {
        let id = NEXT_GRAPH_ID.fetch_add(1, Ordering::Relaxed);
        let prefix = format!("f_graph_{id:x}");
        let temp_store: i64 = con.query_row("PRAGMA temp_store", [], |r| r.get(0))?;
        if temp_store != 1 {
            return Err(StoreError::Invalid(
                "stored graph requires file-backed temporary storage".into(),
            ));
        }
        let prior_query_only: i64 = con.query_row("PRAGMA query_only", [], |r| r.get(0))?;
        if prior_query_only != 0 {
            con.execute_batch("PRAGMA query_only=OFF")?;
        }
        Ok(Self {
            con,
            normalize: Box::new(normalize),
            site_host: site_host.to_lowercase(),
            nodes: format!("{prefix}_nodes"),
            edges: format!("{prefix}_edges"),
            topology: format!("{prefix}_topology"),
            prefix,
            tables: Vec::new(),
            ready: false,
            closed: false,
            scores_ready: false,
            scores: None,
            paths: None,
            prior_query_only: prior_query_only != 0,
        })
    }

    /// Create a TEMP table `<prefix><suffix>` and remember it for cleanup.
    pub fn table(&mut self, suffix: &str, columns: &str) -> Result<String, StoreError> {
        let name = format!("{}{suffix}", self.prefix);
        self.con
            .execute_batch(&format!("CREATE TEMP TABLE {name} ({columns})"))?;
        self.tables.push(name.clone());
        Ok(name)
    }

    /// Drop the session paths, scores and TEMP tables, then restore `query_only`. Idempotent.
    pub fn close(&mut self) -> Result<(), StoreError> {
        if self.closed {
            return Ok(());
        }
        let mut result = Ok(());
        if let Some(paths) = self.paths.as_mut() {
            result = result.and(paths.close());
        }
        if let Some(scores) = self.scores.as_ref() {
            result = result.and(scores.close());
        }
        for name in self.tables.iter().rev() {
            let r = self
                .con
                .execute_batch(&format!("DROP TABLE IF EXISTS temp.{name}"));
            result = result.and(r.map_err(StoreError::from));
        }
        let restore = if self.prior_query_only {
            "PRAGMA query_only=ON"
        } else {
            "PRAGMA query_only=OFF"
        };
        result = result.and(self.con.execute_batch(restore).map_err(StoreError::from));
        self.closed = true;
        result
    }

    /// Materialize nodes, edges and topology once. Later calls are no-ops.
    pub fn prepare(&mut self) -> Result<(), StoreError> {
        if self.closed {
            return Err(StoreError::Invalid("stored graph is closed".into()));
        }
        if self.ready {
            return Ok(());
        }
        self.table("_nodes", "node TEXT PRIMARY KEY")?;
        self.table(
            "_edges",
            "seq INTEGER PRIMARY KEY, raw_src TEXT NOT NULL,raw_dst TEXT NOT NULL,\
             src_key TEXT NOT NULL,dst_key TEXT NOT NULL,comp_key TEXT NOT NULL,\
             anchor TEXT NOT NULL,position TEXT NOT NULL,nofollow INTEGER NOT NULL,\
             internal INTEGER NOT NULL",
        )?;
        self.table(
            "_topology",
            "seq INTEGER PRIMARY KEY,src_key TEXT NOT NULL,dst_key TEXT NOT NULL",
        )?;

        let raw_pages: Vec<String> = {
            let mut stmt = self.con.prepare(
                "SELECT u.url FROM pages p JOIN urls u USING(url_id) ORDER BY p.page_ordinal",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for raw in raw_pages {
            let key = (self.normalize)(raw.trim());
            self.con.execute(
                &format!("INSERT OR IGNORE INTO {} VALUES(?1)", self.nodes),
                params![key],
            )?;
        }

        type EdgeRow = (String, String, String, String, i64);
        let edge_rows: Vec<EdgeRow> = {
            let sql = selected_links_cte("selected")
                + "SELECT src.url,dst.url,selected.anchor,selected.position,selected.nofollow \
                   FROM selected JOIN pages p ON p.url_id=selected.source_url_id \
                   JOIN urls src ON src.url_id=selected.source_url_id \
                   JOIN urls dst ON dst.url_id=selected.destination_url_id \
                   ORDER BY CASE WHEN selected.evidence_representation IN ('static','legacy_unknown') THEN 0 ELSE 1 END,\
                   p.page_ordinal,selected.ordinal,selected.link_id";
            let mut stmt = self.con.prepare(&sql)?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?;
            rows.collect::<Result<_, _>>()?
        };
        for (seq, (source, destination, anchor, position, nofollow)) in
            edge_rows.into_iter().enumerate()
        {
            // Blank values are stored as empty text.
            // They are projected to None at the read boundary.
            let source = source.trim().to_owned();
            let destination = destination.trim().to_owned();
            let anchor = anchor.trim().to_owned();
            let position = position.trim().to_owned();
            let src = (self.normalize)(&source);
            let dst = (self.normalize)(&destination);
            let host = netloc(&destination).to_lowercase();
            let internal = !source.is_empty()
                && !destination.is_empty()
                && (host.is_empty() || host == self.site_host);
            let comp_key = defrag(&dst).to_owned();
            self.con.execute(
                &format!(
                    "INSERT INTO {} VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    self.edges
                ),
                params![
                    seq as i64,
                    source,
                    destination,
                    src,
                    dst,
                    comp_key,
                    anchor,
                    position,
                    nofollow,
                    internal as i64
                ],
            )?;
            if internal && nofollow == 0 {
                self.con.execute(
                    &format!("INSERT INTO {} VALUES(?1,?2,?3)", self.topology),
                    params![seq as i64, src, dst],
                )?;
                self.con.execute(
                    &format!("INSERT OR IGNORE INTO {} VALUES(?1)", self.nodes),
                    params![src],
                )?;
                self.con.execute(
                    &format!("INSERT OR IGNORE INTO {} VALUES(?1)", self.nodes),
                    params![dst],
                )?;
            }
        }
        self.con.execute_batch(&format!(
            "CREATE INDEX {}_composition_edges ON {}(comp_key,internal,seq)",
            self.prefix, self.edges
        ))?;
        self.ready = true;
        Ok(())
    }

    /// Repeatable normalized observations for fact-only attribution checks.
    pub fn iter_evidence_links(&mut self) -> Result<Vec<Map<String, Value>>, StoreError> {
        self.prepare()?;
        let mut stmt = self.con.prepare(&format!(
            "SELECT raw_src,raw_dst,anchor,position FROM {} ORDER BY seq",
            self.edges
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (source, destination, anchor, position) = row?;
            let mut m = Map::new();
            m.insert("source_url".into(), Value::String(source));
            m.insert("destination_url".into(), Value::String(destination));
            m.insert("anchor".into(), opt_text(anchor));
            m.insert("link_position".into(), opt_text(position));
            out.push(m);
        }
        Ok(out)
    }

    /// Anchor texts that repeat a generic phrase, grouped per source page in first-seen order.
    pub fn iter_anchor_groups(
        &mut self,
        is_generic_anchor: impl Fn(&str) -> bool,
        max_locations: i64,
    ) -> Result<Vec<AnchorGroup>, StoreError> {
        self.prepare()?;
        let seen = self.table(
            "_anchor_seen",
            "src TEXT,dst TEXT,anchor_key TEXT,seq INTEGER NOT NULL,PRIMARY KEY(src,dst,anchor_key)",
        )?;
        let occurrences: Vec<(i64, String, String, String)> = {
            let mut stmt = self.con.prepare(&format!(
                "SELECT seq,raw_src,raw_dst,anchor FROM {} ORDER BY seq",
                self.edges
            ))?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            rows.collect::<Result<_, _>>()?
        };
        for (seq, source, destination, anchor) in occurrences {
            if !source.is_empty() && !anchor.is_empty() && is_generic_anchor(&anchor) {
                self.con.execute(
                    &format!("INSERT OR IGNORE INTO {seen} VALUES(?1,?2,?3,?4)"),
                    params![source, destination, anchor.to_lowercase(), seq],
                )?;
            }
        }
        let sources: Vec<(String, i64)> = {
            let mut stmt = self.con.prepare(&format!(
                "SELECT src,COUNT(*),MIN(seq) first FROM {seen} GROUP BY src ORDER BY first"
            ))?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut groups = Vec::new();
        for (source, count) in sources {
            let mut generic_links = Vec::new();
            let mut locations = Vec::new();
            let rows: Vec<(String, String, String, i64)> = {
                let mut stmt = self.con.prepare(&format!(
                    "SELECT e.raw_dst,e.anchor,e.position,e.nofollow FROM {seen} s \
                     JOIN {edges} e USING(seq) WHERE s.src=?1 ORDER BY e.seq LIMIT ?2",
                    edges = self.edges
                ))?;
                let mapped = stmt.query_map(params![source, max_locations], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                })?;
                mapped.collect::<Result<_, _>>()?
            };
            for (destination, anchor, position, nofollow) in rows {
                let mut g = Map::new();
                g.insert("anchor".into(), Value::String(anchor.clone()));
                g.insert("destination".into(), opt_text(destination));
                g.insert("link_position".into(), opt_text(position.clone()));
                generic_links.push(g);

                let mut l = Map::new();
                l.insert("source_url".into(), Value::String(source.clone()));
                l.insert("anchor".into(), Value::String(anchor));
                l.insert("alt_text".into(), Value::Null);
                l.insert("link_position".into(), opt_text(position));
                l.insert("link_path".into(), Value::Null);
                l.insert("follow".into(), Value::Bool(nofollow == 0));
                l.insert("rel".into(), Value::Null);
                l.insert("target".into(), Value::Null);
                locations.push(l);
            }
            groups.push(AnchorGroup {
                source_url: source,
                occurrences_count: count as u64,
                generic_links,
                locations,
            });
        }
        Ok(groups)
    }

    /// Whether any followed internal link exists.
    pub fn has_internal_hyperlinks(&mut self) -> Result<bool, StoreError> {
        self.prepare()?;
        let row: Option<i64> = self
            .con
            .query_row(
                &format!("SELECT 1 FROM {} LIMIT 1", self.topology),
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(row.is_some())
    }

    /// PageRank over the followed internal topology. Computed once; later calls reuse it.
    pub fn link_score(
        &mut self,
        damping: f64,
        max_iterations: i64,
        tolerance: f64,
    ) -> Result<Option<&ScoreView<'a>>, StoreError> {
        self.prepare()?;
        if !self.scores_ready {
            self.scores = compute_scores(
                self.con,
                &self.nodes,
                &self.topology,
                &self.prefix,
                damping,
                max_iterations,
                tolerance,
            )?;
            self.scores_ready = true;
        }
        Ok(self.scores.as_ref())
    }

    /// Inlink composition per destination (without fragment), in first-seen order.
    pub fn iter_inlink_composition(
        &mut self,
        is_indexable_source: impl Fn(&str) -> Option<bool>,
        max_source_examples: i64,
    ) -> Result<Vec<InlinkCompositionRow>, StoreError> {
        self.prepare()?;
        let sources = self.table(
            "_source_facts",
            "raw_src TEXT PRIMARY KEY,indexable INTEGER",
        )?;
        let distinct: Vec<String> = {
            let mut stmt = self.con.prepare(&format!(
                "SELECT DISTINCT raw_src FROM {} WHERE internal=1",
                self.edges
            ))?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for source in distinct {
            let measured = is_indexable_source(&source);
            self.con.execute(
                &format!("INSERT INTO {sources} VALUES(?1,?2)"),
                params![source, measured.map(|b| b as i64)],
            )?;
        }
        let groups: Vec<(String, i64, i64, i64, i64)> = {
            let mut stmt = self.con.prepare(&format!(
                "SELECT e.comp_key,COUNT(*),MIN(e.seq) first,MAX(e.nofollow=0),\
                 MAX(s.indexable IS NOT NULL),MAX(s.indexable=1) FROM {edges} e \
                 JOIN {sources} s USING(raw_src) WHERE e.internal=1 GROUP BY e.comp_key ORDER BY first",
                edges = self.edges
            ))?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                ))
            })?;
            rows.map(|row| {
                row.map(|(d, c, _first, followed, known, indexable)| {
                    (d, c, followed, known, indexable)
                })
            })
            .collect::<Result<_, _>>()?
        };
        let mut out = Vec::new();
        for (destination, count, followed, known, indexable) in groups {
            let mut stmt = self.con.prepare(&format!(
                "SELECT DISTINCT raw_src FROM {} WHERE comp_key=?1 AND internal=1 \
                 ORDER BY raw_src COLLATE BINARY LIMIT ?2",
                self.edges
            ))?;
            let examples: Vec<String> = stmt
                .query_map(params![destination, max_source_examples], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<Result<_, _>>()?;
            out.push(InlinkCompositionRow {
                destination_key: destination,
                occurrences_count: count as u64,
                all_nofollow: followed == 0,
                has_known_source: known != 0,
                has_indexable_source: indexable != 0,
                source_examples: examples,
            });
        }
        Ok(out)
    }

    /// Open (or reopen) the shortest-path session from `seed`. Closes any previous session.
    pub fn begin_paths(&mut self, seed: &str) -> Result<Option<&PathSession<'a>>, StoreError> {
        self.prepare()?;
        if let Some(previous) = self.paths.as_mut() {
            previous.close()?;
        }
        self.paths = PathSession::open(self.con, &self.prefix, seed)?;
        Ok(self.paths.as_ref())
    }

    /// Internal edges by the position recorded on them, blanks kept apart.
    ///
    /// The empty string is returned under its own `""` key rather than merged into any bucket:
    /// a crawl run without position classification stores it on every edge, and folding that
    /// into `content` would turn "nobody looked" into "this is body copy" for the whole site.
    pub fn position_totals(&mut self) -> Result<Map<String, Value>, StoreError> {
        self.prepare()?;
        let mut stmt = self.con.prepare(&format!(
            "SELECT position, COUNT(*) FROM {} WHERE internal=1 GROUP BY position",
            self.edges
        ))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = Map::new();
        for row in rows {
            let (position, count) = row?;
            out.insert(position, Value::from(count));
        }
        Ok(out)
    }

    /// Source pages that emit the same (destination, anchor) more than once.
    ///
    /// Grouped on the raw source and destination spellings, not the normalized keys: this is a
    /// statement about the markup one page emits, and two hrefs that differ only in a trailing
    /// slash are two links written on the page even where they resolve to one destination.
    pub fn iter_duplicate_links(
        &mut self,
        max_repeats: i64,
    ) -> Result<Vec<DuplicateLinkGroup>, StoreError> {
        self.prepare()?;
        let sources: Vec<(String, i64)> = {
            let mut stmt = self.con.prepare(&format!(
                "SELECT raw_src, SUM(n) - COUNT(*) AS surplus FROM (\
                 SELECT raw_src, raw_dst, anchor, COUNT(*) AS n FROM {edges} \
                 WHERE internal=1 GROUP BY raw_src, raw_dst, anchor HAVING n > 1\
                 ) GROUP BY raw_src ORDER BY raw_src",
                edges = self.edges
            ))?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut out = Vec::new();
        for (source, surplus) in sources {
            let mut stmt = self.con.prepare(&format!(
                "SELECT raw_dst, anchor, COUNT(*) AS n FROM {} \
                 WHERE internal=1 AND raw_src=?1 GROUP BY raw_dst, anchor HAVING n > 1 \
                 ORDER BY n DESC, raw_dst LIMIT ?2",
                self.edges
            ))?;
            let mut repeats = Vec::new();
            let rows = stmt.query_map(params![source, max_repeats], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (destination, anchor, count) = row?;
                let mut m = Map::new();
                m.insert("destination".into(), Value::String(destination));
                m.insert("anchor".into(), Value::String(anchor));
                m.insert("count".into(), Value::from(count));
                repeats.push(m);
            }
            out.push(DuplicateLinkGroup {
                source_url: source,
                surplus_total: surplus as u64,
                repeats,
            });
        }
        Ok(out)
    }

    /// Resource rows are not stored by this projection; always empty.
    pub fn iter_resources(&self) -> Vec<(String, String, String)> {
        Vec::new()
    }
}

/// Map an empty stored text to JSON null.
fn opt_text(text: String) -> Value {
    if text.is_empty() {
        Value::Null
    } else {
        Value::String(text)
    }
}

impl Drop for AnalysisGraph<'_> {
    fn drop(&mut self) {
        // Best-effort cleanup; callers that need the error call `close` explicitly.
        let _ = self.close();
    }
}
