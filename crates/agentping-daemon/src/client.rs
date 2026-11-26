//! Client for the local daemon API, used by hooks and the CLI.

use std::time::Duration;

use agentping_core::{Config, Event, Paths};
use serde::de::DeserializeOwned;

use crate::local::read_token;
use crate::server::{Health, StatusResponse};

/// Errors talking to the daemon.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The daemon is not reachable.
    #[error("the AgentPing daemon is not running")]
    Unavailable,
    /// No API token exists (the daemon has never run).
    #[error("no daemon token found")]
    NoToken,
    /// The daemon rejected the request.
    #[error("daemon returned {status}: {message}")]
    Rejected {
        /// HTTP status.
        status: u16,
        /// Message.
        message: String,
    },
    /// Anything else.
    #[error("{0}")]
    Other(String),
}

/// Talks to the daemon over loopback HTTP.
pub struct DaemonClient {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl DaemonClient {
    /// Builds a client from configuration with the given total request timeout.
    pub fn new(paths: &Paths, config: &Config, timeout: Duration) -> Result<Self, ClientError> {
        let host = if config.daemon.host.contains(':') && !config.daemon.host.starts_with('[') {
            format!("[{}]", config.daemon.host)
        } else {
            config.daemon.host.clone()
        };
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_millis(300).min(timeout))
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ClientError::Other(e.without_url().to_string()))?;
        Ok(Self {
            base: format!("http://{host}:{}", config.daemon.port),
            token: read_token(paths),
            http,
        })
    }

    /// Base URL of the daemon.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn map_err(e: reqwest::Error) -> ClientError {
        if e.is_connect() || e.is_timeout() {
            ClientError::Unavailable
        } else {
            ClientError::Other(e.without_url().to_string())
        }
    }

    async fn parse<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T, ClientError> {
        let status = resp.status();
        if !status.is_success() {
            let message = resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
                .unwrap_or_default();
            return Err(ClientError::Rejected {
                status: status.as_u16(),
                message,
            });
        }
        resp.json()
            .await
            .map_err(|e| ClientError::Other(e.without_url().to_string()))
    }

    fn auth(&self) -> Result<&str, ClientError> {
        self.token.as_deref().ok_or(ClientError::NoToken)
    }

    /// Unauthenticated liveness check.
    pub async fn health(&self) -> Result<Health, ClientError> {
        let resp = self
            .http
            .get(format!("{}/v1/health", self.base))
            .send()
            .await
            .map_err(Self::map_err)?;
        let health: Health = Self::parse(resp).await?;
        if health.service != "agentping" {
            return Err(ClientError::Other(
                "another service is listening on the AgentPing port".into(),
            ));
        }
        Ok(health)
    }

    /// Sends an event for ingestion.
    pub async fn send_event(&self, event: &Event) -> Result<serde_json::Value, ClientError> {
        let token = self.auth()?;
        let resp = self
            .http
            .post(format!("{}/v1/events", self.base))
            .bearer_auth(token)
            .json(event)
            .send()
            .await
            .map_err(Self::map_err)?;
        Self::parse(resp).await
    }

    /// Fetches daemon status.
    pub async fn status(&self) -> Result<StatusResponse, ClientError> {
        let token = self.auth()?;
        let resp = self
            .http
            .get(format!("{}/v1/status", self.base))
            .bearer_auth(token)
            .send()
            .await
            .map_err(Self::map_err)?;
        Self::parse(resp).await
    }

    /// Asks the daemon to shut down gracefully.
    pub async fn shutdown(&self) -> Result<(), ClientError> {
        let token = self.auth()?;
        let resp = self
            .http
            .post(format!("{}/v1/admin/shutdown", self.base))
            .bearer_auth(token)
            .send()
            .await
            .map_err(Self::map_err)?;
        let _: serde_json::Value = Self::parse(resp).await?;
        Ok(())
    }
}
