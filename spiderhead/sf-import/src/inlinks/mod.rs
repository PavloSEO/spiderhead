//! The inlink layer: every status, anchor, hreflang, scoring, resource, and
//! graph check over the inlink evidence, plus `run_inlinks`, the fixed order
//! the audit passes them in.

mod anchors;
mod graph;
mod hreflang;
mod resources;
mod scores;
pub(crate) mod shared;

pub use anchors::{check_anchor_text, norm_anchor, GENERIC_ANCHORS};
pub use graph::{
    check_internal_link_graph, duplicate_groups_from_records, open_click_depth_session,
};
pub use hreflang::{
    check_hreflang_confirmation_consistency, check_hreflang_noindex_targets,
    check_hreflang_quality, check_hreflang_reciprocity, check_hreflang_targets,
    check_hreflang_unlinked_targets,
};
pub use resources::{check_insecure_subresources, check_pagination_declarations};
pub use scores::{check_discovery_path, check_inlink_composition, check_link_score};
pub use shared::{
    all_inlink_records, internal_hyperlink_edges, internal_hyperlink_records, process_export,
    site_host, INLINK_SOURCES, MAX_PATH_URLS,
};

use crate::context::{AuditContext, ContextError};

/// `run_inlinks`: the inlink checks in the order the audit runs them.
pub fn run_inlinks(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let site_host = site_host(ctx);
    for (key, internal_check, external_check) in INLINK_SOURCES {
        process_export(ctx, key, internal_check, external_check, &site_host)?;
    }
    check_anchor_text(ctx)?;
    check_hreflang_targets(ctx)?;
    check_hreflang_unlinked_targets(ctx)?;
    check_hreflang_noindex_targets(ctx)?;
    check_hreflang_quality(ctx)?;
    check_hreflang_reciprocity(ctx)?;
    check_hreflang_confirmation_consistency(ctx)?;
    check_link_score(ctx)?;
    check_inlink_composition(ctx)?;
    check_discovery_path(ctx)?;
    check_insecure_subresources(ctx)?;
    check_pagination_declarations(ctx)?;
    // Last: it opens its own path session over the stored graph, and opening
    // one closes whichever session check_discovery_path above is still
    // holding.
    check_internal_link_graph(ctx)
}
