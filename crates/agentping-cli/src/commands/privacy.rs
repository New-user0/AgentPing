//! `agentping privacy`

use agentping_core::paths::display_path;
use chrono::Utc;

use crate::context::Ctx;
use crate::ui;

pub fn run(ctx: &Ctx) -> anyhow::Result<()> {
    let config = ctx.config_or_default();
    let n = &config.notifications;
    let db_path = config.database_path(&ctx.paths);
    let events = ctx
        .open_db(&config)
        .and_then(|db| Ok(db.store().stats(Utc::now())?.events))
        .unwrap_or(0);

    if ctx.json {
        return ui::json(&serde_json::json!({
            "telemetry": false,
            "database": db_path,
            "stored_events": events,
            "retention_days": config.general.retention_days,
            "include_message": n.include_message,
            "include_project_path": n.include_project_path,
            "max_message_chars": n.max_message_chars,
            "providers_enabled": config.providers.enabled_names(),
        }));
    }

    ui::heading("Privacy");
    ui::ok(
        "No telemetry, analytics or update checks. AgentPing only talks to providers you enable.",
    );
    ui::blank();
    ui::heading("Stored locally");
    ui::kv("Database", &display_path(&db_path));
    ui::kv("Events stored", &events.to_string());
    ui::kv(
        "Retention",
        &format!("{} days", config.general.retention_days),
    );
    ui::line(
        "  Events keep: type, agent, session id, project name/path, the agent's notification text,",
    );
    ui::line("  Claude's final reply (for completed tasks) and the questions it asks. Prompts you type are");
    ui::line("  never captured. Secrets live in the OS credential store, never in the database.");
    ui::blank();
    ui::heading("Sent to notification providers");
    let enabled = config.providers.enabled_names();
    ui::kv(
        "Providers",
        &if enabled.is_empty() {
            "none".into()
        } else {
            enabled.join(", ")
        },
    );
    ui::kv("Title, event type, agent, project name", "yes");
    ui::kv(
        "Message text",
        &if n.include_message {
            format!(
                "yes (redacted for common secret formats, max {} chars)",
                n.max_message_chars
            )
        } else {
            "no".into()
        },
    );
    ui::kv(
        "Full project paths",
        if n.include_project_path { "yes" } else { "no" },
    );
    ui::kv("Session id", "first 8 characters only");
    ui::blank();
    ui::heading("Controls");
    ui::hint("agentping config set notifications.include_message false");
    ui::hint("agentping config set general.retention_days 7");
    ui::hint("agentping events clear");
    Ok(())
}
