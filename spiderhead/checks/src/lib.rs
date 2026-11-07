//! Rust port of the reference package checks/ (the 298 SEO checks).

pub mod ai_column;
/// CSS/JS weight and delivery analysis for one page.
pub mod asset_weight;
pub mod audit;
pub mod citability;
pub mod clusterer;
pub mod cwv;
/// Bounded image downloader with deterministic, filesystem-safe paths.
pub mod downloader;
pub mod extraction_suggest;
/// Markdown page export with frontmatter and failure accounting.
pub mod markdown_export;
pub mod marketing_inventory;
/// Page-grounded, resumable meta-description drafting contracts.
pub mod meta_description_drafts;
pub mod mirrors;
/// Schema.org page facts: visible evidence a suggested graph is built from.
pub mod page_facts;
/// Schema.org content-type classification over page facts.
pub mod page_type;
/// Money written the way people write it: price and currency parsing.
pub mod price;
pub mod redirect_rules;
pub mod render_policy;
/// Suggested connected Schema.org `@graph` built from page facts.
pub mod schema_build;
/// Local-first semantic similarity over normalized retained content.
pub mod semantic_similarity;
pub mod site_audit;
pub mod social_meta;
pub mod soft404;
/// `crawl-site --urls-file`: one URL list from a TXT/CSV/XLSX/XML container.
pub mod url_lists;
