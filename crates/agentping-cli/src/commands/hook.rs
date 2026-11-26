//! `agentping hook <agent>`: receives a native hook payload on stdin.
//!
//! Contract with the agent: never write to stdout or stderr, always exit 0, finish fast.
//! Events go to the daemon over loopback; when it is unreachable they are stored directly in
//! SQLite and a short-lived background sender is spawned.

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use agentping_agents::claude::ClaudeAdapter;
use agentping_agents::{AdapterContext, AgentAdapter, MAX_PAYLOAD_BYTES};
use agentping_core::{Config, Event, Paths};
use agentping_daemon::engine::Engine;
use agentping_daemon::process::spawn_detached;
use agentping_daemon::DaemonClient;
use chrono::Utc;

use crate::logging;

/// Runs the hook. Every failure is logged and swallowed.
pub fn run(agent: &str) {
    let Ok(paths) = Paths::discover() else {
        return;
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
    if let Err(e) = handle(agent, &paths, &config) {
        tracing::error!(agent, error = %e, "hook failed");
    }
}

fn read_stdin() -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::io::stdin()
        .take(MAX_PAYLOAD_BYTES as u64 + 1)
        .read_to_end(&mut buf)?;
    Ok(buf)
}

fn handle(agent: &str, paths: &Paths, config: &Config) -> anyhow::Result<()> {
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
        Ok(None) => return Ok(()),
        Err(e) => {
            // The payload may contain sensitive data; only the error kind is logged.
            tracing::warn!(agent, payload_bytes = payload.len(), error = %e, "ignoring unparseable hook payload");
            return Ok(());
        }
    };
    event.validate()?;
    tracing::debug!(event_type = %event.event_type, "hook event received");

    if send_to_daemon(paths, config, &event) {
        return Ok(());
    }
    store_locally(paths, config, &event)
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
