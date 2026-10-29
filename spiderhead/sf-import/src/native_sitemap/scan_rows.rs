//! The SQL rows the retained sitemap/robots checks read, collected while
//! `scan_con` is borrowed — `ctx.add` needs `&mut ctx`, so queries cannot
//! interleave with findings.

use rusqlite::OptionalExtension;
use spiderhead_store::analysis_graph::selected_links_cte;

use crate::context::disk::DiskError;
use crate::context::{AuditContext, ContextError};

/// Every row `run_retained_sitemap` + `run_robots` read.
pub(crate) struct ScanRows {
    /// `links` joins selected with urls/pages/robots_blocked_url payloads.
    pub(crate) links: Vec<(String, String, Option<i64>, bool)>,
    /// `resource_refs` rows for script/stylesheet dependencies.
    pub(crate) resources: Vec<(String, String, String, String, Option<String>)>,
    /// Any `resource_refs` row exists at all.
    pub(crate) has_resource_refs: bool,
    /// A complete `resource_inventory` context item exists.
    pub(crate) has_complete_inventory: bool,
}

pub(crate) fn gather_scan_rows(ctx: &AuditContext) -> Result<ScanRows, ContextError> {
    let links_sql = format!(
        "{}SELECT src.url AS source,dst.url AS target,p.status_code,c.payload_json \
         FROM selected l JOIN urls src ON src.url_id=l.source_url_id \
         JOIN urls dst ON dst.url_id=l.destination_url_id \
         LEFT JOIN pages p ON p.url_id=l.destination_url_id \
         LEFT JOIN context_items c ON c.kind='robots_blocked_url' AND c.item_key='url:'||dst.url_id \
         ORDER BY l.source_url_id,l.ordinal,l.link_id",
        selected_links_cte("selected")
    );
    let resources_sql =
        "SELECT src.url AS source,dst.url AS target,r.kind,r.capture_state,r.reason \
         FROM resource_refs r JOIN pages p ON p.url_id=r.page_url_id \
         JOIN urls src ON src.url_id=r.page_url_id JOIN urls dst ON dst.url_id=r.resource_url_id \
         WHERE r.representation=p.representation AND r.kind IN ('script','stylesheet') \
         ORDER BY r.page_url_id,r.ordinal";
    let mut rows = ScanRows {
        links: Vec::new(),
        resources: Vec::new(),
        has_resource_refs: false,
        has_complete_inventory: false,
    };
    if let Some(con) = ctx.scan_con.as_ref() {
        rows.links = {
            let mut stmt = con.prepare(&links_sql).map_err(DiskError::from)?;
            let collected = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<String>>(3)?.is_some(),
                    ))
                })
                .map_err(DiskError::from)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(DiskError::from)?;
            collected
        };
        rows.resources = {
            let mut stmt = con.prepare(resources_sql).map_err(DiskError::from)?;
            let collected = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                })
                .map_err(DiskError::from)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(DiskError::from)?;
            collected
        };
        rows.has_resource_refs = con
            .query_row("SELECT 1 FROM resource_refs LIMIT 1", [], |_| Ok(()))
            .optional()
            .map_err(DiskError::from)?
            .is_some();
        rows.has_complete_inventory = con
            .query_row(
                "SELECT 1 FROM context_items WHERE kind='resource_inventory' AND completeness='complete' LIMIT 1",
                [],
                |_| Ok(()),
            )
            .optional()
            .map_err(DiskError::from)?
            .is_some();
    }
    Ok(rows)
}
