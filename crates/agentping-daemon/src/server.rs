//! Versioned loopback HTTP API (`/v1`).
//!
//! Security model: binds to loopback only; every endpoint except `/v1/health` requires the
//! bearer token stored in the user's data directory; requests whose `Host` is not a loopback
//! name (DNS rebinding) or that carry an `Origin` header (browser pages) are rejected; bodies
//! are size-limited; there are no endpoints that execute commands.

use std::net::SocketAddr;
use std::sync::Arc;

use agentping_core::{Event, VERSION};
use agentping_storage::{EventQuery, SessionRecord, Stats};
use axum::extract::{DefaultBodyLimit, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{watch, Notify};

use crate::delivery::{with_db, SharedDb};
use crate::engine::{Engine, IngestOutcome};
use crate::local::constant_time_eq;
use crate::remote::{RemoteService, RemoteStatus};
use agentping_core::remote::CreateInteraction;
use axum::extract::Path;

/// Maximum request body size.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// Shared server state.
#[derive(Clone)]
pub struct AppState {
    /// Database.
    pub db: SharedDb,
    /// Event pipeline.
    pub engine: Arc<Engine>,
    /// API token.
    pub token: Arc<str>,
    /// Wakes the delivery worker after new events.
    pub wake: Arc<Notify>,
    /// Signals shutdown.
    pub shutdown: watch::Sender<bool>,
    /// Daemon start time.
    pub started_at: DateTime<Utc>,
    /// Bound port (for Host validation).
    pub port: u16,
    /// Names of providers that are ready.
    pub providers_ready: Arc<Vec<String>>,
    /// Providers that failed to initialize, with reasons.
    pub providers_failed: Arc<Vec<(String, String)>>,
    /// Two-way remote interaction, when enabled.
    pub remote: Option<Arc<RemoteService>>,
}

/// `GET /v1/health` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    /// Always `ok`.
    pub status: String,
    /// Service name.
    pub service: String,
    /// Version.
    pub version: String,
    /// Seconds since start.
    pub uptime_secs: i64,
}

/// `GET /v1/status` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    /// Version.
    pub version: String,
    /// Start time.
    pub started_at: DateTime<Utc>,
    /// Queue and event counters.
    pub stats: Stats,
    /// Sessions active in the last 24h.
    pub sessions: Vec<SessionRecord>,
    /// Ready providers.
    pub providers_ready: Vec<String>,
    /// Misconfigured providers.
    pub providers_failed: Vec<(String, String)>,
}

#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ApiError {
            error: message.into(),
        }),
    )
        .into_response()
}

/// Builds the router.
pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/v1/status", get(status))
        .route("/v1/events", post(ingest).get(list_events))
        .route("/v1/sessions", get(sessions))
        .route("/v1/admin/shutdown", post(shutdown))
        .route("/v1/interactions", post(create_interaction))
        .route(
            "/v1/interactions/{id}",
            get(wait_interaction).delete(cancel_interaction),
        )
        .route("/v1/remote", get(remote_status))
        .route("/v1/remote/presence", post(set_presence))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));
    Router::new()
        .route("/v1/health", get(health))
        .merge(protected)
        .layer(middleware::from_fn_with_state(state.clone(), local_only))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

fn host_allowed(host: &str, port: u16) -> bool {
    let (name, port_part) = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((name, after)) => (name, after.strip_prefix(':')),
            None => return false,
        }
    } else {
        match host.split_once(':') {
            Some((name, p)) => (name, Some(p)),
            None => (host, None),
        }
    };
    if let Some(p) = port_part {
        if p.parse::<u16>() != Ok(port) {
            return false;
        }
    }
    !name.is_empty() && agentping_core::config::is_loopback_host(name)
}

async fn local_only(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !host_allowed(host, state.port) {
        return error(
            StatusCode::FORBIDDEN,
            "requests must target a loopback host",
        );
    }
    if req.headers().contains_key(header::ORIGIN) {
        return error(StatusCode::FORBIDDEN, "browser requests are not allowed");
    }
    next.run(req).await
}

async fn require_token(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");
    if !constant_time_eq(provided, &state.token) {
        return error(StatusCode::UNAUTHORIZED, "missing or invalid token");
    }
    next.run(req).await
}

async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok".into(),
        service: "agentping".into(),
        version: VERSION.into(),
        uptime_secs: (Utc::now() - state.started_at).num_seconds(),
    })
}

