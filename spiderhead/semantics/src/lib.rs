//! Semantic-core pipeline
//! (`seohead semantics <stage>`).

pub mod config;
pub mod demand;
pub mod louvain;
pub mod norm;
pub mod providers;
pub mod pyfmt;
pub mod pymt;
pub mod pyset;
pub mod read_model;
pub mod runner;
pub mod stages;
pub mod store;
mod store_export;

/// Crate-wide error type (thiserror per repo rules).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Configuration or user-input error.
    #[error("{0}")]
    Config(String),
    /// SQLite driver error.
    #[error("sqlite: {0}")]
    Db(rusqlite::Error),
    /// Filesystem or stream error.
    #[error("io: {0}")]
    Io(String),
    /// Provider seam error (paid stage went to a missing/failed provider).
    #[error("provider: {0}")]
    Provider(String),
    /// A pipeline stage failed; carries the stage name and error text.
    #[error("stage {stage}: {message}")]
    Stage {
        /// Stage name.
        stage: String,
        /// Error text.
        message: String,
    },
}
