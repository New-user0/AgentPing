//! Discord webhook provider.

use std::time::Duration;

use agentping_core::secrets::Secret;
use agentping_core::EventType;
use agentping_core::Severity;
use async_trait::async_trait;
use serde_json::{json, Value};
use url::Url;

use crate::provider::{
    retry_after_header, Delivered, DeliveryError, FailureKind, NotificationProvider, ProviderHealth,
};
use crate::render::{char_len, split_text, truncate, RenderedNotification};

const ALLOWED_HOSTS: [&str; 4] = [
    "discord.com",
    "discordapp.com",
    "ptb.discord.com",
    "canary.discord.com",
];

// Discord embed limits: https://discord.com/developers/docs/resources/message#embed-object-embed-limits
const TITLE_LIMIT: usize = 256;
const DESCRIPTION_LIMIT: usize = 4096;
const FIELD_NAME_LIMIT: usize = 256;
const FIELD_VALUE_LIMIT: usize = 1024;
const FOOTER_LIMIT: usize = 2048;
const CONTENT_LIMIT: usize = 2000;
const EMBED_TOTAL_LIMIT: usize = 6000;
const _: () = assert!(CHUNK_LIMIT + 8 <= DESCRIPTION_LIMIT);

/// Validates a Discord webhook URL, returning a normalized URL.
///
/// Only `https://{discord.com,discordapp.com,ptb.discord.com,canary.discord.com}/api[/vN]/webhooks/<id>/<token>`
/// is accepted, which prevents the webhook setting from being abused to send event data to
/// arbitrary hosts. Error messages never include the token.
pub fn validate_webhook_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim()).map_err(|_| "not a valid URL".to_owned())?;
    if url.scheme() != "https" {
        return Err("must use https://".into());
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if !ALLOWED_HOSTS.contains(&host.as_str()) {
        return Err(format!(
            "host '{host}' is not a Discord host (expected discord.com)"
        ));
    }
    if url.port().is_some() || !url.username().is_empty() || url.password().is_some() {
        return Err("must not contain a port or credentials".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("must not contain a query string or fragment".into());
    }
    let segments: Vec<&str> = url
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    let rest = match segments.as_slice() {
        ["api", version, rest @ ..]
            if version.len() > 1
                && version.starts_with('v')
                && version[1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            rest
        }
        ["api", rest @ ..] => rest,
        _ => return Err("path must look like /api/webhooks/<id>/<token>".into()),
    };
    match rest {
        ["webhooks", id, token]
            if !id.is_empty()
                && id.chars().all(|c| c.is_ascii_digit())
                && token.len() >= 20
                && token
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
        {
            Ok(url)
        }
        _ => Err("path must look like /api/webhooks/<id>/<token>".into()),
    }
}

/// Embed color for an event.
pub fn color(event_type: &EventType, severity: Severity) -> u32 {
    if severity == Severity::Urgent {
        return 0xE74C3C;
    }
    match event_type {
        EventType::PermissionRequest => 0xE67E22,
        EventType::UserQuestion => 0x9B59B6,
        EventType::TaskCompleted => 0x2ECC71,
        EventType::TaskFailed => 0xE74C3C,
        EventType::AgentIdle | EventType::AgentStarted | EventType::AgentStopped => 0x95A5A6,
        _ => 0x3498DB,
    }
}

/// Description chunk size; leaves headroom under Discord's 4096 limit for code-fence repair.
const CHUNK_LIMIT: usize = 3900;

/// Closes and reopens Markdown code fences that a chunk boundary would otherwise break.
fn balance_code_fences(chunks: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(chunks.len());
    let mut open = false;
    for chunk in chunks {
        let mut text = if open { format!("```\n{chunk}") } else { chunk };
        if text.matches("```").count() % 2 == 1 {
            text.push_str("\n```");
            open = true;
        } else {
            open = false;
        }
        out.push(text);
    }
    out
}

/// Builds the webhook payloads for a notification. Long messages are split across several
/// messages (`part i/n` in the footer) instead of being truncated.
pub fn build_payloads(n: &RenderedNotification, username: &str) -> Vec<Value> {
    let title = truncate(&n.title, TITLE_LIMIT);
    let mut fields = Vec::new();
    let mut field_cost = 0;
    for f in n.fields.iter().take(25) {
        let name = truncate(&f.name, FIELD_NAME_LIMIT);
        let value = truncate(&f.value, FIELD_VALUE_LIMIT);
        field_cost += char_len(&name) + char_len(&value);
        fields.push(json!({ "name": name, "value": value, "inline": true }));
    }
    let mut chunks = balance_code_fences(split_text(&n.body, CHUNK_LIMIT, &char_len));
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    let parts = chunks.len();
    let color = color(&n.event_type, n.severity);
    // `content` drives the mobile push preview, so keep it short and meaningful.
    let content = match &n.project {
        Some(p) => format!("{} · {p}", n.title),
        None => n.title.clone(),
    };
    let mut payloads = Vec::with_capacity(parts);
    for (i, chunk) in chunks.into_iter().enumerate() {
        let footer_text = if parts > 1 {
            format!("{} · part {}/{}", n.footer, i + 1, parts)
        } else {
            n.footer.clone()
        };
        let footer = truncate(&footer_text, FOOTER_LIMIT);
        let embed_title = if i == 0 {
            title.clone()
        } else {
            truncate(&format!("{title} (continued)"), TITLE_LIMIT)
        };
        let mut embed = json!({
            "title": embed_title,
            "description": chunk,
            "color": color,
            "footer": { "text": footer },
            "timestamp": n.occurred_at.to_rfc3339(),
        });
        let used = char_len(&embed_title) + char_len(&chunk) + char_len(&footer);
        if i == 0 && used + field_cost <= EMBED_TOTAL_LIMIT {
            embed["fields"] = Value::Array(fields.clone());
        }
        let mut payload = json!({
            "embeds": [embed],
            // Never let agent-provided text ping @everyone, roles or users.
            "allowed_mentions": { "parse": [] },
        });
        if i == 0 {
            payload["content"] = Value::String(truncate(&content, CONTENT_LIMIT));
        }
        if !username.trim().is_empty() {
            payload["username"] = Value::String(truncate(username.trim(), 80));
        }
        payloads.push(payload);
    }
    payloads
}

/// Delivers notifications through a Discord webhook.
pub struct DiscordProvider {
    http: reqwest::Client,
    webhook: Url,
    username: String,
}

impl DiscordProvider {
    /// Creates a provider after validating the webhook URL.
    pub fn new(http: reqwest::Client, webhook: &Secret, username: &str) -> Result<Self, String> {
        let webhook = validate_webhook_url(webhook.expose())?;
        Ok(Self {
            http,
            webhook,
            username: username.to_owned(),
        })
    }

    /// Creates a provider for an arbitrary endpoint without validation.
    ///
    /// Only for tests against a local mock server.
    #[doc(hidden)]
    pub fn new_unchecked(http: reqwest::Client, endpoint: Url, username: &str) -> Self {
        Self {
            http,
            webhook: endpoint,
            username: username.to_owned(),
        }
    }

    fn send_url(&self) -> Url {
        let mut url = self.webhook.clone();
        url.query_pairs_mut().append_pair("wait", "true");
        url
    }

    async fn post(&self, payload: &Value) -> Result<Delivered, DeliveryError> {
        let resp = self
            .http
            .post(self.send_url())
            .json(payload)
            .send()
            .await
            .map_err(DeliveryError::from_transport)?;
        let status = resp.status();
        let code = Some(status.as_u16());
        if status.is_success() {
            // With `?wait=true` Discord returns the created message.
            let id = resp
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_owned));
            return Ok(Delivered {
                http_status: code,
                message_ids: id.into_iter().collect(),
            });
        }
        if status.as_u16() == 429 {
            let header = retry_after_header(resp.headers());
            let body: Option<Value> = resp.json().await.ok();
            let from_body = body
                .as_ref()
                .and_then(|v| v.get("retry_after"))
                .and_then(Value::as_f64)
                .filter(|s| s.is_finite() && *s >= 0.0)
                .map(|s| Duration::from_secs_f64(s.min(3600.0)));
            return Err(DeliveryError::retryable(
                "rate limited by Discord",
                code,
                from_body.or(header),
            ));
        }
        if status.is_server_error() {
            let msg = error_text(resp).await;
            return Err(DeliveryError::retryable(
                format!("Discord server error {}: {msg}", status.as_u16()),
                code,
                None,
            ));
        }
        let msg = error_text(resp).await;
        let hint = match status.as_u16() {
            401 | 403 | 404 => {
                " (the webhook may have been deleted; run `agentping setup discord`)"
            }
            _ => "",
        };
        Err(DeliveryError::permanent(
            format!(
                "Discord rejected the message ({}): {msg}{hint}",
                status.as_u16()
            ),
            code,
        ))
    }
}

