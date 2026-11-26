//! The provider abstraction and shared HTTP plumbing.

use std::time::Duration;

use agentping_core::redact::redact;
use async_trait::async_trait;
use serde::Serialize;

use crate::render::RenderedNotification;

/// A successful delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivered {
    /// HTTP status returned by the provider.
    pub http_status: Option<u16>,
}

/// Whether a failed delivery should be retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureKind {
    /// Temporary failure; retry (after `retry_after` when the provider asked for it).
    Retryable {
        /// Provider-requested delay.
        retry_after: Option<Duration>,
    },
    /// Permanent failure (bad credentials, deleted webhook, invalid request).
    Permanent,
}

/// A failed delivery. The message is always redacted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct DeliveryError {
    /// Retry classification.
    pub kind: FailureKind,
    /// HTTP status, when one was received.
    pub http_status: Option<u16>,
    /// Redacted, human readable description.
    pub message: String,
}

impl DeliveryError {
    /// A retryable error.
    pub fn retryable(
        message: impl AsRef<str>,
        http_status: Option<u16>,
        retry_after: Option<Duration>,
    ) -> Self {
        Self {
            kind: FailureKind::Retryable { retry_after },
            http_status,
            message: redact(message.as_ref()).into_owned(),
        }
    }

    /// A permanent error.
    pub fn permanent(message: impl AsRef<str>, http_status: Option<u16>) -> Self {
        Self {
            kind: FailureKind::Permanent,
            http_status,
            message: redact(message.as_ref()).into_owned(),
        }
    }

    /// Classifies a transport-level `reqwest` error without leaking the request URL.
    pub fn from_transport(err: reqwest::Error) -> Self {
        let what = if err.is_timeout() {
            "request timed out"
        } else if err.is_connect() {
            "could not connect"
        } else {
            "request failed"
        };
        let detail = err.without_url().to_string();
        Self::retryable(format!("{what}: {detail}"), None, None)
    }
}

/// Result of a provider health check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderHealth {
    /// Whether the provider is usable.
    pub healthy: bool,
    /// Redacted explanation.
    pub detail: String,
}

/// A notification delivery channel.
///
/// Implementations must never log or return credentials, and must classify failures so the
/// queue can retry transient problems and stop on permanent ones.
#[async_trait]
pub trait NotificationProvider: Send + Sync {
    /// Stable provider identifier (`discord`, `telegram`).
    fn name(&self) -> &'static str;

    /// Delivers one notification.
    async fn send(&self, notification: &RenderedNotification) -> Result<Delivered, DeliveryError>;

    /// Checks that the provider is reachable and the credentials are valid, without sending a
    /// visible message.
    async fn health_check(&self) -> ProviderHealth;
}

/// Builds the shared HTTP client: short timeouts, no redirects, identifying user agent.
pub fn http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .user_agent(concat!("AgentPing/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

/// Parses a `Retry-After` header expressed in (possibly fractional) seconds.
pub(crate) fn retry_after_header(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map(|s| Duration::from_secs_f64(s.min(3600.0)))
}
