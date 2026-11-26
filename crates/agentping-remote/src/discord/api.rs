//! Minimal Discord REST client authenticated as a bot.

use std::time::Duration;

use agentping_core::redact::redact;
use agentping_core::secrets::Secret;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::model::{Channel, Message, User};

/// Discord API base.
pub const API_BASE: &str = "https://discord.com/api/v10";

/// Errors from the Discord API.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The token is invalid.
    #[error("Discord rejected the bot token (401); create a new token and run `agentping setup discord-bot`")]
    Unauthorized,
    /// The bot lacks access.
    #[error("the bot lacks access ({0}); invite it to the server and give it View Channel, Send Messages and Read Message History in that channel")]
    Forbidden(String),
    /// Not found.
    #[error("not found: {0}")]
    NotFound(String),
    /// Other HTTP error.
    #[error("Discord API error {status}: {message}")]
    Http {
        /// Status.
        status: u16,
        /// Message.
        message: String,
    },
    /// Transport error.
    #[error("could not reach Discord: {0}")]
    Transport(String),
}

/// Bot-authenticated REST client.
#[derive(Clone)]
pub struct DiscordApi {
    http: reqwest::Client,
    base: String,
    token: Secret,
}

impl DiscordApi {
    /// Creates a client.
    pub fn new(http: reqwest::Client, token: Secret) -> Self {
        Self {
            http,
            base: API_BASE.to_owned(),
            token,
        }
    }

    /// Creates a client against a custom base URL (tests only).
    #[doc(hidden)]
    pub fn with_base(http: reqwest::Client, token: Secret, base: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            token,
        }
    }

    /// The bot token (for the gateway).
    pub(crate) fn token(&self) -> &Secret {
        &self.token
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<T, ApiError> {
        for attempt in 0..3 {
            let mut req = self
                .http
                .request(method.clone(), format!("{}{path}", self.base))
                .header("Authorization", format!("Bot {}", self.token.expose()));
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().await.map_err(|e| {
                ApiError::Transport(redact(&e.without_url().to_string()).into_owned())
            })?;
            let status = resp.status().as_u16();
            if status == 429 && attempt < 2 {
                let wait = resp
                    .json::<Value>()
                    .await
                    .ok()
                    .and_then(|v| v.get("retry_after").and_then(Value::as_f64))
                    .unwrap_or(1.0)
                    .clamp(0.1, 10.0);
                tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                continue;
            }
            if (200..300).contains(&status) {
                let text = resp.text().await.unwrap_or_default();
                let text = if text.trim().is_empty() {
                    "null"
                } else {
                    &text
                };
                return serde_json::from_str(text).map_err(|e| ApiError::Http {
                    status,
                    message: format!("unexpected response: {e}"),
                });
            }
            let message = resp
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default();
            return Err(match status {
                401 => ApiError::Unauthorized,
                403 => ApiError::Forbidden(message),
                404 => ApiError::NotFound(message),
                _ => ApiError::Http { status, message },
            });
        }
        Err(ApiError::Http {
            status: 429,
            message: "rate limited".into(),
        })
    }

    /// The bot's own user.
    pub async fn me(&self) -> Result<User, ApiError> {
        self.request(reqwest::Method::GET, "/users/@me", None).await
    }

    /// Looks up a channel the bot can see.
    pub async fn channel(&self, channel_id: &str) -> Result<Channel, ApiError> {
        self.request(
            reqwest::Method::GET,
            &format!("/channels/{channel_id}"),
            None,
        )
        .await
    }

    /// Posts a message and returns it.
    pub async fn send_message(&self, channel_id: &str, body: &Value) -> Result<Message, ApiError> {
        self.request(
            reqwest::Method::POST,
            &format!("/channels/{channel_id}/messages"),
            Some(body),
        )
        .await
    }

    /// Edits a message.
    pub async fn edit_message(
        &self,
        channel_id: &str,
        message_id: &str,
        body: &Value,
    ) -> Result<(), ApiError> {
        let _: Value = self
            .request(
                reqwest::Method::PATCH,
                &format!("/channels/{channel_id}/messages/{message_id}"),
                Some(body),
            )
            .await?;
        Ok(())
    }

    /// Recent messages in a channel, newest first.
    pub async fn recent_messages(
        &self,
        channel_id: &str,
        limit: u8,
    ) -> Result<Vec<Message>, ApiError> {
        self.request(
            reqwest::Method::GET,
            &format!(
                "/channels/{channel_id}/messages?limit={}",
                limit.clamp(1, 100)
            ),
            None,
        )
        .await
    }

    /// Adds a reaction (e.g. ✅) to a message.
    pub async fn react(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), ApiError> {
        let encoded: String = emoji.bytes().map(|b| format!("%{b:02X}")).collect();
        let _: Value = self
            .request(
                reqwest::Method::PUT,
                &format!("/channels/{channel_id}/messages/{message_id}/reactions/{encoded}/@me"),
                None,
            )
            .await?;
        Ok(())
    }

    /// Responds to a component interaction.
    pub async fn interaction_response(
        &self,
        interaction_id: &str,
        token: &str,
        body: &Value,
    ) -> Result<(), ApiError> {
        let _: Value = self
            .request(
                reqwest::Method::POST,
                &format!("/interactions/{interaction_id}/{token}/callback"),
                Some(body),
            )
            .await?;
        Ok(())
    }
}
