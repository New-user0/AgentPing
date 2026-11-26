//! Discord bot: REST API, Gateway connection and interactive prompts.

pub mod api;
pub mod gateway;
pub mod model;
pub mod prompt;

pub use api::{ApiError, DiscordApi};
pub use gateway::GatewayEvent;
