//! `agentping hook <agent>`: receives a native hook payload on stdin.
//!
//! Contract with the agent: write nothing to stdout or stderr and exit 0, quickly. Events go to
//! the daemon over loopback; when it is unreachable they are stored directly in SQLite and a
//! short-lived background sender is spawned.
//!
//! The only exception is remote interaction (disabled by default): when the user answers a
//! permission request or question from Discord, the hook prints the documented decision JSON on
//! stdout; when they reply to a finished session, the `--wait-reply` hook (registered with
//! `asyncRewake`) prints the reply on stderr and exits 2 so Claude wakes up with it.

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use agentping_agents::claude::{hook_output, remote_candidate, ClaudeAdapter, HookOutput};
use agentping_agents::{AdapterContext, AgentAdapter, MAX_PAYLOAD_BYTES};
use agentping_core::remote::{CreateInteraction, InteractionRequest};
use agentping_core::{Config, Event, Paths};
use agentping_daemon::engine::Engine;
use agentping_daemon::process::spawn_detached;
use agentping_daemon::{ClientError, DaemonClient};
use chrono::Utc;

use crate::logging;

/// Runs the hook and returns the process exit code. Failures are logged and swallowed.
pub fn run(agent: &str, wait_reply: bool) -> i32 {
    let Ok(paths) = Paths::discover() else {
        return 0;
    };
    let loaded = Config::load(&paths);
    let level = loaded
        .as_ref()
        .map(|l| l.config.general.log_level.clone())
        .unwrap_or_else(|_| "info".into());
    logging::init(&paths, &level, logging::Mode::FileOnly);
    let config = match loaded {
        Ok(l) => l.config,
        Err(e) => {
            tracing::error!(error = %e, "configuration is invalid; using defaults for this event");
            Config::default()
        }
    };
    match handle(agent, wait_reply, &paths, &config) {
        Ok(code) => code,
        Err(e) => {
            tracing::error!(agent, error = %e, "hook failed");
            0
        }
    }
}

fn read_stdin() -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::io::stdin()
        .take(MAX_PAYLOAD_BYTES as u64 + 1)
        .read_to_end(&mut buf)?;
    Ok(buf)
}

fn handle(agent: &str, wait_reply: bool, paths: &Paths, config: &Config) -> anyhow::Result<i32> {
    let adapter: Box<dyn AgentAdapter> = match agent {
        "claude" | "claude-code" => Box::new(ClaudeAdapter::new(config.agents.claude.clone())),
        other => anyhow::bail!("unknown agent integration '{other}'"),
    };
    let payload = read_stdin()?;
    let ctx = AdapterContext {
        received_at: Utc::now(),
        project_dir: std::env::var_os("CLAUDE_PROJECT_DIR").map(PathBuf::from),
    };
    let event = match adapter.parse(&payload, &ctx) {
        Ok(Some(e)) => e,
        Ok(None) => return Ok(0),
        Err(e) => {
            // The payload may contain sensitive data; only the error kind is logged.
            tracing::warn!(agent, payload_bytes = payload.len(), error = %e, "ignoring unparseable hook payload");
            return Ok(0);
        }
    };
    event.validate()?;
    tracing::debug!(event_type = %event.event_type, wait_reply, "hook event received");

    // The reply waiter duplicates the Stop hook's payload; the plain hook records the event.
    if !wait_reply && !send_to_daemon(paths, config, &event) {
        store_locally(paths, config, &event)?;
    }
    if !config.remote.enabled {
        return Ok(0);
    }
    let Ok(raw) = serde_json::from_slice::<serde_json::Value>(&payload) else {
        return Ok(0);
    };
    let Some(candidate) = remote_candidate(&raw) else {
        return Ok(0);
    };
    let is_continue = matches!(candidate.request, InteractionRequest::Continue { .. });
    let wanted = match &candidate.request {
        InteractionRequest::Permission { .. } => config.remote.permissions,
        InteractionRequest::Question { .. } => config.remote.questions,
        InteractionRequest::Continue { .. } => config.remote.continue_replies,
    };
    if !wanted || is_continue != wait_reply {
        return Ok(0);
    }
    // A headless run (`claude -p`, SDK) would stay alive while the reply listener waits, so
    // reply-to-continue only applies to interactive sessions. Approvals and answers still work.
    if is_continue && headless_session() {
        tracing::debug!("headless session: not waiting for a remote reply");
        return Ok(0);
    }
    let timeout_minutes = if is_continue {
        config.remote.reply_timeout_minutes
    } else {
        config.remote.decision_timeout_minutes
    };
    let request = CreateInteraction {
        agent: event.agent.as_str().to_owned(),
        agent_name: event.agent_name.clone(),
        session_id: event.session_id.clone(),
        project_name: event.project_name.clone(),
        project_path: event.project_path.clone(),
        request: candidate.request,
        timeout_secs: u64::from(timeout_minutes) * 60,
        supersedes_cancel_key: candidate.supersedes_cancel_key,
    };
    let Some((response, responder)) = wait_for_remote(paths, config, &request)? else {
        return Ok(0);
    };
    Ok(match hook_output(&raw, &response, &responder) {
        Some(HookOutput::Json(v)) => {
            println!("{v}");
            0
        }
        Some(HookOutput::Wake(text)) => {
            eprintln!("{text}");
            2
        }
        None => 0,
    })
}

