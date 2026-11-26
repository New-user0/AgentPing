//! `agentping setup discord-bot`, `agentping away`, `agentping back`.

use std::time::{Duration, Instant};

use agentping_agents::claude::install::{inspect, managed_hooks, settings_path, Scope};
use agentping_core::secrets::SecretKey;
use agentping_daemon::DaemonClient;
use agentping_notifiers::http_client;
use agentping_remote::discord::DiscordApi;
use anyhow::{bail, Context as _};
use serde_json::{json, Value};

use crate::cli::SetupDiscordBotArgs;
use crate::commands::{daemon_cmd, setup};
use crate::context::Ctx;
use crate::ui;

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

fn is_snowflake(s: &str) -> bool {
    (15..=22).contains(&s.len()) && s.chars().all(|c| c.is_ascii_digit())
}

/// The channel the notification webhook posts to (so prompts land in the same place).
async fn webhook_channel(ctx: &Ctx) -> Option<String> {
    let (url, _) = ctx.secrets().get(SecretKey::DiscordWebhookUrl).ok()??;
    let valid = agentping_notifiers::discord::validate_webhook_url(url.expose()).ok()?;
    let v: Value = http_client()
        .ok()?
        .get(valid)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v.get("channel_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub fn setup_discord_bot(ctx: &Ctx, args: &SetupDiscordBotArgs) -> anyhow::Result<()> {
    if args.disable {
        ctx.set_config_values(&[("remote.enabled", "false")])?;
        ctx.secrets().delete(SecretKey::DiscordBotToken)?;
        ui::ok("Remote interaction disabled and the bot token deleted");
        reinstall_hooks_if_present(ctx)?;
        restart_daemon_if_running(ctx);
        return Ok(());
    }
    ui::heading("Discord two-way setup");
    if args.input.from_env.is_none() && !args.input.stdin {
        ui::line(
            "You need a Discord bot (Developer Portal → New Application → Bot → Reset Token),",
        );
        ui::line(
            "with \"Message Content Intent\" enabled, invited to your server with View Channels,",
        );
        ui::line("Send Messages, Read Message History and Embed Links.");
    }
    let token = setup::obtain_secret_public(&args.input, "Discord bot token")?;
    let rt = runtime()?;
    let api = DiscordApi::new(http_client()?, token.clone());
    let me = rt.block_on(api.me()).map_err(|e| anyhow::anyhow!("{e}"))?;
    ui::ok(&format!("Bot token valid: {}", me.display()));

    let config = ctx.config_or_default();
    let channel_id = match args
        .channel_id
        .clone()
        .or(config.remote.discord_channel_id.clone())
    {
        Some(c) => c,
        None => match rt.block_on(webhook_channel(ctx)) {
            Some(c) => {
                ui::ok("Using the channel your notification webhook posts to");
                c
            }
            None => bail!("pass --channel-id <id> (right-click the channel → Copy Channel ID, with Developer Mode on)"),
        },
    };
    if !is_snowflake(&channel_id) {
        bail!("'{channel_id}' is not a Discord channel id");
    }
    let channel = rt
        .block_on(api.channel(&channel_id))
        .map_err(|e| anyhow::anyhow!("cannot access channel {channel_id}: {e}"))?;
    ui::ok(&format!(
        "Bot can see #{}",
        channel.name.as_deref().unwrap_or(&channel_id)
    ));

    let user_id = match &args.user_id {
        Some(u) if is_snowflake(u) => u.clone(),
        Some(u) => bail!("'{u}' is not a Discord user id"),
        None => rt.block_on(pair(&api, &channel_id))?,
    };

    ctx.paths.ensure_dirs()?;
    let source = ctx.secrets().set(
        SecretKey::DiscordBotToken,
        &token,
        args.input.insecure_store,
    )?;
    ui::ok(&format!("Bot token saved to the {source}"));
    let mut users = config.remote.discord_user_ids.clone();
    if !users.contains(&user_id) {
        users.push(user_id.clone());
    }
    let users_toml = format!(
        "[{}]",
        users
            .iter()
            .map(|u| format!("\"{u}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let channel_toml = format!("\"{channel_id}\"");
    ctx.set_config_values(&[
        ("remote.discord_channel_id", channel_toml.as_str()),
        ("remote.discord_user_ids", users_toml.as_str()),
        ("remote.enabled", "true"),
    ])?;
    ui::ok("Remote interaction enabled");
    reinstall_hooks_if_present(ctx)?;
    restart_daemon_if_running(ctx);
    let _ = rt.block_on(api.send_message(
        &channel_id,
        &json!({
            "content": "✅ **AgentPing two-way control is on.** I'll post permission requests and questions here while you're away, and you can reply to a finished/waiting ping to give Claude new instructions. Send `help` for commands.",
            "allowed_mentions": { "parse": [] }
        }),
    ));
    ui::blank();
    ui::line("Try it: start Claude Code, run `agentping away`, and ask Claude to run a command.");
    ui::hint("The daemon must be running for two-way control: `agentping daemon start` (or `daemon install`).");
    Ok(())
}

/// Pairs the Discord account that types a one-time code in the channel.
async fn pair(api: &DiscordApi, channel_id: &str) -> anyhow::Result<String> {
    let code = format!("AP-{:06}", rand_code());
    ui::blank();
    ui::heading("Pair your Discord account");
    ui::line(&format!(
        "Send this message in the channel now:  {}",
        ui::bold(&code)
    ));
    ui::line(&ui::dim("Only the account that sends it will be allowed to answer prompts. Waiting up to 3 minutes…"));
    let started = Instant::now();
    let mut warned = false;
    while started.elapsed() < Duration::from_secs(180) {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let messages = api
            .recent_messages(channel_id, 20)
            .await
            .map_err(|e| anyhow::anyhow!("could not read the channel: {e}"))?;
        if let Some(m) = messages
            .iter()
            .find(|m| !m.author.bot && m.content.trim().eq_ignore_ascii_case(&code))
        {
            ui::ok(&format!(
                "Paired with {} ({})",
                m.author.display(),
                m.author.id
            ));
            let _ = api.react(channel_id, &m.id, "✅").await;
            return Ok(m.author.id.clone());
        }
        let blank = messages
            .iter()
            .any(|m| !m.author.bot && m.webhook_id.is_none() && m.content.is_empty());
        if blank && !warned {
            warned = true;
            ui::warn("Messages arrive without text: enable \"Message Content Intent\" for the bot in the Developer Portal (Bot tab), then send the code again.");
        }
    }
    bail!("pairing timed out; run `agentping setup discord-bot` again (or pass --user-id)")
}

fn rand_code() -> u32 {
    rand::random::<u32>() % 1_000_000
}

/// Re-installs Claude Code hooks (user scope) when they are present, so timeouts and the
/// reply waiter match the new remote settings.
fn reinstall_hooks_if_present(ctx: &Ctx) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let Some(path) = settings_path(Scope::User, &cwd) else {
        return Ok(());
    };
    let config = ctx.config_or_default();
    let any = inspect(&path, &managed_hooks(&config.remote))
        .map(|s| !s.installed.is_empty())
        .unwrap_or(false);
    if any {
        setup::install_claude_hooks(ctx, Scope::User, false)?;
    } else {
        ui::hint("Install the Claude Code hooks with `agentping setup claude`.");
    }
    Ok(())
}

fn restart_daemon_if_running(ctx: &Ctx) {
    let config = ctx.config_or_default();
    if daemon_cmd::probe(ctx, &config).running {
        if let Err(e) = daemon_cmd::stop(ctx).and_then(|_| daemon_cmd::start(ctx)) {
            ui::warn(&format!("Could not restart the daemon: {e:#}"));
        }
    } else {
        ui::hint("Start the daemon so it can talk to Discord: `agentping daemon start`");
    }
}

pub fn presence(ctx: &Ctx, away: bool) -> anyhow::Result<()> {
    let config = ctx.load_config()?.config;
    if !config.remote.enabled {
        bail!("remote interaction is not enabled; run `agentping setup discord-bot`");
    }
    let client = DaemonClient::new(&ctx.paths, &config, Duration::from_secs(5))?;
    let status = runtime()?
        .block_on(client.set_presence(away))
        .context("the daemon must be running (`agentping daemon start`)")?;
    if ctx.json {
        return ui::json(&status);
    }
    if away {
        ui::ok("Away: permission requests and questions will be sent to Discord");
    } else {
        ui::ok("Back: prompts stay in the terminal");
        if config.remote.away_after_minutes > 0 {
            ui::hint(&format!(
                "You'll be considered away again after {} minutes without typing a prompt.",
                config.remote.away_after_minutes
            ));
        }
    }
    Ok(())
}
