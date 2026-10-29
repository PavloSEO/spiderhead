//! `spiderhead-tui` — the interactive terminal observer shell.
//!
//! A presentation adapter over
//! `spiderhead-projects` observer APIs. The shell never starts work or runs
//! commands; it renders bounded retained evidence and routes notes/goals
//! into the project inbox.

pub mod app;
pub mod error;
pub mod keys;
pub mod labels;
pub mod localization;
pub mod panels;
pub mod pyjson;
pub mod render;
pub mod rlayout;
pub mod scan_now;
pub mod state;
pub mod text;
pub mod theme;
pub mod watch_views;

pub use error::{TuiError, TuiResult};
pub use panels::{MIN_HEIGHT, MIN_WIDTH};
pub use state::ShellState;