/// Registers the interaction with the daemon and waits for the remote answer.
fn wait_for_remote(
    paths: &Paths,
    config: &Config,
    request: &CreateInteraction,
) -> anyhow::Result<Option<(agentping_core::remote::InteractionResponse, String)>> {
    let client = DaemonClient::new(paths, config, Duration::from_secs(70))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let created = match client.create_interaction(request).await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, "remote interaction unavailable");
                return Ok(None);
            }
        };
        let Some(id) = created.id.filter(|_| created.routed) else {
            tracing::debug!(reason = ?created.reason, "prompt not routed to Discord");
            return Ok(None);
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(request.timeout_secs);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                let _ = client.cancel_interaction(&id).await;
                return Ok(None);
            }
            match client.wait_interaction(&id, remaining.as_secs().clamp(1, 50)).await {
                Ok(state) if state.status == "pending" => {}
                Ok(state) if state.status == "resolved" => {
                    tracing::info!(interaction = %id, "applying remote answer");
                    return Ok(state.response.map(|r| (r, state.responder.unwrap_or_default())));
                }
                Ok(state) => {
                    tracing::debug!(interaction = %id, status = %state.status, "remote prompt closed");
                    return Ok(None);
                }
                Err(ClientError::Rejected { status: 404, .. }) => return Ok(None),
                Err(e) => {
                    tracing::debug!(error = %e, "waiting for the daemon");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    })
}

fn send_to_daemon(paths: &Paths, config: &Config, event: &Event) -> bool {
    let Ok(client) = DaemonClient::new(paths, config, Duration::from_millis(1500)) else {
        return false;
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    match rt.block_on(client.send_event(event)) {
        Ok(_) => true,
        Err(e) => {
            tracing::debug!(error = %e, "daemon unavailable; storing event locally");
            false
        }
    }
}

fn store_locally(paths: &Paths, config: &Config, event: &Event) -> anyhow::Result<()> {
    paths.ensure_dirs()?;
    let mut db = agentping_storage::Database::open(&config.database_path(paths))?;
    let outcome = Engine::new(config.clone())?.ingest(&mut db, event, Utc::now())?;
    drop(db);
    if outcome.queued() > 0 && config.daemon.spawn_delivery_when_offline {
        let exe = std::env::current_exe()?;
        match spawn_detached(&exe, &["deliver"]) {
            Ok(pid) => tracing::debug!(pid, "spawned background delivery"),
            Err(e) => tracing::error!(error = %e, "could not spawn background delivery"),
        }
    }
    Ok(())
}

/// Whether the hook runs under a non-interactive Claude Code session (`-p` / SDK).
fn headless_session() -> bool {
    let entry = std::env::var("CLAUDE_CODE_ENTRYPOINT").unwrap_or_default();
    let attended = std::env::var("CLAUDE_CODE_SESSION_ATTENDED").unwrap_or_default();
    entry.starts_with("sdk") || attended == "0"
}
