//! External demand, SERP and analytics sources used by the toolkit: one canonical client per
//! provider, behind the [`http::Transport`] seam. Layer rules:
//!
//! * a client does not know who called it: it validates a request and returns provider data;
//! * network and quota failures are not process failures: they are error values;
//! * secrets come only from files or environment variables and never appear in logs
//!   ([`credentials`]);
//! * every paid call is written to the spend log ([`spend`]);
//! * this layer does not own project state.

pub mod analytics_findings;
pub mod arsenkin;
pub mod backlink_history;
pub mod backlink_indexes;
pub mod bing_webmaster;
pub mod cloudflare;
pub mod cloudflare_history;
pub mod cloudflare_logs;
pub mod cloudflare_traffic;
pub mod credentials;
pub mod crtsh;
pub mod crux;
pub mod cwv;
pub mod dataforseo;
pub mod evidence_import;
pub mod evidence_join;
pub mod evidence_join_store;
pub mod external_join;
pub mod ga4;
pub mod ga4_content;
pub mod gsc;
pub mod gsc_inspection_history;
pub mod http;
pub mod indexnow;
pub mod metrika;
pub mod metrika_traffic;
pub mod miratext;
pub mod oauth;
pub mod oauth_flow;
pub mod pagespeed;
pub mod provider_artifacts;
pub mod provider_findings;
pub mod providers;
pub mod providers_lifecycle;
pub mod pyerr;
pub mod pyjson;
pub mod pystr;
pub mod pytime;
pub mod search_console_findings;
pub mod sources_db;
pub mod spend;
pub mod spend_gate;
pub mod topvisor;
pub mod wayback;
pub mod yandex_cloud;
pub mod yandex_regions;
pub mod yandex_webmaster;