async fn status(State(state): State<AppState>) -> Response {
    let result = with_db(&state.db, |db| {
        let now = Utc::now();
        let store = db.store();
        Ok((
            store.stats(now)?,
            store.sessions(now - chrono::Duration::hours(24), false)?,
        ))
    })
    .await;
    match result {
        Ok((stats, sessions)) => Json(StatusResponse {
            version: VERSION.into(),
            started_at: state.started_at,
            stats,
            sessions,
            providers_ready: state.providers_ready.to_vec(),
            providers_failed: state.providers_failed.to_vec(),
        })
        .into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn ingest(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let event: Event = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("invalid event: {e}")),
    };
    if let Err(e) = event.validate() {
        return error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string());
    }
    let engine = state.engine.clone();
    let result = with_db(&state.db, move |db| engine.ingest(db, &event, Utc::now())).await;
    match result {
        Ok(outcome) => {
            if outcome.queued() > 0 {
                state.wake.notify_one();
            }
            if let Some(remote) = state.remote.clone() {
                let closed = outcome.closed_interactions().to_vec();
                if !closed.is_empty() {
                    tokio::spawn(async move { remote.on_closed(closed).await });
                }
            }
            let code = match outcome {
                IngestOutcome::Accepted { .. } => StatusCode::ACCEPTED,
                IngestOutcome::Duplicate { .. } => StatusCode::OK,
            };
            (code, Json(outcome)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to ingest event");
            error(StatusCode::INTERNAL_SERVER_ERROR, "failed to store event")
        }
    }
}

#[derive(Debug, Deserialize)]
struct EventsParams {
    limit: Option<u32>,
    agent: Option<String>,
    project: Option<String>,
    #[serde(rename = "type")]
    event_type: Option<String>,
}

async fn list_events(State(state): State<AppState>, Query(p): Query<EventsParams>) -> Response {
    let query = EventQuery {
        limit: Some(p.limit.unwrap_or(50).min(500)),
        agent: p.agent,
        project: p.project,
        event_type: p.event_type,
        ..Default::default()
    };
    match with_db(&state.db, move |db| Ok(db.store().list_events(&query)?)).await {
        Ok(events) => Json(events).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn sessions(State(state): State<AppState>) -> Response {
    let result = with_db(&state.db, |db| {
        Ok(db
            .store()
            .sessions(Utc::now() - chrono::Duration::hours(24), true)?)
    })
    .await;
    match result {
        Ok(s) => Json(s).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn shutdown(State(state): State<AppState>) -> Response {
    tracing::info!("shutdown requested through the API");
    let _ = state.shutdown.send(true);
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "status": "stopping" })),
    )
        .into_response()
}

/// Binds the listener on a loopback address.
pub async fn bind(host: &str, port: u16) -> anyhow::Result<tokio::net::TcpListener> {
    if !agentping_core::config::is_loopback_host(host) {
        anyhow::bail!("refusing to bind to non-loopback address '{host}'");
    }
    let host = if host.eq_ignore_ascii_case("localhost") {
        "127.0.0.1"
    } else {
        host
    };
    let ip: std::net::IpAddr = host.trim_matches(['[', ']']).parse()?;
    let addr = SocketAddr::new(ip, port);
    tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow::anyhow!(
                "{addr} is already in use; is the AgentPing daemon already running? (`agentping daemon status`)"
            )
        } else {
            anyhow::anyhow!("could not bind {addr}: {e}")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::host_allowed;

    #[test]
    fn host_validation() {
        assert!(host_allowed("127.0.0.1:38471", 38471));
        assert!(host_allowed("localhost:38471", 38471));
        assert!(host_allowed("[::1]:38471", 38471));
        assert!(host_allowed("[::1]", 38471));
        assert!(host_allowed("127.0.0.1", 38471));
        assert!(!host_allowed("127.0.0.1:9999", 38471));
        assert!(!host_allowed("evil.example:38471", 38471));
        assert!(!host_allowed("attacker.com", 38471));
        assert!(!host_allowed("", 38471));
    }
}

fn remote_disabled() -> Response {
    error(
        StatusCode::CONFLICT,
        "remote interaction is not enabled (run `agentping setup discord-bot`)",
    )
}

async fn create_interaction(
    State(state): State<AppState>,
    Json(req): Json<CreateInteraction>,
) -> Response {
    let Some(remote) = state.remote.clone() else {
        return remote_disabled();
    };
    match remote.create(req).await {
        Ok(result) => Json(result).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "could not create remote interaction");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not create the interaction",
            )
        }
    }
}

#[derive(Debug, Deserialize)]
struct WaitParams {
    wait: Option<u64>,
}

async fn wait_interaction(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(p): Query<WaitParams>,
) -> Response {
    let Some(remote) = state.remote.clone() else {
        return remote_disabled();
    };
    let wait = std::time::Duration::from_secs(p.wait.unwrap_or(0).min(55));
    match remote.wait(&id, wait).await {
        Ok(Some(s)) => Json(s).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "no such interaction"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn cancel_interaction(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(remote) = state.remote.clone() else {
        return remote_disabled();
    };
    match remote
        .cancel(&id, "Timed out — answer it in the terminal")
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn remote_status(State(state): State<AppState>) -> Response {
    match state.remote.clone() {
        Some(r) => Json(r.status().await).into_response(),
        None => Json(RemoteStatus::default()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct PresenceBody {
    away: bool,
}

async fn set_presence(State(state): State<AppState>, Json(body): Json<PresenceBody>) -> Response {
    let Some(remote) = state.remote.clone() else {
        return remote_disabled();
    };
    match remote.set_away(body.away).await {
        Ok(()) => Json(remote.status().await).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
