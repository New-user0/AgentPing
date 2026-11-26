//! Telegram Bot API provider.

use std::sync::OnceLock;
use std::time::Duration;

use agentping_core::secrets::Secret;
use agentping_core::Severity;
use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use url::Url;

use crate::provider::{
    Delivered, DeliveryError, FailureKind, NotificationProvider, ProviderHealth,
};
use crate::render::{char_len, split_text, RenderedNotification};

const API_BASE: &str = "https://api.telegram.org";
const MESSAGE_LIMIT: usize = 4096;

/// Validates the shape of a bot token (`<digits>:<secret>`). Never echoes the token.
pub fn validate_bot_token(token: &str) -> Result<(), String> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^\d{5,15}:[A-Za-z0-9_-]{30,64}$").ok());
    match re {
        Some(re) if re.is_match(token.trim()) => Ok(()),
        _ => Err("does not look like a Telegram bot token (expected 123456789:ABC…)".into()),
    }
}

/// Escapes text for Telegram's HTML parse mode.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Formats a notification as one or more Telegram HTML messages, each within the 4096
/// character limit. Long bodies are split rather than truncated.
pub fn format_messages(n: &RenderedNotification) -> Vec<String> {
    let mut fields = String::new();
    for f in &n.fields {
        fields.push_str(&format!(
            "\n<b>{}:</b> {}",
            escape_html(&f.name),
            escape_html(&f.value)
        ));
    }
    let header = format!("<b>{}</b>\n", escape_html(&n.title));
    let continued = format!("<b>{} (continued)</b>\n", escape_html(&n.title));
    // Reserve room for the header, the fields/footer (on the last part) and the part label.
    let reserve = char_len(&continued) + char_len(&fields) + char_len(&escape_html(&n.footer)) + 40;
    let budget = MESSAGE_LIMIT.saturating_sub(reserve).max(200);
    let measure = |s: &str| char_len(&escape_html(s));
    let mut chunks = split_text(&n.body, budget, &measure);
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    let parts = chunks.len();
    let footer = escape_html(&n.footer);
    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let head = if i == 0 { &header } else { &continued };
            let body = escape_html(chunk);
            let label = if parts > 1 {
                format!(" · part {}/{parts}", i + 1)
            } else {
                String::new()
            };
            let tail = if i + 1 == parts { fields.as_str() } else { "" };
            format!("{head}\n{body}\n{tail}\n\n<i>{footer}{label}</i>")
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    ok: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    parameters: Option<ApiParameters>,
}

#[derive(Debug, Deserialize)]
struct ApiParameters {
    #[serde(default)]
    retry_after: Option<u64>,
}

/// A chat discovered through `getUpdates`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredChat {
    /// Chat id.
    pub id: String,
    /// Display label (title, username or first name).
    pub label: String,
    /// Chat type (`private`, `group`, `supergroup`, `channel`).
    pub kind: String,
}

/// Delivers notifications through a Telegram bot.
pub struct TelegramProvider {
    http: reqwest::Client,
    base: Url,
    token: Secret,
    chat_id: String,
    silent_low_severity: bool,
}

impl TelegramProvider {
    /// Creates a provider after validating the token format.
    pub fn new(
        http: reqwest::Client,
        token: Secret,
        chat_id: &str,
        silent_low_severity: bool,
    ) -> Result<Self, String> {
        validate_bot_token(token.expose())?;
        let base = Url::parse(API_BASE).map_err(|e| e.to_string())?;
        Ok(Self {
            http,
            base,
            token,
            chat_id: chat_id.trim().to_owned(),
            silent_low_severity,
        })
    }

    /// Creates a provider against a custom API base. Only for tests against a mock server.
    #[doc(hidden)]
    pub fn new_with_base(http: reqwest::Client, base: Url, token: Secret, chat_id: &str) -> Self {
        Self {
            http,
            base,
            token,
            chat_id: chat_id.to_owned(),
            silent_low_severity: false,
        }
    }

    fn method_url(&self, method: &str) -> Result<Url, DeliveryError> {
        self.base
            .join(&format!("./bot{}/{method}", self.token.expose()))
            .map_err(|_| DeliveryError::permanent("invalid Telegram API URL", None))
    }

    async fn call(&self, method: &str, body: &Value) -> Result<(u16, ApiResponse), DeliveryError> {
        let resp = self
            .http
            .post(self.method_url(method)?)
            .json(body)
            .send()
            .await
            .map_err(DeliveryError::from_transport)?;
        let status = resp.status().as_u16();
        let parsed: ApiResponse = resp.json().await.map_err(|_| {
            DeliveryError::retryable(
                format!("unexpected response from Telegram (HTTP {status})"),
                Some(status),
                None,
            )
        })?;
        Ok((status, parsed))
    }

