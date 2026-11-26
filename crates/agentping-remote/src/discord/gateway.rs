//! Discord Gateway (WebSocket) client: receives messages and button interactions in real time
//! without exposing any public endpoint. Handles heartbeats, resume and reconnects.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::api::DiscordApi;
use super::model::{Interaction, Message};

const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

/// GUILDS | GUILD_MESSAGES | DIRECT_MESSAGES | MESSAGE_CONTENT
pub const INTENTS: u64 = (1 << 0) | (1 << 9) | (1 << 12) | (1 << 15);

/// Events forwarded to the application.
#[derive(Debug, Clone)]
pub enum GatewayEvent {
    /// Connected and identified.
    Ready {
        /// Bot user id.
        bot_user_id: String,
    },
    /// A message was posted.
    MessageCreate(Box<Message>),
    /// A button or select menu was used.
    Interaction(Box<Interaction>),
    /// The gateway refused us permanently (bad token or intents not enabled).
    Fatal(String),
}

enum SessionEnd {
    /// Reconnect, resuming if possible.
    Reconnect { resumable: bool },
    /// Stop for good.
    Fatal(String),
    /// Shutdown requested.
    Shutdown,
}

struct Resume {
    session_id: String,
    url: String,
    seq: Option<u64>,
}

/// Runs the gateway connection until `shutdown` turns true or a fatal error occurs.
pub async fn run(
    api: DiscordApi,
    events: mpsc::Sender<GatewayEvent>,
    mut shutdown: watch::Receiver<bool>,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut resume: Option<Resume> = None;
    let mut backoff = Duration::from_secs(1);
    loop {
        if *shutdown.borrow() {
            return;
        }
        let url = resume
            .as_ref()
            .map(|r| format!("{}/?v=10&encoding=json", r.url.trim_end_matches('/')))
            .unwrap_or_else(|| GATEWAY_URL.to_owned());
        match session(&api, &url, &mut resume, &events, &mut shutdown).await {
            SessionEnd::Shutdown => return,
            SessionEnd::Fatal(reason) => {
                tracing::error!(%reason, "Discord gateway stopped");
                let _ = events.send(GatewayEvent::Fatal(reason)).await;
                return;
            }
            SessionEnd::Reconnect { resumable } => {
                if !resumable {
                    resume = None;
                }
                tracing::info!(
                    resumable,
                    delay_secs = backoff.as_secs(),
                    "reconnecting to Discord gateway"
                );
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = stopped(&mut shutdown) => return,
                }
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

fn fatal_close(code: u16) -> Option<String> {
    Some(match code {
        4004 => "Discord rejected the bot token (4004). Run `agentping setup discord-bot` with a valid token.".to_owned(),
        4013 | 4014 => "Discord refused the requested intents (4014). In the Developer Portal → Bot, enable \"Message Content Intent\", then `agentping daemon restart`.".to_owned(),
        4010..=4012 => format!("Discord gateway closed with unrecoverable code {code}"),
        _ => return None,
    })
}

async fn session(
    api: &DiscordApi,
    url: &str,
    resume: &mut Option<Resume>,
    events: &mpsc::Sender<GatewayEvent>,
    shutdown: &mut watch::Receiver<bool>,
) -> SessionEnd {
    let ws = match tokio_tungstenite::connect_async(url).await {
        Ok((ws, _)) => ws,
        Err(e) => {
            tracing::warn!(error = %e, "could not connect to Discord gateway");
            return SessionEnd::Reconnect { resumable: true };
        }
    };
    let (mut tx, mut rx) = ws.split();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(3600));
    heartbeat.reset();
    let mut acked = true;
    let token = api.token().expose().to_owned();
    loop {
        tokio::select! {
            _ = stopped(shutdown) => {
                let _ = tx.send(WsMessage::Close(None)).await;
                return SessionEnd::Shutdown;
            }
            _ = heartbeat.tick() => {
                if !acked {
                    tracing::warn!("Discord gateway heartbeat not acknowledged; reconnecting");
                    return SessionEnd::Reconnect { resumable: true };
                }
                acked = false;
                let seq = resume.as_ref().and_then(|r| r.seq);
                if tx.send(WsMessage::Text(json!({ "op": 1, "d": seq }).to_string().into())).await.is_err() {
                    return SessionEnd::Reconnect { resumable: true };
                }
            }
            frame = rx.next() => {
                let text = match frame {
                    Some(Ok(WsMessage::Text(t))) => t.to_string(),
                    Some(Ok(WsMessage::Close(close))) => {
                        let code = close.map(|c| u16::from(c.code)).unwrap_or(1000);
                        if let Some(reason) = fatal_close(code) {
                            return SessionEnd::Fatal(reason);
                        }
                        return SessionEnd::Reconnect { resumable: code != 4007 && code != 4009 };
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => {
                        tracing::warn!(error = %e, "Discord gateway error");
                        return SessionEnd::Reconnect { resumable: true };
                    }
                    None => return SessionEnd::Reconnect { resumable: true },
                };
                let Ok(payload) = serde_json::from_str::<Value>(&text) else { continue };
                if let Some(s) = payload.get("s").and_then(Value::as_u64) {
                    if let Some(r) = resume.as_mut() {
                        r.seq = Some(s);
                    }
                }
                match payload.get("op").and_then(Value::as_u64).unwrap_or(99) {
                    10 => {
                        let interval = payload["d"]["heartbeat_interval"].as_u64().unwrap_or(41_250);
                        let period = Duration::from_millis(interval);
                        // First beat after a random fraction of the interval, as Discord asks.
                        let jitter = period.mul_f64(rand::random::<f64>());
                        heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + jitter, period);
                        let msg = match resume.as_ref() {
                            Some(r) => json!({ "op": 6, "d": { "token": token, "session_id": r.session_id, "seq": r.seq } }),
                            None => json!({
                                "op": 2,
                                "d": {
                                    "token": token,
                                    "intents": INTENTS,
                                    "properties": { "os": std::env::consts::OS, "browser": "agentping", "device": "agentping" }
                                }
                            }),
                        };
                        if tx.send(WsMessage::Text(msg.to_string().into())).await.is_err() {
                            return SessionEnd::Reconnect { resumable: true };
                        }
                    }
                    11 => acked = true,
                    1 => {
                        let seq = resume.as_ref().and_then(|r| r.seq);
                        let _ = tx.send(WsMessage::Text(json!({ "op": 1, "d": seq }).to_string().into())).await;
                    }
                    7 => return SessionEnd::Reconnect { resumable: true },
                    9 => {
                        let resumable = payload["d"].as_bool().unwrap_or(false);
                        tokio::time::sleep(Duration::from_millis(1000 + rand::random::<u64>() % 4000)).await;
                        return SessionEnd::Reconnect { resumable };
                    }
                    0 => dispatch(&payload, resume, events).await,
                    _ => {}
                }
            }
        }
    }
}

async fn dispatch(
    payload: &Value,
    resume: &mut Option<Resume>,
    events: &mpsc::Sender<GatewayEvent>,
) {
    let kind = payload.get("t").and_then(Value::as_str).unwrap_or("");
    let d = &payload["d"];
    match kind {
        "READY" => {
            *resume = Some(Resume {
                session_id: d["session_id"].as_str().unwrap_or_default().to_owned(),
                url: d["resume_gateway_url"]
                    .as_str()
                    .unwrap_or("wss://gateway.discord.gg")
                    .to_owned(),
                seq: payload.get("s").and_then(Value::as_u64),
            });
            let bot_user_id = d["user"]["id"].as_str().unwrap_or_default().to_owned();
            tracing::info!("connected to Discord gateway");
            let _ = events.send(GatewayEvent::Ready { bot_user_id }).await;
        }
        "RESUMED" => tracing::info!("resumed Discord gateway session"),
        "MESSAGE_CREATE" => {
            if let Ok(m) = serde_json::from_value::<Message>(d.clone()) {
                let _ = events.send(GatewayEvent::MessageCreate(Box::new(m))).await;
            }
        }
        "INTERACTION_CREATE" => {
            if let Ok(i) = serde_json::from_value::<Interaction>(d.clone()) {
                let _ = events.send(GatewayEvent::Interaction(Box::new(i))).await;
            }
        }
        _ => {}
    }
}

/// Resolves once shutdown is requested (without holding the watch guard across awaits).
async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|s| *s).await;
}
