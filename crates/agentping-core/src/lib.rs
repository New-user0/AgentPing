//! Core building blocks for AgentPing: the unified event model, configuration, platform paths,
//! credential storage and secret redaction.
//!
//! This crate has no knowledge of specific agents or notification providers.

pub mod config;
pub mod error;
pub mod event;
pub mod fsutil;
pub mod paths;
pub mod project;
pub mod redact;
pub mod remote;
pub mod retry;
pub mod secrets;
pub mod time;

pub use config::Config;
pub use error::CoreError;
pub use event::{AgentKind, Event, EventType, Severity};
pub use paths::Paths;

/// AgentPing version (from Cargo).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Product name.
pub const PRODUCT_NAME: &str = "AgentPing";

/// Product tagline.
pub const TAGLINE: &str = "Stay connected to your AI coding agents, wherever you are.";
