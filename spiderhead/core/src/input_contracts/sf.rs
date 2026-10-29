// Catalogue data only.

//! SF family (`sf ...`) and `mcp` command contracts.

use super::{command, form, CommandContract, InputKind};

/// Input contracts for the `sf` family and the `mcp` command.
pub(super) const COMMANDS: &[CommandContract] = &[
    command("sf run", None, &[
        form(InputKind::LiveUrl, &["crawl"], ""),
        form(InputKind::LocalFile, &["load_crawl"], "Saved .seospider crawl; requires licensed SF CLI"),
        form(InputKind::LocalFile, &["crawl_list"], "URL-list file for licensed SF live traversal"),
        form(InputKind::LegacyDirectory, &["exports_dir"], ""),
        form(InputKind::LocalConfig, &["config"], "SF audit configuration; model-based semantic_similarity requires an explicit adapter declaration. The offline lexical adapter (analysis.semantic) is on by default and never loads a model."),
        form(InputKind::LocalFile, &["auth_config"], "Optional SF authentication profile."),
        form(InputKind::InlineText, &["auth"], "Optional HTTP Basic credentials; do not persist or log them."),
        form(InputKind::LocalFile, &["sf_cli"], "Optional explicit licensed SF CLI path."),
        form(InputKind::LiveUrl, &["sitemap"], "Optional explicit sitemap URL for live rechecks."),
        form(InputKind::LocalDirectory, &["out"], "Local audit and optional task output directory."),
    ], ""),
    command("sf tasks", None, &[
        form(InputKind::AuditDocument, &["audit_json"], "Audit JSON or a retained scan.v1 artifact."),
        form(InputKind::LocalConfig, &["config"], ""),
        form(InputKind::LocalDirectory, &["out"], "Local task output directory."),
    ], ""),
    command("sf doctor", None, &[
        form(InputKind::LocalConfig, &["config"], ""),
        form(InputKind::LocalFile, &["sf_cli"], "Optional explicit licensed SF CLI path."),
    ], ""),
    command("sf save-config", None, &[
        form(InputKind::LocalFile, &["out"], ""),
    ], ""),
    command("sf import", None, &[
        form(InputKind::LocalDirectory, &["bundle"], "Saved SF crawl bundle: .seospider directory or archive."),
        form(InputKind::NoInput, &["inspect"], "Inventory tables and row counts; no conversion."),
        form(InputKind::LocalFile, &["derby_jar"], "Optional explicit Derby JDBC jar path."),
    ], ""),
    command("sf snippet-edits", None, &[
        form(InputKind::LocalFile, &["csv"], "Supplied CSV; requires url and title/description columns."),
        form(InputKind::LocalFile, &["out"], "New JSON review-task destination; refuses overwrite."),
        form(InputKind::LocalFile, &["metrics_json"], "Optional named local font profiles for width measurement."),
        form(InputKind::ScanArtifact, &["scan"], "Optional retained context for exact source URLs; read-only."),
        form(InputKind::LocalFile, &["content_area_json"], "Optional JSON content-area rules for retained context."),
        form(InputKind::Selector, &["query", "representation", "variant_key", "max_body_bytes"], "query requires scan; measurements are local font widths, not search-engine truncation predictions."),
    ], ""),
    command("mcp", None, &[
        form(InputKind::NoInput, &[], ""),
    ], "Starts the local MCP server: stdio by default, opt-in streamable HTTP with --transport http; profile and progress-notification behavior are startup options."),
];
