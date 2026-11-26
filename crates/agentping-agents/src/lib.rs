//! Agent integrations. Each adapter translates native agent payloads into the unified
//! [`agentping_core::Event`] model; nothing downstream knows about agent-specific formats.

pub mod claude;

use std::path::PathBuf;

use agentping_core::{AgentKind, Event};
use chrono::{DateTime, Utc};

/// Maximum accepted size of a native payload.
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

/// Errors produced while parsing native payloads.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AdapterError {
    /// The payload is empty.
    #[error("empty payload")]
    Empty,
    /// The payload exceeds [`MAX_PAYLOAD_BYTES`].
    #[error("payload is larger than {MAX_PAYLOAD_BYTES} bytes")]
    TooLarge,
    /// The payload is not valid JSON.
    #[error("invalid JSON: {0}")]
    InvalidJson(String),
    /// The payload is valid JSON but not an object.
    #[error("payload is not a JSON object")]
    NotAnObject,
    /// A required field is missing.
    #[error("missing required field '{0}'")]
    MissingField(&'static str),
}

/// Environment facts available to adapters.
#[derive(Debug, Clone)]
pub struct AdapterContext {
    /// When the payload was received.
    pub received_at: DateTime<Utc>,
    /// Project root supplied by the agent's environment (e.g. `CLAUDE_PROJECT_DIR`).
    pub project_dir: Option<PathBuf>,
}

impl AdapterContext {
    /// Context for "now" with no project hint.
    pub fn now() -> Self {
        Self {
            received_at: Utc::now(),
            project_dir: None,
        }
    }
}

/// Translates native agent payloads into AgentPing events.
pub trait AgentAdapter {
    /// The agent this adapter handles.
    fn kind(&self) -> AgentKind;

    /// Parses one native payload. Returns `Ok(None)` for payloads that are valid but
    /// intentionally ignored.
    fn parse(&self, payload: &[u8], ctx: &AdapterContext) -> Result<Option<Event>, AdapterError>;
}
