//! Typed errors for the core crate.

use std::path::{Path, PathBuf};

/// Errors produced by `agentping-core`.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// The user's home directory could not be determined.
    #[error("could not determine the home directory; set AGENTPING_HOME to choose a location")]
    NoHomeDirectory,

    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The configuration file could not be parsed.
    #[error("invalid configuration in {path}: {message}")]
    ConfigParse {
        /// Configuration file path.
        path: PathBuf,
        /// Parser message (includes line/column when available).
        message: String,
    },

    /// The configuration parsed but failed validation.
    #[error("configuration is invalid:\n{}", .0.iter().map(|i| format!("  - {i}")).collect::<Vec<_>>().join("\n"))]
    ConfigInvalid(Vec<crate::config::ConfigIssue>),

    /// The configuration was written by a newer AgentPing.
    #[error("config_version {found} is newer than this build supports ({supported}); please upgrade AgentPing")]
    ConfigTooNew {
        /// Version found in the file.
        found: u32,
        /// Newest version this build understands.
        supported: u32,
    },

    /// A `config set` key does not exist or cannot be set.
    #[error("cannot set '{key}': {reason}")]
    ConfigKey {
        /// Dotted key.
        key: String,
        /// Explanation.
        reason: String,
    },

    /// Secret storage failed.
    #[error("secret storage error: {0}")]
    Secret(String),
}

impl CoreError {
    /// Convenience constructor for [`CoreError::Io`].
    pub fn io(path: &Path, source: std::io::Error) -> Self {
        CoreError::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}
