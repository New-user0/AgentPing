//! `agentping providers`

use agentping_core::secrets::SecretKey;
use agentping_notifiers::{build_provider, http_client};
use serde::Serialize;

use crate::cli::ProvidersArgs;
use crate::commands::setup::title_case;
use crate::context::Ctx;
use crate::ui;

#[derive(Serialize)]
struct ProviderRow {
    name: &'static str,
    enabled: bool,
    implemented: bool,
    credential: Option<String>,
    ready: bool,
    detail: String,
    healthy: Option<bool>,
}

pub fn run(ctx: &Ctx, args: &ProvidersArgs) -> anyhow::Result<()> {
    let config = ctx.load_config()?.config;
    let secrets = ctx.secrets();
    let http = http_client()?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut rows = Vec::new();
    for (name, key, enabled) in [
        (
            "discord",
            Some(SecretKey::DiscordWebhookUrl),
            config.providers.discord.enabled,
        ),
        (
            "telegram",
            Some(SecretKey::TelegramBotToken),
            config.providers.telegram.enabled,
        ),
        ("whatsapp", None, config.providers.whatsapp.enabled),
    ] {
        let credential = match key {
            Some(k) => secrets.get(k)?.map(|(_, src)| src.to_string()),
            None => None,
        };
        let (ready, mut detail, provider) = match key {
            None => (
                false,
                "not implemented yet (see `agentping setup whatsapp`)".to_owned(),
                None,
            ),
            Some(_) => match build_provider(name, &config, &secrets, &http) {
                Ok(p) => (true, "configured".to_owned(), Some(p)),
                Err(e) => (false, e, None),
            },
        };
        let mut healthy = None;
        if args.check && enabled {
            if let Some(p) = provider {
                let h = rt.block_on(p.health_check());
                healthy = Some(h.healthy);
                detail = h.detail;
            }
        }
        rows.push(ProviderRow {
            name,
            enabled,
            implemented: key.is_some(),
            credential,
            ready,
            detail,
            healthy,
        });
    }
    if ctx.json {
        return ui::json(&rows);
    }
    ui::heading("Notification providers");
    for r in &rows {
        let state = match (r.enabled, r.ready, r.healthy) {
            (_, _, Some(false)) => ui::status_word("unreachable", Some(false)),
            (true, true, _) => ui::status_word("enabled", Some(true)),
            (true, false, _) => ui::status_word("enabled but not ready", Some(false)),
            (false, _, _) => ui::dim("disabled"),
        };
        ui::line(&format!("  {:<10} {state}", title_case(r.name)));
        if let Some(c) = &r.credential {
            ui::hint(&format!("credential: {c}"));
        }
        if r.enabled || !r.implemented {
            ui::hint(&r.detail);
        }
    }
    if !args.check {
        ui::blank();
        ui::line(&ui::dim("Add --check to test connectivity."));
    }
    Ok(())
}
