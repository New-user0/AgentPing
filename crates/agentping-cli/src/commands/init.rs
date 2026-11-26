//! `agentping init`: guided onboarding (interactive or scripted).

use agentping_agents::claude::detect;
use agentping_core::paths::display_path;
use agentping_core::{TAGLINE, VERSION};
use agentping_daemon::service;

use crate::cli::{DoctorArgs, InitArgs, ProviderName, SecretInput};
use crate::commands::{daemon_cmd, doctor, setup};
use crate::context::Ctx;
use crate::ui;

enum DaemonChoice {
    Service,
    StartNow,
    Skip,
}

pub fn run(ctx: &Ctx, args: &InitArgs) -> anyhow::Result<()> {
    let interactive = !args.non_interactive && ui::interactive();
    if !args.non_interactive && !interactive {
        anyhow::bail!("`agentping init` needs a terminal; use --non-interactive with flags for scripted setup");
    }

    ui::heading(&format!("Welcome to AgentPing v{VERSION}"));
    ui::line(&ui::dim(TAGLINE));
    ui::blank();

    // 1-3. Environment detection.
    ui::heading("Environment");
    ui::kv(
        "Operating system",
        &format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH),
    );
    let claude = detect(true);
    match (&claude.version, claude.installed()) {
        (Some(v), _) => ui::ok(&format!("Claude Code detected: {v}")),
        (None, true) => ui::ok("Claude Code detected"),
        (None, false) => ui::warn("Claude Code was not found on PATH"),
    }
    if let Some(dir) = &claude.config_dir {
        ui::kv("Claude config", &display_path(dir));
    }
    if ctx.ensure_config_file()? {
        ui::ok(&format!(
            "Created {}",
            display_path(&ctx.paths.config_file())
        ));
    } else {
        ui::ok(&format!("Using {}", display_path(&ctx.paths.config_file())));
    }
    ui::blank();

    // 4-6. Providers.
    let providers = if interactive {
        choose_providers()?
    } else {
        args.providers.clone()
    };
    let mut configured = Vec::new();
    for p in &providers {
        let result = match p {
            ProviderName::Discord => {
                ui::heading("Discord");
                if interactive {
                    ui::line("Create a webhook: Server Settings → Integrations → Webhooks → New Webhook → Copy Webhook URL.");
                }
                let input = SecretInput {
                    from_env: args.discord_webhook_env.clone(),
                    stdin: false,
                    insecure_store: args.insecure_store,
                };
                setup::configure_discord(ctx, None, &input, true)
            }
            ProviderName::Telegram => {
                ui::heading("Telegram");
                if interactive {
                    ui::line("1. Message @BotFather, send /newbot and copy the token.");
                    ui::line("2. Send any message to your new bot.");
                }
                let input = SecretInput {
                    from_env: args.telegram_token_env.clone(),
                    stdin: false,
                    insecure_store: args.insecure_store,
                };
                setup::configure_telegram(ctx, &input, args.telegram_chat_id.as_deref(), true)
            }
            ProviderName::Whatsapp => setup::whatsapp(),
        };
        match result {
            Ok(()) if *p != ProviderName::Whatsapp => configured.push(p.as_str()),
            Ok(()) => {}
            Err(e) => {
                ui::fail(&format!(
                    "{} setup failed: {e:#}",
                    setup::title_case(p.as_str())
                ));
                ui::hint(&format!(
                    "Retry later with `agentping setup {}`",
                    p.as_str()
                ));
            }
        }
        ui::blank();
    }

    // 7. Claude Code hooks.
    let install_hooks = if args.no_claude {
        false
    } else if interactive {
        dialoguer::Confirm::new()
            .with_prompt("Install AgentPing hooks into Claude Code (your existing settings are preserved and backed up)?")
            .default(claude.installed())
            .interact()?
    } else {
        true
    };
    if install_hooks {
        ui::heading("Claude Code hooks");
        if let Err(e) = setup::install_claude_hooks(ctx, args.claude_scope.into(), false) {
            ui::fail(&format!("Could not install hooks: {e:#}"));
        }
        ui::blank();
    }

    // 8. Daemon.
    let choice = if interactive {
        let items = [
            "Start automatically at login (recommended)",
            "Start now for this session only",
            "Skip (hooks send notifications directly)",
        ];
        match dialoguer::Select::new()
            .with_prompt("Background daemon")
            .items(&items)
            .default(0)
            .interact()?
        {
            0 => DaemonChoice::Service,
            1 => DaemonChoice::StartNow,
            _ => DaemonChoice::Skip,
        }
    } else if args.install_daemon {
        DaemonChoice::Service
    } else {
        DaemonChoice::Skip
    };
    ui::heading("Daemon");
    let daemon_result = match choice {
        DaemonChoice::Service => service::install(&ctx.current_exe()?, &ctx.paths.log_dir)
            .and_then(|msg| {
                ui::ok(&msg);
                if cfg!(windows) {
                    daemon_cmd::start(ctx)
                } else {
                    Ok(())
                }
            }),
        DaemonChoice::StartNow => daemon_cmd::start(ctx),
        DaemonChoice::Skip => {
            ui::info("Skipped. Hooks will start a short-lived sender when needed.");
            Ok(())
        }
    };
    if let Err(e) = daemon_result {
        ui::fail(&format!("{e:#}"));
    }
    ui::blank();

    // 9. Verify.
    ui::heading("Summary");
    if configured.is_empty() {
        ui::warn("No notification provider configured yet");
        ui::hint("agentping setup discord   or   agentping setup telegram");
    } else {
        ui::ok(&format!("Notifications via {}", configured.join(", ")));
    }
    if install_hooks {
        ui::ok("Claude Code will ping you when it needs permission, asks a question, finishes or fails");
        ui::hint("Restart open Claude Code sessions to activate the hooks.");
    }
    ui::hint("Run `agentping doctor` any time to verify the setup.");
    if interactive {
        ui::blank();
        let run_doctor = dialoguer::Confirm::new()
            .with_prompt("Run diagnostics now?")
            .default(true)
            .interact()?;
        if run_doctor {
            ui::blank();
            doctor::run(ctx, &DoctorArgs { offline: false })?;
        }
    }
    Ok(())
}

fn choose_providers() -> anyhow::Result<Vec<ProviderName>> {
    ui::heading("Notification providers");
    ui::line(&ui::dim(
        "Space to select, Enter to confirm. WhatsApp is not available yet.",
    ));
    let options = [ProviderName::Discord, ProviderName::Telegram];
    let labels = ["Discord (webhook)", "Telegram (bot)"];
    let picked = dialoguer::MultiSelect::new()
        .items(&labels)
        .defaults(&[true, false])
        .interact()?;
    ui::blank();
    Ok(picked.into_iter().map(|i| options[i]).collect())
}
