//! `agentping status` and the bare `agentping` overview.

use agentping_agents::claude::install::{inspect, settings_path, Scope};
use agentping_core::time::{ago, humanize_duration};
use agentping_core::VERSION;
use agentping_storage::{SessionRecord, Stats};
use chrono::Utc;
use serde::Serialize;

use crate::commands::daemon_cmd::{probe, DaemonProbe};
use crate::context::Ctx;
use crate::ui;

#[derive(Serialize)]
struct StatusOut {
    version: &'static str,
    config_file: String,
    config_exists: bool,
    daemon: DaemonProbe,
    providers_enabled: Vec<String>,
    claude_hooks_installed: Option<bool>,
    stats: Stats,
    sessions: Vec<SessionRecord>,
}

fn agent_label(agent: &str) -> String {
    match agent {
        "claude" => "Claude Code".into(),
        other => other.into(),
    }
}

pub fn run(ctx: &Ctx) -> anyhow::Result<()> {
    let loaded = ctx.load_config()?;
    let config = &loaded.config;
    let daemon = probe(ctx, config);
    let db = ctx.open_db(config)?;
    let now = Utc::now();
    let stats = db.store().stats(now)?;
    let sessions = db
        .store()
        .sessions(now - chrono::Duration::hours(24), false)?;
    let hooks = std::env::current_dir()
        .ok()
        .and_then(|cwd| settings_path(Scope::User, &cwd))
        .and_then(|p| inspect(&p).ok())
        .map(|s| s.is_complete());
    let providers: Vec<String> = config
        .providers
        .enabled_names()
        .into_iter()
        .map(str::to_owned)
        .collect();

    if ctx.json {
        return ui::json(&StatusOut {
            version: VERSION,
            config_file: loaded.path.display().to_string(),
            config_exists: loaded.exists,
            daemon,
            providers_enabled: providers,
            claude_hooks_installed: hooks,
            stats,
            sessions,
        });
    }

    ui::heading(&format!("AgentPing v{VERSION}"));
    ui::blank();
    match &daemon.health {
        Some(h) => {
            ui::kv("Daemon", &ui::status_word("Running", Some(true)));
            ui::kv(
                "Uptime",
                &humanize_duration(chrono::Duration::seconds(h.uptime_secs)),
            );
        }
        None => ui::kv("Daemon", &ui::status_word("Not running", None)),
    }
    ui::kv(
        "Providers",
        &if providers.is_empty() {
            ui::status_word("none enabled", Some(false))
        } else {
            providers.join(", ")
        },
    );
    ui::kv(
        "Claude hooks",
        &match hooks {
            Some(true) => ui::status_word("installed", Some(true)),
            Some(false) => ui::status_word("not installed", None),
            None => ui::status_word("unknown", None),
        },
    );
    ui::blank();
    ui::heading(&format!("Active sessions: {}", sessions.len()));
    for s in &sessions {
        ui::blank();
        ui::line(&ui::bold(&agent_label(&s.agent)));
        ui::kv("  Project", s.project_name.as_deref().unwrap_or("unknown"));
        ui::kv("  Status", s.status.label());
        ui::kv("  Last event", &ago(s.last_activity_at, now));
    }
    ui::blank();
    ui::heading("Notifications");
    ui::kv("Delivered", &stats.delivered.to_string());
    ui::kv("Pending", &stats.pending.to_string());
    ui::kv(
        "Failed",
        &if stats.failed > 0 {
            ui::status_word(&stats.failed.to_string(), Some(false))
        } else {
            "0".into()
        },
    );
    ui::kv("Events (24h)", &stats.events_24h.to_string());
    if !loaded.exists {
        ui::blank();
        ui::hint("No configuration yet; run `agentping init` to get started.");
    } else if stats.failed > 0 {
        ui::blank();
        ui::hint("See failures with `agentping events` and `agentping doctor`.");
    }
    Ok(())
}

/// Shown when `agentping` runs without a subcommand.
pub fn overview(ctx: &Ctx) -> anyhow::Result<()> {
    ui::heading(&format!("AgentPing v{VERSION}"));
    ui::line(&ui::dim(agentping_core::TAGLINE));
    ui::blank();
    let exists = ctx.paths.config_file().exists();
    if !exists {
        ui::line("Get started:");
        ui::hint("agentping init        interactive setup (Discord/Telegram + Claude Code hooks)");
        ui::hint("agentping --help      all commands");
        return Ok(());
    }
    let config = ctx.config_or_default();
    let daemon = probe(ctx, &config);
    ui::kv(
        "Daemon",
        &if daemon.running {
            ui::status_word("running", Some(true))
        } else {
            ui::status_word("not running", None)
        },
    );
    let providers = config.providers.enabled_names();
    ui::kv(
        "Providers",
        &if providers.is_empty() {
            ui::status_word("none enabled", Some(false))
        } else {
            providers.join(", ")
        },
    );
    ui::blank();
    ui::line("Common commands:");
    ui::hint("agentping status      sessions and delivery counters");
    ui::hint("agentping events      recent events");
    ui::hint("agentping test        send a test notification");
    ui::hint("agentping doctor      diagnose problems");
    Ok(())
}
