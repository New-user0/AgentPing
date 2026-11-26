//! `agentping rules test`

use agentping_core::config::parse_hhmm;
use agentping_core::{AgentKind, Event, EventType, Severity};
use agentping_rules::{RuleContext, RulesEngine};
use chrono::{Local, Utc};

use crate::cli::{RulesCommand, RulesTestArgs};
use crate::context::Ctx;
use crate::ui;

pub fn run(ctx: &Ctx, cmd: &RulesCommand) -> anyhow::Result<()> {
    match cmd {
        RulesCommand::Test(args) => test(ctx, args),
    }
}

fn test(ctx: &Ctx, args: &RulesTestArgs) -> anyhow::Result<()> {
    let config = ctx.load_config()?.config;
    if !EventType::is_builtin_name(&args.event_type) {
        anyhow::bail!(
            "unknown event type '{}' (known: {})",
            args.event_type,
            agentping_core::config::builtin_event_type_names().join(", ")
        );
    }
    let event_type: EventType = args.event_type.parse().unwrap_or(EventType::Notification);
    let agent: AgentKind = args.agent.parse().unwrap_or(AgentKind::Claude);
    let mut event = Event::new(event_type, agent, Utc::now());
    event.project_name = args.project.clone();
    event.project_path = args.path.clone();
    event.session_id = Some("rules-test".into());
    if let Some(s) = &args.severity {
        event.severity = s.parse::<Severity>().map_err(anyhow::Error::msg)?;
    }
    let now_local = match &args.time {
        Some(t) => parse_hhmm(t).ok_or_else(|| anyhow::anyhow!("--time must look like 22:30"))?,
        None => Local::now().time(),
    };
    let engine = RulesEngine::new(&config)?;
    let decision = engine.evaluate(
        &event,
        &RuleContext {
            now_local,
            recently_notified: false,
        },
    );
    if ctx.json {
        return ui::json(&decision);
    }
    if decision.notify {
        ui::ok(&format!(
            "Would notify via {} (severity {})",
            decision.providers.join(", "),
            decision.severity
        ));
    } else {
        ui::warn("Would not notify");
    }
    ui::kv("Reason", &decision.reason);
    Ok(())
}