    /// Lists chats that recently messaged the bot, to help users find their chat id.
    pub async fn discover_chats(&self) -> Result<Vec<DiscoveredChat>, DeliveryError> {
        let (status, resp) = self.call("getUpdates", &json!({ "limit": 50 })).await?;
        if !resp.ok {
            return Err(classify(status, resp));
        }
        let mut chats: Vec<DiscoveredChat> = Vec::new();
        for update in resp
            .result
            .as_ref()
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let chat = [
                "message",
                "channel_post",
                "my_chat_member",
                "edited_message",
            ]
            .iter()
            .find_map(|k| update.get(k).and_then(|m| m.get("chat")));
            let Some(chat) = chat else { continue };
            let Some(id) = chat.get("id").and_then(Value::as_i64) else {
                continue;
            };
            let label = ["title", "username", "first_name"]
                .iter()
                .find_map(|k| chat.get(k).and_then(Value::as_str))
                .unwrap_or("unknown")
                .to_owned();
            let kind = chat
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            if !chats.iter().any(|c| c.id == id.to_string()) {
                chats.push(DiscoveredChat {
                    id: id.to_string(),
                    label,
                    kind,
                });
            }
        }
        Ok(chats)
    }
}

fn classify(status: u16, resp: ApiResponse) -> DeliveryError {
    let description = resp.description.unwrap_or_else(|| "unknown error".into());
    let retry_after = resp
        .parameters
        .and_then(|p| p.retry_after)
        .map(Duration::from_secs);
    if status == 429 || retry_after.is_some() {
        return DeliveryError::retryable(
            format!("rate limited by Telegram: {description}"),
            Some(status),
            retry_after,
        );
    }
    if status >= 500 {
        return DeliveryError::retryable(
            format!("Telegram server error {status}: {description}"),
            Some(status),
            None,
        );
    }
    let hint = match status {
        401 => " (the bot token is invalid; run `agentping setup telegram`)",
        400 if description.contains("chat not found") => {
            " (check providers.telegram.chat_id and that you messaged the bot first)"
        }
        403 => " (the bot was blocked or removed from the chat)",
        _ => "",
    };
    DeliveryError::permanent(
        format!("Telegram rejected the message ({status}): {description}{hint}"),
        Some(status),
    )
}

#[async_trait]
impl NotificationProvider for TelegramProvider {
    fn name(&self) -> &'static str {
        "telegram"
    }

    async fn send(&self, n: &RenderedNotification) -> Result<Delivered, DeliveryError> {
        let mut all = Delivered {
            http_status: None,
            message_ids: Vec::new(),
        };
        for (i, text) in format_messages(n).iter().enumerate() {
            let silent = i > 0 || (self.silent_low_severity && n.severity == Severity::Low);
            let body = json!({
                "chat_id": self.chat_id,
                "text": text,
                "parse_mode": "HTML",
                "link_preview_options": { "is_disabled": true },
                "disable_notification": silent,
            });
            let mut attempt = 0;
            loop {
                let result = match self.call("sendMessage", &body).await {
                    Ok((status, resp)) if resp.ok => Ok((
                        status,
                        resp.result
                            .as_ref()
                            .and_then(|r| r.get("message_id"))
                            .and_then(Value::as_i64),
                    )),
                    Ok((status, resp)) => Err(classify(status, resp)),
                    Err(e) => Err(e),
                };
                match result {
                    Ok((status, id)) => {
                        all.http_status = Some(status);
                        all.message_ids.extend(id.map(|i| i.to_string()));
                        break;
                    }
                    // Avoid re-sending earlier parts: wait out short rate limits in place.
                    Err(DeliveryError {
                        kind: FailureKind::Retryable { retry_after },
                        ..
                    }) if i > 0 && attempt < 3 => {
                        attempt += 1;
                        let wait = retry_after.unwrap_or(Duration::from_secs(2));
                        tokio::time::sleep(wait.min(Duration::from_secs(10))).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(all)
    }

    async fn health_check(&self) -> ProviderHealth {
        match self.call("getMe", &json!({})).await {
            Ok((_, resp)) if resp.ok => {
                let name = resp
                    .result
                    .as_ref()
                    .and_then(|r| r.get("username"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                ProviderHealth {
                    healthy: true,
                    detail: match name {
                        Some(n) => format!("bot @{n} is reachable"),
                        None => "bot is reachable".into(),
                    },
                }
            }
            Ok((status, resp)) => ProviderHealth {
                healthy: false,
                detail: classify(status, resp).message,
            },
            Err(e) => ProviderHealth {
                healthy: false,
                detail: e.message,
            },
        }
    }
}

#[cfg(test)]
mod tests;
