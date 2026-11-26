//! `agentping doctor`: diagnostics with actionable fixes. Never prints secrets.

use agentping_agents::claude::detect;
use agentping_agents::claude::install::{inspect, settings_path, Scope};
use agentping_core::config::Config;
use agentping_core::paths::display_path;
use agentping_core::secrets::{SecretKey, SecretSource};
use agentping_core::VERSION;
use agentping_notifiers::{build_provider, http_client};
use chrono::Utc;
use serde::Serialize;

use crate::cli::DoctorArgs;
use crate::commands::daemon_cmd::probe;
use crate::commands::setup::title_case;
use crate::context::{is_dev_build, Ctx};
use crate::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Level {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
struct Check {
    name: String,
    level: Level,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fix: Option<String>,
}

#[derive(Default)]
struct Report {
    checks: Vec<Check>,
}

impl Report {
    fn add(
        &mut self,
        level: Level,
        name: impl Into<String>,
        detail: impl Into<String>,
        fix: Option<&str>,
    ) {
        self.checks.push(Check {
            name: name.into(),
            level,
            detail: detail.into(),
            fix: fix.map(str::to_owned),
        });
    }
    fn pass(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.add(Level::Pass, name, detail, None);
    }
    fn warn(&mut self, name: impl Into<String>, detail: impl Into<String>, fix: &str) {
        self.add(Level::Warn, name, detail, Some(fix));
    }
    fn fail(&mut self, name: impl Into<String>, detail: impl Into<String>, fix: &str) {
        self.add(Level::Fail, name, detail, Some(fix));
    }
}

pub fn run(ctx: &Ctx, args: &DoctorArgs) -> anyhow::Result<()> {
    let mut r = Report::default();
    let exe = ctx.current_exe()?;
    r.pass(
        "Version",
        format!(
            "AgentPing v{VERSION} ({}, {})",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    );
    if is_dev_build(&exe) {
        r.warn(
            "Installation",
            format!("running a development build from {}", display_path(&exe)),
            "Install with `cargo install --path crates/agentping-cli` and re-run `agentping setup claude`",
        );
    } else {
        r.pass("Installation", display_path(&exe));
    }

    // Configuration
    let config_path = ctx.paths.config_file();
    let config = if !config_path.exists() {
        r.warn(
            "Configuration",
            "no config file; using defaults",
            "Run `agentping init`",
        );
        Config::default()
    } else {
        match ctx.load_config() {
            Ok(l) => {
                r.pass(
                    "Configuration",
                    format!("{} is valid", display_path(&config_path)),
                );
                l.config
            }
            Err(e) => {
                r.fail(
                    "Configuration",
                    format!("{e:#}"),
                    "Fix the reported keys, then run `agentping config validate`",
                );
                Config::default()
            }
        }
    };

    // Storage
    match ctx.open_db(&config) {
        Ok(db) => match (db.quick_check(), db.store().stats(Utc::now())) {
            (Ok(None), Ok(stats)) => {
                r.pass(
                    "Database",
                    format!(
                        "{} ({} events)",
                        display_path(&config.database_path(&ctx.paths)),
                        stats.events
                    ),
                );
                if stats.failed > 0 {
                    r.warn(
                        "Deliveries",
                        format!("{} notification(s) failed", stats.failed),
                        "Inspect with `agentping events --limit 50` and `agentping events show <id>`",
                    );
                }
                if stats.unprocessed_events > 0 {
                    r.warn(
                        "Queue",
                        format!(
                            "{} event(s) waiting to be processed",
                            stats.unprocessed_events
                        ),
                        "Start the daemon (`agentping daemon start`) to process them",
                    );
                }
            }
            (Ok(Some(problem)), _) => r.fail(
                "Database",
                format!("integrity check failed: {problem}"),
                "Back up and delete the database file; AgentPing recreates it",
            ),
            (Err(e), _) | (_, Err(e)) => r.fail(
                "Database",
                e.to_string(),
                "Check file permissions on the data directory",
            ),
        },
        Err(e) => r.fail(
            "Database",
            format!("{e:#}"),
            "Check that the data directory is writable",
        ),
    }

    // Daemon / IPC
    let daemon = probe(ctx, &config);
    if daemon.running {
        let token = agentping_daemon::local::read_token(&ctx.paths).is_some();
        r.pass("Daemon", format!("running on {}", daemon.address));
        if token {
            r.pass("Local API", "token present; loopback only");
        } else {
            r.fail(
                "Local API",
                "token file missing",
                "Restart the daemon: `agentping daemon restart`",
            );
        }
    } else {
        r.warn(
            "Daemon",
            "not running (hooks will deliver with a short-lived background process)",
            "Run `agentping daemon start`, or `agentping daemon install` to start it at login",
        );
    }

    // Claude Code
    let claude = detect(true);
    if claude.installed() {
        let detail = match (&claude.version, &claude.binary) {
            (Some(v), _) => v.clone(),
            (None, Some(b)) => display_path(b),
            (None, None) => "configuration directory found".into(),
        };
        r.pass("Claude Code", detail);
    } else {
        r.warn(
            "Claude Code",
            "not detected",
            "Install Claude Code, then run `agentping setup claude`",
        );
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut any_hooks = false;
    for (scope, label) in [
        (Scope::User, "user"),
        (Scope::Project, "project"),
        (Scope::Local, "local"),
    ] {
        let Some(path) = settings_path(scope, &cwd) else {
            continue;
        };
        if !path.exists() {
            continue;
        }
        match inspect(
            &path,
            &agentping_agents::claude::install::managed_hooks(&config.remote),
        ) {
            Ok(s) if s.installed.is_empty() => {}
            Ok(s) => {
                any_hooks = true;
                let name = format!("Claude hooks ({label})");
                if !s.broken_commands.is_empty() {
                    r.fail(
                        name,
                        format!(
                            "hook command points to a missing binary: {}",
                            s.broken_commands.join(", ")
                        ),
                        "Re-run `agentping setup claude` with the installed binary",
                    );
                } else if !s.missing.is_empty() {
                    r.warn(
                        name,
                        format!("missing: {}", s.missing.join(", ")),
                        "Re-run `agentping setup claude`",
                    );
                } else {
                    r.pass(
                        name,
                        format!("all hooks installed in {}", display_path(&path)),
                    );
                }
            }
            Err(e) => r.fail(
                format!("Claude settings ({label})"),
                e.to_string(),
                "Fix the JSON syntax in that file",
            ),
        }
    }
    if !any_hooks {
        r.warn(
            "Claude hooks",
            "not installed",
            "Run `agentping setup claude`",
        );
    }

    // Two-way remote interaction
    if config.remote.enabled {
        match ctx.secrets().get(SecretKey::DiscordBotToken) {
            Ok(Some(_)) => r.pass("Discord bot token", "stored"),
            _ => r.fail(
                "Discord bot token",
                "remote is enabled but no bot token is stored",
                "Run `agentping setup discord-bot`",
            ),
        }
        if daemon.running {
            let status = agentping_daemon::DaemonClient::new(
                &ctx.paths,
                &config,
                std::time::Duration::from_secs(3),
            )
            .ok()
            .and_then(|c| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .ok()
                    .and_then(|rt| rt.block_on(c.remote_status()).ok())
            });
            match status {
                Some(s) if s.connected => r.pass(
                    "Two-way (Discord)",
                    format!("connected; {}", if s.away { "away: prompts go to Discord" } else { "at keyboard: prompts stay local" }),
                ),
                Some(s) => r.fail(
                    "Two-way (Discord)",
                    s.error.unwrap_or_else(|| "the bot is not connected to Discord".into()),
                    "Check the bot token and Message Content Intent, then `agentping daemon restart`",
                ),
                None => r.warn("Two-way (Discord)", "could not query the daemon", "Run `agentping daemon restart`"),
            }
        } else {
            r.fail(
                "Two-way (Discord)",
                "needs the daemon, which is not running",
                "Run `agentping daemon start`",
            );
        }
    }

    // Providers
    let secrets = ctx.secrets();
    let enabled = config.providers.enabled_names();
    if enabled.is_empty() {
        r.fail(
            "Providers",
            "no notification provider is enabled",
            "Run `agentping setup discord` or `agentping setup telegram`",
        );
    }
    let http = http_client()?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    for name in enabled {
        let label = title_case(name);
        let key = if name == "discord" {
            SecretKey::DiscordWebhookUrl
        } else {
            SecretKey::TelegramBotToken
        };
        if let Ok(Some((_, src))) = secrets.get(key) {
            if src == SecretSource::File {
                r.warn(
                    format!("{label} credential"),
                    "stored in a plaintext file",
                    "Re-run setup without --insecure-store once the OS credential store works",
                );
            }
        }
        match build_provider(name, &config, &secrets, &http) {
            Err(e) => r.fail(label.clone(), e, &format!("Run `agentping setup {name}`")),
            Ok(_) if args.offline => r.pass(label.clone(), "configured (connectivity not checked)"),
            Ok(p) => {
                let h = rt.block_on(p.health_check());
                if h.healthy {
                    r.pass(format!("{label} connectivity"), h.detail);
                } else {
                    r.fail(
                        format!("{label} connectivity"),
                        h.detail,
                        &format!("Check your network, then `agentping setup {name}`"),
                    );
                }
            }
        }
    }
    if secrets.keyring_enabled() {
        r.pass("Credential store", "OS credential store available");
    } else {
        r.warn(
            "Credential store",
            "OS credential store disabled or unsupported",
            "Secrets fall back to environment variables or the opt-in plaintext file",
        );
    }
    if agentping_core::fsutil::is_world_or_group_accessible(&ctx.paths.secrets_file()) {
        r.fail(
            "Permissions",
            "secrets file is readable by other users",
            "chmod 600 the secrets file",
        );
    }

    print(ctx, &r)
}

fn print(ctx: &Ctx, r: &Report) -> anyhow::Result<()> {
    let fails = r.checks.iter().filter(|c| c.level == Level::Fail).count();
    let warns = r.checks.iter().filter(|c| c.level == Level::Warn).count();
    if ctx.json {
        ui::json(&serde_json::json!({ "checks": r.checks, "failures": fails, "warnings": warns }))?;
    } else {
        ui::heading("AgentPing Diagnostics");
        ui::blank();
        for c in &r.checks {
            let text = format!("{}: {}", c.name, c.detail);
            match c.level {
                Level::Pass => ui::ok(&text),
                Level::Warn => ui::warn(&text),
                Level::Fail => ui::fail(&text),
            }
            if let Some(fix) = &c.fix {
                ui::hint(fix);
            }
        }
        ui::blank();
        match (fails, warns) {
            (0, 0) => ui::line(&ui::status_word("Everything looks good.", Some(true))),
            (0, w) => ui::line(&ui::status_word(
                &format!("Working, with {w} warning(s)."),
                None,
            )),
            (f, _) => ui::line(&ui::status_word(
                &format!("{f} problem(s) need attention."),
                Some(false),
            )),
        }
    }
    if fails > 0 {
        std::process::exit(1);
    }
    Ok(())
}
