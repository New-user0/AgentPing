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

    async fn send_json<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T, ClientError> {
        let token = self.auth()?;
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await.map_err(Self::map_err)?;
        Self::parse(resp).await
    }

    /// Registers a remote interaction for a waiting hook.
    pub async fn create_interaction(
        &self,
        req: &agentping_core::remote::CreateInteraction,
    ) -> Result<agentping_core::remote::CreateInteractionResult, ClientError> {
        let body = serde_json::to_value(req).map_err(|e| ClientError::Other(e.to_string()))?;
        self.send_json(reqwest::Method::POST, "/v1/interactions", Some(&body))
            .await
    }

    /// Long-polls an interaction for up to `wait_secs`.
    pub async fn wait_interaction(
        &self,
        id: &str,
        wait_secs: u64,
    ) -> Result<agentping_core::remote::InteractionState, ClientError> {
        self.send_json(
            reqwest::Method::GET,
            &format!("/v1/interactions/{id}?wait={wait_secs}"),
            None,
        )
        .await
    }

    /// Withdraws an interaction.
    pub async fn cancel_interaction(&self, id: &str) -> Result<(), ClientError> {
        let token = self.auth()?;
        let resp = self
            .http
            .delete(format!("{}/v1/interactions/{id}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .map_err(Self::map_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(ClientError::Rejected {
                status: resp.status().as_u16(),
                message: String::new(),
            })
        }
    }

    /// Remote interaction status.
    pub async fn remote_status(&self) -> Result<crate::remote::RemoteStatus, ClientError> {
        self.send_json(reqwest::Method::GET, "/v1/remote", None)
            .await
    }

    /// Sets presence (`away = true` routes prompts to Discord).
    pub async fn set_presence(
        &self,
        away: bool,
    ) -> Result<crate::remote::RemoteStatus, ClientError> {
        let body = serde_json::json!({ "away": away });
        self.send_json(reqwest::Method::POST, "/v1/remote/presence", Some(&body))
            .await
    }
}
