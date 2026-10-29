//! Project workspaces: revisioned documents, journals, evidence, coverage checklists,
//! run observation and bounded local delivery.

pub mod action_log;
pub mod archive;
pub mod bindings;
pub mod catalogue;
pub mod competitor_scans;
pub mod coverage;
pub mod crawl_evidence;
pub mod error;
pub mod event_log;
pub mod evidence;
pub mod evidence_cache;
pub mod execution;
pub mod exports;
pub mod facts;
pub mod finding_views;
pub mod inbox;
pub mod io;
pub mod journal;
pub mod locks;
pub mod monitoring;
pub mod observer;
pub mod origin_pacing;
pub mod permissions;
pub mod playbooks;
pub mod priorities;
pub mod progress;
pub mod project_settings;
pub mod projection;
pub mod pyjson;
pub mod pytime;
pub mod run_control;
pub mod run_observation;
pub mod runtime;
pub mod schedule;
pub mod schedule_install;
pub mod service_delivery;
pub mod status_measurement;
pub mod urls;
pub mod workspace;

pub use error::{ProjectsError, ProjectsResult};
pub use workspace::{create_project, open_project, project_status};
