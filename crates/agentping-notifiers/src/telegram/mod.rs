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

use crate::provider::{Delivered, DeliveryError, NotificationProvider, ProviderHealth};
use crate::render::RenderedNotification;

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

/// Formats a notification as Telegram HTML, within the message length limit.
pub fn format_message(n: &RenderedNotification) -> String {
    let mut fields = String::new();
    for f in &n.fields {
        fields.push_str(&format!(
            "\n<b>{}:</b> {}",
            escape_html(&f.name),
            escape_html(&f.value)
        ));
    }
    let header = format!("<b>{}</b>\n", escape_html(&n.title));
    let footer = format!("\n\n<i>{}</i>", escape_html(&n.footer));
    let overhead = header.chars().count() + fields.chars().count() + footer.chars().count() + 2;
    let body_budget = MESSAGE_LIMIT.saturating_sub(overhead).max(1);
    let body = escape_truncated(&n.body, body_budget);
    format!("{header}\n{body}\n{fields}{footer}")
}

/// Escapes `text`, keeping the escaped result within `budget` characters without ever cutting
/// an HTML entity in half.
fn escape_truncated(text: &str, budget: usize) -> String {
    let full = escape_html(text);
    if full.chars().count() <= budget {
        return full;
    }
    let mut out = String::new();
    let mut used = 0;
    let mut buf = [0u8; 4];
    for c in text.chars() {
        let escaped = escape_html(c.encode_utf8(&mut buf));
        let len = escaped.chars().count();
        if used + len > budget.saturating_sub(1) {
            break;
        }
        out.push_str(&escaped);
        used += len;
    }
    out.push('…');
    out
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
        let body = json!({
            "chat_id": self.chat_id,
            "text": format_message(n),
            "parse_mode": "HTML",
            "link_preview_options": { "is_disabled": true },
            "disable_notification": self.silent_low_severity && n.severity == Severity::Low,
        });
        let (status, resp) = self.call("sendMessage", &body).await?;
        if resp.ok {
            Ok(Delivered {
                http_status: Some(status),
            })
        } else {
            Err(classify(status, resp))
        }
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
