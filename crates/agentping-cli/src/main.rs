//! `agentping` — stay connected to your AI coding agents, wherever you are.

mod cli;
mod commands;
mod context;
mod logging;
mod ui;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command, SetupCommand};
use crate::context::Ctx;

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Hooks must never print anything or fail; handle them before any other setup.
    if let Some(Command::Hook(args)) = &cli.command {
        let code = commands::hook::run(&args.agent, args.wait_reply);
        return ExitCode::from(u8::try_from(code).unwrap_or(0));
    }

    if cli.no_color {
        ui::disable_color();
    }
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            ui::error(&format!("{e:#}"));
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let ctx = Ctx::new(cli.json)?;
    let Some(command) = cli.command else {
        return commands::status::overview(&ctx);
    };
    let is_daemon_run = matches!(command, Command::Daemon(cli::DaemonCommand::Run))
        || matches!(command, Command::Deliver(_));
    if !is_daemon_run {
        let level = ctx.config_or_default().general.log_level;
        let stderr = match cli.verbose {
            0 => None,
            1 => Some("info"),
            2 => Some("debug"),
            _ => Some("trace"),
        };
        logging::init(&ctx.paths, &level, logging::Mode::WithStderr(stderr));
    }
    match &command {
        Command::Init(args) => commands::init::run(&ctx, args),
        Command::Setup(SetupCommand::Claude(args)) => commands::setup::claude(&ctx, args),
        Command::Setup(SetupCommand::Discord(args)) => commands::setup::discord(&ctx, args),
        Command::Setup(SetupCommand::Telegram(args)) => commands::setup::telegram(&ctx, args),
        Command::Setup(SetupCommand::Whatsapp) => commands::setup::whatsapp(),
        Command::Setup(SetupCommand::DiscordBot(args)) => {
            commands::remote_cmd::setup_discord_bot(&ctx, args)
        }
        Command::Away => commands::remote_cmd::presence(&ctx, true),
        Command::Back => commands::remote_cmd::presence(&ctx, false),
        Command::Test(args) => commands::test_cmd::run(&ctx, args),
        Command::Status => commands::status::run(&ctx),
        Command::Doctor(args) => commands::doctor::run(&ctx, args),
        Command::Logs(args) => commands::logs::run(&ctx, args),
        Command::Events(args) => commands::events::run(&ctx, args),
        Command::Providers(args) => commands::providers::run(&ctx, args),
        Command::Config(args) => commands::config_cmd::run(&ctx, args),
        Command::Rules(cmd) => commands::rules_cmd::run(&ctx, cmd),
        Command::Privacy => commands::privacy::run(&ctx),
        Command::Daemon(cmd) => commands::daemon_cmd::run(&ctx, cmd, cli.verbose),
        Command::Uninstall(args) => commands::uninstall::run(&ctx, args),
        Command::Deliver(args) => commands::deliver::run(&ctx, args),
        Command::Hook(_) => Ok(()),
    }
}
