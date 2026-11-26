//! `agentping events …`

use agentping_core::time::ago;
use agentping_notifiers::render::truncate;
use agentping_storage::{EventQuery, NotificationStatus, StoredEvent};
use chrono::{Local, TimeZone, Utc};

use crate::cli::{EventsAction, EventsArgs};
use crate::context::Ctx;
use crate::ui;

pub fn run(ctx: &Ctx, args: &EventsArgs) -> anyhow::Result<()> {
    match &args.action {
        Some(EventsAction::Show { id }) => show(ctx, id),
        Some(EventsAction::Clear { yes }) => clear(ctx, *yes),
        None => list(ctx, args),
    }
}

fn delivery_summary(e: &StoredEvent) -> String {
    if e.deliveries.is_empty() {
        return match e.decision.as_deref() {
            Some(d) if d.starts_with("muted") => ui::dim("muted"),
            Some(_) => ui::dim("—"),
            None => ui::status_word("unprocessed", None),
        };
    }
    e.deliveries
        .iter()
        .map(|d| {
            let good = match d.status {
                NotificationStatus::Delivered => Some(true),
                NotificationStatus::Failed => Some(false),
                NotificationStatus::Cancelled => {
                    return ui::dim(&format!("{} cancelled", d.provider));
                }
                _ => None,
            };
            ui::status_word(&format!("{} {}", d.provider, d.status), good)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn list(ctx: &Ctx, args: &EventsArgs) -> anyhow::Result<()> {
    let config = ctx.load_config()?.config;
    let db = ctx.open_db(&config)?;
    let since = if args.today {
        let midnight = Local::now().date_naive().and_hms_opt(0, 0, 0);
        midnight
            .and_then(|m| Local.from_local_datetime(&m).earliest())
            .map(|t| t.with_timezone(&Utc))
    } else {
        args.since.map(|d| Utc::now() - d)
    };
    let limit = args.limit.clamp(1, 1000);
    let query = EventQuery {
        since,
        agent: args.agent.clone(),
        project: args.project.clone(),
        event_type: args.event_type.clone(),
        session: args.session.clone(),
        limit: Some(limit),
        offset: Some(args.page.saturating_sub(1) * limit),
    };
    let events = db.store().list_events(&query)?;
    if ctx.json {
        return ui::json(&events);
    }
    if events.is_empty() {
        ui::info("No events match.");
        if args.page > 1 {
            ui::hint("Try a lower --page.");
        }
        return Ok(());
    }
    let now = Utc::now();
    for e in &events {
        let ev = &e.event;
        let when = e.received_at.with_timezone(&Local);
        let when = if (now - e.received_at).num_hours() < 20 {
            when.format("%H:%M:%S").to_string()
        } else {
            when.format("%b %d %H:%M").to_string()
        };
        let id_short: String = ev.id.to_string().chars().take(13).collect();
        ui::line(&format!(
            "{}  {}  {}  {}",
            ui::dim(&when),
            ui::bold(&format!("{:<18}", ev.event_type.as_str())),
            ev.project_name.as_deref().unwrap_or("-"),
            delivery_summary(e),
        ));
        let detail = ev
            .message
            .as_deref()
            .or(ev.title.as_deref())
            .map(|m| truncate(&m.replace(['\n', '\r'], " "), 100))
            .unwrap_or_default();
        ui::line(&format!(
            "  {} {}",
            ui::dim(&format!("{id_short} · {} ·", ev.agent_name)),
            ui::dim(&detail)
        ));
    }
    ui::blank();
    ui::line(&ui::dim(&format!(
        "Showing {} event(s), page {}. Details: agentping events show <id>",
        events.len(),
        args.page
    )));
    Ok(())
}

fn show(ctx: &Ctx, id: &str) -> anyhow::Result<()> {
    let config = ctx.load_config()?.config;
    let db = ctx.open_db(&config)?;
    let Some(stored) = db.store().find_event(id)? else {
        anyhow::bail!("no unique event matches '{id}' (use at least 4 characters of the id)");
    };
    let mut attempts = Vec::new();
    for d in &stored.deliveries {
        attempts.push((
            d.provider.clone(),
            db.store().attempts_for_notification(&d.id)?,
        ));
    }
    if ctx.json {
        #[derive(serde::Serialize)]
        struct Out<'a> {
            #[serde(flatten)]
            event: &'a StoredEvent,
            attempts: &'a [(String, Vec<agentping_storage::DeliveryAttemptRecord>)],
        }
        return ui::json(&Out {
            event: &stored,
            attempts: &attempts,
        });
    }
    let e = &stored.event;
    let now = Utc::now();
    ui::heading(&format!("{} · {}", e.event_type.label(), e.agent_name));
    ui::kv("Id", &e.id.to_string());
    ui::kv("Type", e.event_type.as_str());
    ui::kv("Severity", e.severity.as_str());
    ui::kv("Project", e.project_name.as_deref().unwrap_or("-"));
    if let Some(p) = &e.project_path {
        ui::kv("Path", p);
    }
    ui::kv("Session", e.session_id.as_deref().unwrap_or("-"));
    ui::kv(
        "Received",
        &format!(
            "{} ({})",
            stored
                .received_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S"),
            ago(stored.received_at, now)
        ),
    );
    if let Some(native) = &e.source.native_event {
        let sub = e
            .source
            .subtype
            .as_deref()
            .map(|s| format!(" / {s}"))
            .unwrap_or_default();
        ui::kv("Source", &format!("{} {native}{sub}", e.source.integration));
    }
    if let Some(t) = &e.title {
        ui::kv("Title", t);
    }
    if let Some(m) = &e.message {
        ui::kv("Message", &truncate(m, 2000));
    }
    ui::kv(
        "Decision",
        stored.decision.as_deref().unwrap_or("not processed yet"),
    );
    if !stored.deliveries.is_empty() {
        ui::blank();
        ui::heading("Deliveries");
        for (d, (_, tries)) in stored.deliveries.iter().zip(&attempts) {
            let good = match d.status {
                NotificationStatus::Delivered => Some(true),
                NotificationStatus::Failed => Some(false),
                _ => None,
            };
            ui::line(&format!(
                "  {} {} ({} attempt{})",
                d.provider,
                ui::status_word(d.status.as_str(), good),
                d.attempts,
                if d.attempts == 1 { "" } else { "s" }
            ));
            if let Some(next) = d
                .next_attempt_at
                .filter(|_| d.status == NotificationStatus::Pending)
            {
                ui::hint(&format!(
                    "next attempt at {}",
                    next.with_timezone(&Local).format("%H:%M:%S")
                ));
            }
            for a in tries {
                let detail = a
                    .error
                    .as_deref()
                    .map(|e| format!(" — {e}"))
                    .unwrap_or_default();
                let status = a
                    .http_status
                    .map(|s| format!(" HTTP {s}"))
                    .unwrap_or_default();
                ui::hint(&format!(
                    "{} {}{status} in {}ms{detail}",
                    a.attempted_at.with_timezone(&Local).format("%H:%M:%S"),
                    a.outcome,
                    a.duration_ms
                ));
            }
            if d.status == NotificationStatus::Cancelled {
                if let Some(reason) = &d.last_error {
                    ui::hint(reason);
                }
            }
        }
    }
    Ok(())
}

fn clear(ctx: &Ctx, yes: bool) -> anyhow::Result<()> {
    let config = ctx.load_config()?.config;
    let mut db = ctx.open_db(&config)?;
    let count = db.store().stats(Utc::now())?.events;
    if count == 0 {
        ui::info("Event history is already empty.");
        return Ok(());
    }
    if !yes {
        if !ui::interactive() {
            anyhow::bail!("refusing to delete history without confirmation; pass --yes");
        }
        let confirmed = dialoguer::Confirm::new()
            .with_prompt(format!(
                "Delete all {count} events, sessions and delivery history? This cannot be undone"
            ))
            .default(false)
            .interact()?;
        if !confirmed {
            ui::info("Nothing deleted.");
            return Ok(());
        }
    }
    let removed = db.write(|s| s.clear_all())?;
    db.vacuum()?;
    ui::ok(&format!(
        "Deleted {removed} events and all delivery history"
    ));
    Ok(())
}
