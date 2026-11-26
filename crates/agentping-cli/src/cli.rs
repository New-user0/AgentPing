//! Command-line definitions.

use clap::{Args, Parser, Subcommand, ValueEnum};

/// AgentPing — stay connected to your AI coding agents, wherever you are.
#[derive(Debug, Parser)]
#[command(
    name = "agentping",
    version,
    about = "Stay connected to your AI coding agents, wherever you are.",
    long_about = "Stay connected to your AI coding agents, wherever you are.\n\n\
                  AgentPing notifies you on your phone (Discord, Telegram) when AI coding agents \
                  such as Claude Code need permission, ask a question, finish, fail or go idle.\n\n\
                  Get started with `agentping init`.",
    propagate_version = true
)]
pub struct Cli {
    /// Disable colored output (also respects NO_COLOR).
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Print machine-readable JSON where supported.
    #[arg(long, global = true)]
    pub json: bool,

    /// Also print logs to stderr (-v info, -vv debug, -vvv trace).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Interactive first-time setup (providers, Claude Code hooks, daemon).
    Init(InitArgs),
    /// Configure an integration or notification provider.
    #[command(subcommand)]
    Setup(SetupCommand),
    /// Send a test notification through configured providers.
    Test(TestArgs),
    /// Show daemon, session and notification status.
    Status,
    /// Diagnose the installation and suggest fixes.
    Doctor(DoctorArgs),
    /// Show AgentPing logs.
    Logs(LogsArgs),
    /// Inspect or clear event history.
    Events(EventsArgs),
    /// List notification providers and their state.
    Providers(ProvidersArgs),
    /// Show, validate or change configuration.
    Config(ConfigArgs),
    /// Explain how the rules would treat an event.
    #[command(subcommand)]
    Rules(RulesCommand),
    /// Show what AgentPing stores and sends.
    Privacy,
    /// Route permission requests and questions to Discord while you are away.
    Away,
    /// Stop routing prompts to Discord (you are back at the keyboard).
    Back,
    /// Manage the background daemon.
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// Remove hooks, services and stored credentials.
    Uninstall(UninstallArgs),
    /// Receive a native hook payload on stdin (used by agent hooks).
    #[command(hide = true)]
    Hook(HookArgs),
    /// Deliver queued notifications and exit (spawned when the daemon is offline).
    #[command(hide = true)]
    Deliver(DeliverArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Do not prompt; use flags and environment variables.
    #[arg(long)]
    pub non_interactive: bool,
    /// Providers to enable (non-interactive), e.g. `discord,telegram`.
    #[arg(long, value_delimiter = ',')]
    pub providers: Vec<ProviderName>,
    /// Read the Discord webhook URL from this environment variable.
    #[arg(long, value_name = "VAR")]
    pub discord_webhook_env: Option<String>,
    /// Read the Telegram bot token from this environment variable.
    #[arg(long, value_name = "VAR")]
    pub telegram_token_env: Option<String>,
    /// Telegram chat id.
    #[arg(long)]
    pub telegram_chat_id: Option<String>,
    /// Where to install Claude Code hooks.
    #[arg(long, value_enum, default_value_t = HookScope::User)]
    pub claude_scope: HookScope,
    /// Skip Claude Code hook installation.
    #[arg(long)]
    pub no_claude: bool,
    /// Register the daemon as a background service.
    #[arg(long)]
    pub install_daemon: bool,
    /// Allow storing secrets in a plaintext file if the OS credential store is unavailable.
    #[arg(long)]
    pub insecure_store: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProviderName {
    Discord,
    Telegram,
    Whatsapp,
}

impl ProviderName {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderName::Discord => "discord",
            ProviderName::Telegram => "telegram",
            ProviderName::Whatsapp => "whatsapp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HookScope {
    /// ~/.claude/settings.json (all projects)
    User,
    /// ./.claude/settings.json (shared with your team)
    Project,
    /// ./.claude/settings.local.json (this machine only)
    Local,
}

#[derive(Debug, Subcommand)]
pub enum SetupCommand {
    /// Install (or remove) AgentPing's Claude Code hooks.
    Claude(SetupClaudeArgs),
    /// Configure Discord webhook notifications.
    Discord(SetupDiscordArgs),
    /// Configure Telegram bot notifications.
    Telegram(SetupTelegramArgs),
    /// Enable two-way control from Discord (bot: approve, answer, reply to continue).
    DiscordBot(SetupDiscordBotArgs),
    /// WhatsApp support status and requirements.
    Whatsapp,
}

#[derive(Debug, Args)]
pub struct SetupClaudeArgs {
    /// Which Claude Code settings file to modify.
    #[arg(long, value_enum, default_value_t = HookScope::User)]
    pub scope: HookScope,
    /// Show what would change without writing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Remove AgentPing's hooks instead of installing them.
    #[arg(long)]
    pub remove: bool,
}

#[derive(Debug, Args)]
pub struct SecretInput {
    /// Read the secret from this environment variable (keeps it out of shell history).
    #[arg(long, value_name = "VAR", conflicts_with = "stdin")]
    pub from_env: Option<String>,
    /// Read the secret from standard input.
    #[arg(long)]
    pub stdin: bool,
    /// Allow a user-only plaintext file if the OS credential store is unavailable.
    #[arg(long)]
    pub insecure_store: bool,
}

#[derive(Debug, Args)]
pub struct SetupDiscordArgs {
    /// Webhook URL (prefer --from-env or the interactive prompt to keep it out of shell history).
    #[arg(long, value_name = "URL", conflicts_with_all = ["from_env", "stdin"])]
    pub webhook_url: Option<String>,
    #[command(flatten)]
    pub input: SecretInput,
    /// Do not send a test message.
    #[arg(long)]
    pub no_test: bool,
    /// Disable Discord and delete the stored webhook.
    #[arg(long, conflicts_with_all = ["webhook_url", "from_env", "stdin"])]
    pub disable: bool,
}

#[derive(Debug, Args)]
pub struct SetupTelegramArgs {
    /// Chat id to send to (otherwise discovered from messages sent to the bot).
    #[arg(long)]
    pub chat_id: Option<String>,
    #[command(flatten)]
    pub input: SecretInput,
    /// Do not send a test message.
    #[arg(long)]
    pub no_test: bool,
    /// Disable Telegram and delete the stored bot token.
    #[arg(long, conflicts_with_all = ["from_env", "stdin", "chat_id"])]
    pub disable: bool,
}

#[derive(Debug, Args)]
pub struct SetupDiscordBotArgs {
    #[command(flatten)]
    pub input: SecretInput,
    /// Channel the bot posts prompts in and reads replies from (default: the webhook's channel).
    #[arg(long)]
    pub channel_id: Option<String>,
    /// Your Discord user id (skips pairing; otherwise you type a one-time code in the channel).
    #[arg(long)]
    pub user_id: Option<String>,
    /// Disable remote interaction and delete the stored bot token.
    #[arg(long, conflicts_with_all = ["from_env", "stdin", "channel_id", "user_id"])]
    pub disable: bool,
}

#[derive(Debug, Args)]
pub struct TestArgs {
    /// Provider to test (default: all enabled providers).
    #[arg(value_enum)]
    pub provider: Option<ProviderName>,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Skip network connectivity checks.
    #[arg(long)]
    pub offline: bool,
}

#[derive(Debug, Args)]
pub struct LogsArgs {
    /// Keep printing new log lines.
    #[arg(short, long)]
    pub follow: bool,
    /// Number of lines to show.
    #[arg(short = 'n', long, default_value_t = 50)]
    pub lines: usize,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct EventsArgs {
    #[command(subcommand)]
    pub action: Option<EventsAction>,
    /// Only events received today.
    #[arg(long, conflicts_with = "since")]
    pub today: bool,
    /// Only events newer than this (e.g. 30m, 2h, 7d).
    #[arg(long, value_parser = parse_age)]
    pub since: Option<chrono::Duration>,
    /// Filter by agent (e.g. claude).
    #[arg(long)]
    pub agent: Option<String>,
    /// Filter by project name.
    #[arg(long)]
    pub project: Option<String>,
    /// Filter by event type (e.g. permission_request).
    #[arg(long = "type", value_name = "TYPE")]
    pub event_type: Option<String>,
    /// Filter by session id prefix.
    #[arg(long)]
    pub session: Option<String>,
    /// Maximum number of events.
    #[arg(long, default_value_t = 25)]
    pub limit: u32,
    /// Page number (starting at 1).
    #[arg(long, default_value_t = 1)]
    pub page: u32,
}

#[derive(Debug, Subcommand)]
pub enum EventsAction {
    /// Show one event with its delivery history.
    Show {
        /// Event id or unique prefix.
        id: String,
    },
    /// Delete all event history.
    Clear {
        /// Do not ask for confirmation.
        #[arg(long, short)]
        yes: bool,
    },
}

#[derive(Debug, Args)]
pub struct ProvidersArgs {
    /// Also check connectivity.
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: Option<ConfigAction>,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print the configuration file path.
    Path,
    /// Print the effective configuration.
    Show,
    /// Validate the configuration file.
    Validate,
    /// Set a value, e.g. `agentping config set notifications.include_message false`.
    Set {
        /// Dotted key.
        key: String,
        /// New value (TOML syntax; bare words are strings).
        value: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum RulesCommand {
    /// Evaluate a hypothetical event against your rules.
    Test(RulesTestArgs),
}

#[derive(Debug, Args)]
pub struct RulesTestArgs {
    /// Event type, e.g. permission_request.
    #[arg(long = "type", value_name = "TYPE")]
    pub event_type: String,
    /// Project name.
    #[arg(long)]
    pub project: Option<String>,
    /// Project path.
    #[arg(long)]
    pub path: Option<String>,
    /// Agent id.
    #[arg(long, default_value = "claude")]
    pub agent: String,
    /// Severity override (low, normal, high, urgent).
    #[arg(long)]
    pub severity: Option<String>,
    /// Local time to evaluate at (HH:MM).
    #[arg(long)]
    pub time: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start the daemon in the background.
    Start,
    /// Stop the running daemon.
    Stop,
    /// Restart the daemon.
    Restart,
    /// Show whether the daemon is running.
    Status,
    /// Run the daemon in the foreground.
    Run,
    /// Start the daemon automatically at login (systemd / launchd / Task Scheduler).
    Install,
    /// Remove the login service.
    Uninstall,
}

#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Also delete configuration, logs and event history.
    #[arg(long)]
    pub purge: bool,
    /// Do not ask for confirmation.
    #[arg(long, short)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct HookArgs {
    /// Agent integration (currently `claude`).
    pub agent: String,
    /// Wait in the background for a remote reply after the agent stops (asyncRewake hook).
    #[arg(long)]
    pub wait_reply: bool,
}

#[derive(Debug, Args)]
pub struct DeliverArgs {
    /// Keep waiting for notifications due within this many seconds.
    #[arg(long, default_value_t = 120)]
    pub horizon_secs: u64,
}

/// Parses `30s`, `15m`, `2h`, `7d`.
pub fn parse_age(s: &str) -> Result<chrono::Duration, String> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: i64 = num
        .parse()
        .map_err(|_| format!("'{s}' is not a duration like 30m, 2h or 7d"))?;
    match unit {
        "s" => Ok(chrono::Duration::seconds(n)),
        "m" | "" => Ok(chrono::Duration::minutes(n)),
        "h" => Ok(chrono::Duration::hours(n)),
        "d" => Ok(chrono::Duration::days(n)),
        "w" => Ok(chrono::Duration::weeks(n)),
        _ => Err(format!("'{s}' is not a duration like 30m, 2h or 7d")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn ages_parse() {
        assert_eq!(parse_age("2h").unwrap(), chrono::Duration::hours(2));
        assert_eq!(parse_age("7d").unwrap(), chrono::Duration::days(7));
        assert_eq!(parse_age("45").unwrap(), chrono::Duration::minutes(45));
        assert!(parse_age("soon").is_err());
    }
}
