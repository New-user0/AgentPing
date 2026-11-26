//! Two-way remote interaction for AgentPing.
//!
//! A Discord bot posts interactive prompts (permission requests, questions) and receives the
//! user's button presses and replies over the Discord Gateway, so no public endpoint is needed.
//! Only allow-listed Discord users are accepted. Everything maps onto what the agent's hook API
//! documents; there is no shell access.

pub mod discord;
pub mod reply;