async fn error_text(resp: reqwest::Response) -> String {
    let text = resp.text().await.unwrap_or_default();
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| truncate(text.trim(), 200))
}

#[async_trait]
impl NotificationProvider for DiscordProvider {
    fn name(&self) -> &'static str {
        "discord"
    }

    async fn send(&self, n: &RenderedNotification) -> Result<Delivered, DeliveryError> {
        let payloads = build_payloads(n, &self.username);
        let mut all = Delivered {
            http_status: None,
            message_ids: Vec::new(),
        };
        for (i, payload) in payloads.iter().enumerate() {
            let mut attempt = 0;
            loop {
                match self.post(payload).await {
                    Ok(d) => {
                        all.http_status = d.http_status;
                        all.message_ids.extend(d.message_ids);
                        break;
                    }
                    // Once part 1 is out, retrying the whole notification would duplicate it,
                    // so ride out short rate limits for the remaining parts in place.
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
        match self.http.get(self.webhook.clone()).send().await {
            Ok(resp) if resp.status().is_success() => {
                let name = resp
                    .json::<Value>()
                    .await
                    .ok()
                    .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_owned));
                ProviderHealth {
                    healthy: true,
                    detail: match name {
                        Some(n) => format!("webhook '{n}' is reachable"),
                        None => "webhook is reachable".into(),
                    },
                }
            }
            Ok(resp) => ProviderHealth {
                healthy: false,
                detail: format!(
                    "Discord returned {} for the webhook{}",
                    resp.status().as_u16(),
                    if resp.status().as_u16() == 404 {
                        " (deleted or wrong URL; run `agentping setup discord`)"
                    } else {
                        ""
                    }
                ),
            },
            Err(e) => ProviderHealth {
                healthy: false,
                detail: DeliveryError::from_transport(e).message,
            },
        }
    }
}

#[cfg(test)]
mod tests;
