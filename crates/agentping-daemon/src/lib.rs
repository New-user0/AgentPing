//! AgentPing's engine and local service: the event pipeline, the delivery worker with retries,
//! the loopback API, the API client used by hooks and the CLI, and OS service registration.

pub mod client;
pub mod delivery;
pub mod engine;
pub mod local;
pub mod process;
pub mod remote;
pub mod runtime;
pub mod server;
pub mod service;

pub use client::{ClientError, DaemonClient};
pub use engine::{Engine, IngestOutcome};
