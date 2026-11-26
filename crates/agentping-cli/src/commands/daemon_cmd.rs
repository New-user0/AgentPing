//! `agentping daemon …`

use std::time::{Duration, Instant};

use agentping_core::time::humanize_duration;
use agentping_core::Config;
use agentping_daemon::local::read_state;
use agentping_daemon::process::spawn_detached;
use agentping_daemon::runtime::run_daemon;
use agentping_daemon::server::Health;
use agentping_daemon::{service, ClientError, DaemonClient};
use serde::Serialize;

use crate::cli::DaemonCommand;
use crate::context::{is_dev_build, Ctx};
use crate::{logging, ui};

/// Result of probing the daemon.
#[derive(Debug, Clone, Serialize)]
pub struct DaemonProbe {
    /// Whether it responded.
    pub running: bool,
    /// Address probed.
    pub address: String,
    /// Health payload.
    pub health: Option<Health>,
    /// Error, when not running.
    pub error: Option<String>,
}

pub fn probe(ctx: &Ctx, config: &Config) -> DaemonProbe {
    let address = format!("{}:{}", config.daemon.host, config.daemon.port);
    let client = match DaemonClient::new(&ctx.paths, config, Duration::from_secs(2)) {
        Ok(c) => c,
        Err(e) => {
            return DaemonProbe {
                running: false,
                address,
                health: None,
                error: Some(e.to_string()),
            }
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            return DaemonProbe {
                running: false,
                address,
                health: None,
                error: Some(e.to_string()),
            }
        }
    };
    match rt.block_on(client.health()) {
        Ok(h) => DaemonProbe {
            running: true,
            address,
            health: Some(h),
            error: None,
        },
        Err(e) => DaemonProbe {
            running: false,
            address,
            health: None,
            error: Some(e.to_string()),
        },
    }
}

fn wait_for(ctx: &Ctx, config: &Config, running: bool, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if probe(ctx, config).running == running {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

pub fn run(ctx: &Ctx, cmd: &DaemonCommand, verbose: u8) -> anyhow::Result<()> {
    match cmd {
        DaemonCommand::Run => {
            let config = ctx.load_config()?.config;
            let stderr = match verbose {
                0 => Some("info"),
                1 => Some("debug"),
                _ => Some("trace"),
            };
            logging::init(
                &ctx.paths,
                &config.general.log_level,
                logging::Mode::WithStderr(stderr),
            );
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(run_daemon(ctx.paths.clone(), config))
        }
        DaemonCommand::Start => start(ctx),
        DaemonCommand::Stop => stop(ctx),
        DaemonCommand::Restart => {
            stop(ctx)?;
            start(ctx)
        }
        DaemonCommand::Status => status(ctx),
        DaemonCommand::Install => {
            let exe = ctx.current_exe()?;
            if is_dev_build(&exe) {
                ui::warn("Registering a development build; prefer `cargo install --path crates/agentping-cli` first.");
            }
            ctx.paths.ensure_dirs()?;
            let msg = service::install(&exe, &ctx.paths.log_dir)?;
            ui::ok(&msg);
            if cfg!(windows) {
                start(ctx)?;
            }
            Ok(())
        }
        DaemonCommand::Uninstall => {
            let msg = service::uninstall()?;
            ui::ok(&msg);
            Ok(())
        }
    }
}

pub fn start(ctx: &Ctx) -> anyhow::Result<()> {
    let config = ctx.load_config()?.config;
    let p = probe(ctx, &config);
    if p.running {
        ui::ok(&format!("Daemon is already running on {}", p.address));
        return Ok(());
    }
    ctx.paths.ensure_dirs()?;
    let exe = ctx.current_exe()?;
    let pid = spawn_detached(&exe, &["daemon", "run"])?;
    if wait_for(ctx, &config, true, Duration::from_secs(8)) {
        ui::ok(&format!("Daemon started on {} (pid {pid})", p.address));
        Ok(())
    } else {
        anyhow::bail!("the daemon did not come up; check `agentping logs` for details")
    }
}

pub fn stop(ctx: &Ctx) -> anyhow::Result<()> {
    let config = ctx.load_config()?.config;
    if !probe(ctx, &config).running {
        ui::info("Daemon is not running");
        return Ok(());
    }
    let client = DaemonClient::new(&ctx.paths, &config, Duration::from_secs(3))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    match rt.block_on(client.shutdown()) {
        Ok(()) | Err(ClientError::Unavailable) => {}
        Err(e) => anyhow::bail!("could not stop the daemon: {e}"),
    }
    if wait_for(ctx, &config, false, Duration::from_secs(10)) {
        ui::ok("Daemon stopped");
        Ok(())
    } else {
        anyhow::bail!("the daemon did not stop within 10 seconds")
    }
}

fn status(ctx: &Ctx) -> anyhow::Result<()> {
    let config = ctx.load_config()?.config;
    let p = probe(ctx, &config);
    let installed = service::is_installed();
    if ctx.json {
        #[derive(Serialize)]
        struct Out<'a> {
            #[serde(flatten)]
            probe: &'a DaemonProbe,
            service_installed: bool,
            pid: Option<u32>,
        }
        return ui::json(&Out {
            probe: &p,
            service_installed: installed,
            pid: read_state(&ctx.paths).filter(|_| p.running).map(|s| s.pid),
        });
    }
    if let Some(h) = &p.health {
        ui::ok(&format!(
            "Daemon running on {} (v{}, up {})",
            p.address,
            h.version,
            humanize_duration(chrono::Duration::seconds(h.uptime_secs))
        ));
        if let Some(s) = read_state(&ctx.paths) {
            ui::kv("PID", &s.pid.to_string());
        }
    } else {
        ui::warn("Daemon is not running");
        ui::hint("Start it with `agentping daemon start`. Without it, hooks deliver notifications with a short-lived background process.");
    }
    ui::kv(
        "Start at login",
        if installed {
            "installed"
        } else {
            "not installed (`agentping daemon install`)"
        },
    );
    Ok(())
}
