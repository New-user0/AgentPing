//! `agentping setup …`: Claude Code hooks and notification providers.

use std::io::BufRead;

use agentping_agents::claude::install::{
    detect_shell, managed_hooks, plan_install, plan_uninstall, settings_path, HookShell, Scope,
};
use agentping_core::paths::display_path;
use agentping_core::secrets::{Secret, SecretKey, SecretSource};
use agentping_notifiers::discord::validate_webhook_url;
use agentping_notifiers::telegram::{validate_bot_token, TelegramProvider};
use agentping_notifiers::{build_provider, http_client, render};
use anyhow::{bail, Context as _};

use crate::cli::{HookScope, SecretInput, SetupClaudeArgs, SetupDiscordArgs, SetupTelegramArgs};
use crate::context::{is_dev_build, Ctx};
use crate::ui;

impl From<HookScope> for Scope {
    fn from(s: HookScope) -> Self {
        match s {
            HookScope::User => Scope::User,
            HookScope::Project => Scope::Project,
            HookScope::Local => Scope::Local,
        }
    }
}

// ---------------------------------------------------------------- Claude Code

pub fn claude(ctx: &Ctx, args: &SetupClaudeArgs) -> anyhow::Result<()> {
    if args.remove {
        return remove_claude_hooks(args.scope.into(), args.dry_run).map(|_| ());
    }
    install_claude_hooks(ctx, args.scope.into(), args.dry_run)
}

/// Installs hooks into the settings file for `scope`.
pub fn install_claude_hooks(ctx: &Ctx, scope: Scope, dry_run: bool) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let path = settings_path(scope, &cwd)
        .context("could not locate the Claude Code settings directory")?;
    let exe = ctx.current_exe()?;
    if is_dev_build(&exe) {
        ui::warn(&format!(
            "{} is a development build inside a Cargo target directory.",
            display_path(&exe)
        ));
        ui::hint("Hooks will break if you `cargo clean`. Prefer `cargo install --path crates/agentping-cli` first.");
    }
    let shell = detect_shell();
    let remote = ctx.config_or_default().remote;
    let hooks = managed_hooks(&remote);
    let change = plan_install(&path, &exe, shell, &hooks)?;
    let shell_label = match shell {
        HookShell::Posix => "sh",
        HookShell::GitBash => "Git Bash",
        HookShell::PowerShell => "PowerShell",
    };
    if dry_run {
        ui::heading("Dry run: Claude Code hooks");
        ui::kv("Settings file", &display_path(&path));
        ui::kv(
            "Command",
            &format!("{} hook claude ({shell_label})", display_path(&exe)),
        );
        for h in &hooks {
            ui::info(&format!("{} (timeout {}s)", h.label(), h.timeout));
        }
        if change.changed {
            ui::line(&ui::dim("Resulting settings:"));
            ui::line(&serde_json::to_string_pretty(&change.after)?);
        } else {
            ui::ok("Already installed; nothing would change.");
        }
        return Ok(());
    }
    if !change.changed {
        ui::ok(&format!(
            "Claude Code hooks are already installed in {}",
            display_path(&path)
        ));
        return Ok(());
    }
    let backup = change.apply()?;
    ui::ok(&format!(
        "Installed {} Claude Code hooks in {}",
        hooks.len(),
        display_path(&path)
    ));
    if let Some(b) = backup {
        ui::hint(&format!(
            "Backup of previous settings: {}",
            display_path(&b)
        ));
    }
    ui::hint(&format!(
        "Hooks run `agentping hook claude` via {shell_label}."
    ));
    ui::hint("Restart running Claude Code sessions so they pick up the new hooks.");
    Ok(())
}

/// Removes hooks from the settings file for `scope`. Returns whether anything changed.
pub fn remove_claude_hooks(scope: Scope, dry_run: bool) -> anyhow::Result<bool> {
    let cwd = std::env::current_dir()?;
    let path = settings_path(scope, &cwd)
        .context("could not locate the Claude Code settings directory")?;
    if !path.exists() {
        ui::info(&format!(
            "{} does not exist; nothing to remove",
            display_path(&path)
        ));
        return Ok(false);
    }
    let change = plan_uninstall(&path)?;
    if !change.changed {
        ui::info(&format!("No AgentPing hooks in {}", display_path(&path)));
        return Ok(false);
    }
    if dry_run {
        ui::info(&format!(
            "Would remove AgentPing hooks from {}",
            display_path(&path)
        ));
        return Ok(true);
    }
    let backup = change.apply()?;
    ui::ok(&format!(
        "Removed AgentPing hooks from {}",
        display_path(&path)
    ));
    if let Some(b) = backup {
        ui::hint(&format!("Backup: {}", display_path(&b)));
    }
    Ok(true)
}

// ---------------------------------------------------------------- secrets input

