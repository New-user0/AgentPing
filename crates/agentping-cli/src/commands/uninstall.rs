//! `agentping uninstall [--purge]`

use agentping_agents::claude::install::Scope;
use agentping_core::paths::display_path;
use agentping_core::secrets::SecretKey;
use agentping_daemon::service;

use crate::cli::UninstallArgs;
use crate::commands::{daemon_cmd, setup};
use crate::context::Ctx;
use crate::ui;

pub fn run(ctx: &Ctx, args: &UninstallArgs) -> anyhow::Result<()> {
    if !args.yes {
        if !ui::interactive() {
            anyhow::bail!("refusing to uninstall without confirmation; pass --yes");
        }
        let what = if args.purge {
            "Remove AgentPing hooks, service, credentials, configuration and ALL event history"
        } else {
            "Remove AgentPing hooks, background service and stored credentials (config and history are kept)"
        };
        if !dialoguer::Confirm::new()
            .with_prompt(format!("{what}?"))
            .default(false)
            .interact()?
        {
            ui::info("Nothing changed.");
            return Ok(());
        }
    }

    for scope in [Scope::User, Scope::Project, Scope::Local] {
        if let Err(e) = setup::remove_claude_hooks(scope, false) {
            ui::warn(&format!("Could not update Claude Code settings: {e:#}"));
        }
    }
    if let Err(e) = daemon_cmd::stop(ctx) {
        ui::warn(&format!("{e:#}"));
    }
    if service::is_installed() {
        match service::uninstall() {
            Ok(msg) => ui::ok(&msg),
            Err(e) => ui::warn(&format!("{e:#}")),
        }
    }
    let secrets = ctx.secrets();
    for key in SecretKey::ALL {
        match secrets.delete(key) {
            Ok(true) => ui::ok(&format!("Deleted stored {}", key.label())),
            Ok(false) => {}
            Err(e) => ui::warn(&format!("Could not delete {}: {e}", key.label())),
        }
    }
    if args.purge {
        for dir in [
            &ctx.paths.config_dir,
            &ctx.paths.data_dir,
            &ctx.paths.log_dir,
        ] {
            if dir.exists() {
                std::fs::remove_dir_all(dir)?;
                ui::ok(&format!("Deleted {}", display_path(dir)));
            }
        }
    } else {
        ui::info(&format!(
            "Kept configuration in {}",
            display_path(&ctx.paths.config_dir)
        ));
        ui::info(&format!(
            "Kept event history in {}",
            display_path(&ctx.paths.data_dir)
        ));
    }
    ui::blank();
    ui::line("To remove the binary itself: cargo uninstall agentping-cli (or delete the agentping executable).");
    Ok(())
}