fn read_env_secret(var: &str) -> anyhow::Result<String> {
    if let Ok(v) = std::env::var(var) {
        if !v.trim().is_empty() {
            return Ok(v.trim().to_owned());
        }
    }
    #[cfg(windows)]
    if let Some(v) = read_windows_user_env(var) {
        return Ok(v);
    }
    bail!("environment variable {var} is not set")
}

/// Reads a user-level environment variable from the registry, for shells started before the
/// variable was defined. The value is captured, never printed.
#[cfg(windows)]
fn read_windows_user_env(var: &str) -> Option<String> {
    if !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let out = std::process::Command::new("reg")
        .args(["query", r"HKCU\Environment", "/v", var])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix(var)?.trim_start();
        let value = rest
            .strip_prefix("REG_SZ")
            .or_else(|| rest.strip_prefix("REG_EXPAND_SZ"))?
            .trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// Obtains a secret from `--from-env`, `--stdin` or a hidden prompt.
pub fn obtain_secret_public(input: &SecretInput, prompt: &str) -> anyhow::Result<Secret> {
    obtain_secret(None, input, prompt)
}

/// Obtains a secret from a flag, an environment variable, stdin or an interactive prompt.
fn obtain_secret(
    direct: Option<&str>,
    input: &SecretInput,
    prompt: &str,
) -> anyhow::Result<Secret> {
    if let Some(v) = direct {
        ui::warn("Passing secrets as command-line arguments can leave them in your shell history.");
        return Ok(Secret::new(v.trim()));
    }
    if let Some(var) = &input.from_env {
        return Ok(Secret::new(read_env_secret(var)?));
    }
    if input.stdin {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        let v = line.trim();
        if v.is_empty() {
            bail!("no value received on stdin");
        }
        return Ok(Secret::new(v));
    }
    if !ui::interactive() {
        bail!("no secret provided; use --from-env VAR or --stdin when not running interactively");
    }
    let value = dialoguer::Password::new()
        .with_prompt(prompt)
        .interact()
        .context("prompt cancelled")?;
    Ok(Secret::new(value.trim()))
}

fn report_storage(source: SecretSource, key: SecretKey) {
    match source {
        SecretSource::Keyring => {
            ui::ok(&format!("{} saved to the OS credential store", key.label()))
        }
        SecretSource::File => {
            ui::warn(&format!(
                "{} saved to a plaintext file readable only by you",
                key.label()
            ));
            ui::hint("The OS credential store was unavailable. Delete it anytime with `agentping uninstall`.");
        }
        SecretSource::Environment => {}
    }
}

async fn send_test(ctx: &Ctx, provider: &str) -> anyhow::Result<()> {
    let config = ctx.load_config()?.config;
    let http = http_client()?;
    let p = build_provider(provider, &config, &ctx.secrets(), &http).map_err(anyhow::Error::msg)?;
    let health = p.health_check().await;
    if !health.healthy {
        bail!("{} connectivity check failed: {}", provider, health.detail);
    }
    ui::ok(&format!(
        "{} connectivity verified ({})",
        title_case(provider),
        health.detail
    ));
    let event = render::test_event();
    let n = render(&event, event.severity, &config);
    p.send(&n).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    ui::ok(&format!(
        "Test notification sent to {}",
        title_case(provider)
    ));
    Ok(())
}

pub fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

// ---------------------------------------------------------------- Discord

pub fn discord(ctx: &Ctx, args: &SetupDiscordArgs) -> anyhow::Result<()> {
    if args.disable {
        ctx.set_config_values(&[("providers.discord.enabled", "false")])?;
        let removed = ctx.secrets().delete(SecretKey::DiscordWebhookUrl)?;
        ui::ok("Discord notifications disabled");
        if removed {
            ui::hint("Stored webhook URL deleted.");
        }
        return Ok(());
    }
    ui::heading("Discord setup");
    if args.webhook_url.is_none() && args.input.from_env.is_none() && !args.input.stdin {
        ui::line("Create a webhook: Server Settings → Integrations → Webhooks → New Webhook → Copy Webhook URL.");
        ui::line(&ui::dim(
            "Your input is hidden and stored in the OS credential store, never in config files.",
        ));
    }
    configure_discord(ctx, args.webhook_url.as_deref(), &args.input, !args.no_test)
}

/// Stores the webhook, enables Discord and optionally sends a test message.
pub fn configure_discord(
    ctx: &Ctx,
    direct: Option<&str>,
    input: &SecretInput,
    test: bool,
) -> anyhow::Result<()> {
    let secret = obtain_secret(direct, input, "Discord webhook URL")?;
    validate_webhook_url(secret.expose())
        .map_err(|e| anyhow::anyhow!("invalid Discord webhook URL: {e}"))?;
    ctx.paths.ensure_dirs()?;
    let source = ctx
        .secrets()
        .set(SecretKey::DiscordWebhookUrl, &secret, input.insecure_store)?;
    report_storage(source, SecretKey::DiscordWebhookUrl);
    ctx.set_config_values(&[("providers.discord.enabled", "true")])?;
    ui::ok("Discord enabled in config");
    if test {
        runtime()?.block_on(send_test(ctx, "discord"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- Telegram

pub fn telegram(ctx: &Ctx, args: &SetupTelegramArgs) -> anyhow::Result<()> {
    if args.disable {
        ctx.set_config_values(&[("providers.telegram.enabled", "false")])?;
        let removed = ctx.secrets().delete(SecretKey::TelegramBotToken)?;
        ui::ok("Telegram notifications disabled");
        if removed {
            ui::hint("Stored bot token deleted.");
        }
        return Ok(());
    }
    ui::heading("Telegram setup");
    if args.input.from_env.is_none() && !args.input.stdin {
        ui::line("1. In Telegram, message @BotFather, send /newbot and copy the bot token.");
        ui::line("2. Open your new bot and send it any message (so it can find your chat).");
    }
    configure_telegram(ctx, &args.input, args.chat_id.as_deref(), !args.no_test)
}

/// Stores the bot token, resolves the chat id, enables Telegram and optionally tests it.
pub fn configure_telegram(
    ctx: &Ctx,
    input: &SecretInput,
    chat_id: Option<&str>,
    test: bool,
) -> anyhow::Result<()> {
    let token = obtain_secret(None, input, "Telegram bot token")?;
    validate_bot_token(token.expose()).map_err(|e| anyhow::anyhow!("invalid bot token: {e}"))?;
    let chat = match chat_id {
        Some(c) => c.trim().to_owned(),
        None => discover_chat(&token)?,
    };
    ctx.paths.ensure_dirs()?;
    let source = ctx
        .secrets()
        .set(SecretKey::TelegramBotToken, &token, input.insecure_store)?;
    report_storage(source, SecretKey::TelegramBotToken);
    let quoted = format!("\"{}\"", chat.replace('"', ""));
    ctx.set_config_values(&[
        ("providers.telegram.chat_id", quoted.as_str()),
        ("providers.telegram.enabled", "true"),
    ])?;
    ui::ok(&format!("Telegram enabled (chat {chat})"));
    if test {
        runtime()?.block_on(send_test(ctx, "telegram"))?;
    }
    Ok(())
}

fn discover_chat(token: &Secret) -> anyhow::Result<String> {
    let provider = TelegramProvider::new(http_client()?, token.clone(), "0", false)
        .map_err(anyhow::Error::msg)?;
    let rt = runtime()?;
    loop {
        let chats = rt
            .block_on(provider.discover_chats())
            .map_err(|e| anyhow::anyhow!("could not read bot updates: {e}"))?;
        match chats.len() {
            0 if ui::interactive() => {
                ui::warn("No chats found yet. Send any message to your bot in Telegram.");
                let retry = dialoguer::Confirm::new()
                    .with_prompt("Check again?")
                    .default(true)
                    .interact()?;
                if !retry {
                    bail!("no chat found; re-run with --chat-id <id>");
                }
            }
            0 => bail!("no chat found; send a message to the bot or pass --chat-id <id>"),
            1 => {
                ui::ok(&format!(
                    "Found chat '{}' ({})",
                    chats[0].label, chats[0].kind
                ));
                return Ok(chats[0].id.clone());
            }
            _ if ui::interactive() => {
                let items: Vec<String> = chats
                    .iter()
                    .map(|c| format!("{} ({}, id {})", c.label, c.kind, c.id))
                    .collect();
                let idx = dialoguer::Select::new()
                    .with_prompt("Which chat should receive notifications?")
                    .items(&items)
                    .default(0)
                    .interact()?;
                return Ok(chats[idx].id.clone());
            }
            _ => bail!("several chats found; pass --chat-id <id>"),
        }
    }
}

// ---------------------------------------------------------------- WhatsApp

pub fn whatsapp() -> anyhow::Result<()> {
    ui::heading("WhatsApp");
    ui::warn("WhatsApp delivery is not implemented in this version of AgentPing.");
    ui::line("");
    ui::line("AgentPing will only support WhatsApp through Meta's official WhatsApp Business");
    ui::line(
        "Platform (Cloud API). It requires a Meta Business account, a dedicated business phone",
    );
    ui::line(
        "number, a permanent access token and pre-approved message templates for messages sent",
    );
    ui::line("outside a 24-hour customer-service window. Conversations may be billed by Meta.");
    ui::line("");
    ui::hint("Details: docs/integrations/whatsapp.md");
    ui::hint(
        "Use Discord (`agentping setup discord`) or Telegram (`agentping setup telegram`) today.",
    );
    Ok(())
}
