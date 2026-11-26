//! Daemon lifecycle and one-shot delivery.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentping_core::secrets::SecretStore;
use agentping_core::{Config, Paths, VERSION};
use agentping_notifiers::{build_providers, http_client};
use agentping_storage::Database;
use chrono::Utc;
use tokio::sync::{watch, Notify};

use crate::delivery::{with_db, Deliverer, SharedDb};
use crate::engine::Engine;
use crate::local::{clear_state, load_or_create_token, write_state, DaemonState};
use crate::server::{bind, router, AppState};

/// Longest a worker sleeps between queue checks.
const MAX_IDLE: Duration = Duration::from_secs(30);

/// Runs the daemon in the foreground until Ctrl-C or an API shutdown request.
pub async fn run_daemon(paths: Paths, config: Config) -> anyhow::Result<()> {
    paths.ensure_dirs()?;
    let db_path = config.database_path(&paths);
    let mut db = Database::open(&db_path)?;
    let engine = Arc::new(Engine::new(config.clone())?);
    let now = Utc::now();
    let backlog = engine.process_backlog(&mut db, now)?;
    let pruned = engine.prune(&mut db, now)?;
    if backlog > 0 || pruned > 0 {
        tracing::info!(backlog, pruned, "startup maintenance complete");
    }
    let db: SharedDb = Arc::new(Mutex::new(db));

    let token = load_or_create_token(&paths)?;
    let secrets = SecretStore::new(paths.clone());
    let providers = Arc::new(build_providers(&config, &secrets, &http_client()?));
    for (name, err) in &providers.errors {
        tracing::warn!(provider = %name, error = %err, "provider is enabled but not usable");
    }

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let remote = start_remote(&db, &config, &secrets, shutdown_rx.clone())?;

    let listener = bind(&config.daemon.host, config.daemon.port).await?;
    let addr = listener.local_addr()?;
    let wake = Arc::new(Notify::new());
    let started_at = Utc::now();
    let state = AppState {
        db: db.clone(),
        engine: engine.clone(),
        token: token.into(),
        wake: wake.clone(),
        shutdown: shutdown_tx.clone(),
        started_at,
        port: addr.port(),
        providers_ready: Arc::new(providers.providers.keys().cloned().collect()),
        providers_failed: Arc::new(
            providers
                .errors
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        remote,
    };
    write_state(
        &paths,
        &DaemonState {
            pid: std::process::id(),
            addr: addr.to_string(),
            started_at,
            version: VERSION.into(),
        },
    )?;
    tracing::info!(%addr, version = VERSION, "AgentPing daemon started");

    let deliverer = Deliverer::new(db.clone(), Arc::new(config.clone()), providers);
    let worker = tokio::spawn(worker_loop(deliverer, wake, shutdown_rx.clone()));
    let maintenance = tokio::spawn(maintenance_loop(db.clone(), engine, shutdown_rx.clone()));

    let signal_tx = shutdown_tx.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("interrupt received; shutting down");
            let _ = signal_tx.send(true);
        }
    });

    let mut server_shutdown = shutdown_rx.clone();
    let served = axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            let _ = server_shutdown.wait_for(|stop| *stop).await;
        })
        .await;
    let _ = shutdown_tx.send(true);
    let _ = worker.await;
    let _ = maintenance.await;
    clear_state(&paths);
    tracing::info!("AgentPing daemon stopped");
    served.map_err(Into::into)
}

async fn worker_loop(deliverer: Deliverer, wake: Arc<Notify>, mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        // A pass runs to completion even if shutdown is requested mid-way, so in-flight sends
        // are recorded rather than abandoned.
        if let Err(e) = deliverer.run_pass().await {
            tracing::error!(error = %e, "delivery pass failed");
        }
        let sleep = match deliverer.next_due_in().await {
            Ok(Some(d)) => d.clamp(Duration::from_millis(100), MAX_IDLE),
            Ok(None) => MAX_IDLE,
            Err(e) => {
                tracing::error!(error = %e, "could not read the queue");
                MAX_IDLE
            }
        };
        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(sleep) => {}
            _ = shutdown.wait_for(|stop| *stop) => break,
        }
    }
}

async fn maintenance_loop(db: SharedDb, engine: Arc<Engine>, mut shutdown: watch::Receiver<bool>) {
    let day = Duration::from_secs(24 * 3600);
    loop {
        tokio::select! {
            _ = tokio::time::sleep(day) => {}
            _ = shutdown.wait_for(|stop| *stop) => break,
        }
        let engine = engine.clone();
        match with_db(&db, move |db| engine.prune(db, Utc::now())).await {
            Ok(n) => tracing::info!(pruned = n, "retention cleanup complete"),
            Err(e) => tracing::error!(error = %e, "retention cleanup failed"),
        }
    }
}

/// Delivers queued notifications until nothing is due within `horizon`, then returns.
///
/// Used when the daemon is not running: a hook spawns `agentping deliver` in the background.
pub async fn deliver_until_idle(
    paths: &Paths,
    config: &Config,
    horizon: Duration,
    max_runtime: Duration,
) -> anyhow::Result<crate::delivery::PassStats> {
    let db_path = config.database_path(paths);
    let mut db = Database::open(&db_path)?;
    let engine = Engine::new(config.clone())?;
    engine.process_backlog(&mut db, Utc::now())?;
    let db: SharedDb = Arc::new(Mutex::new(db));
    let secrets = SecretStore::new(paths.clone());
    let providers = Arc::new(build_providers(config, &secrets, &http_client()?));
    let deliverer = Deliverer::new(db, Arc::new(config.clone()), providers);
    let deadline = tokio::time::Instant::now() + max_runtime;
    let mut total = crate::delivery::PassStats::default();
    loop {
        let pass = deliverer.run_pass().await?;
        total.delivered += pass.delivered;
        total.retried += pass.retried;
        total.failed += pass.failed;
        match deliverer.next_due_in().await? {
            Some(wait) if wait <= horizon && tokio::time::Instant::now() + wait < deadline => {
                tokio::time::sleep(wait.max(Duration::from_millis(100))).await;
            }
            _ => break,
        }
    }
    Ok(total)
}

fn start_remote(
    db: &SharedDb,
    config: &Config,
    secrets: &SecretStore,
    shutdown: watch::Receiver<bool>,
) -> anyhow::Result<Option<Arc<crate::remote::RemoteService>>> {
    if !config.remote.enabled {
        return Ok(None);
    }
    let Some((token, _)) = secrets.get(agentping_core::secrets::SecretKey::DiscordBotToken)? else {
        tracing::error!("remote is enabled but no Discord bot token is stored; run `agentping setup discord-bot`");
        return Ok(None);
    };
    let api = agentping_remote::discord::DiscordApi::new(http_client()?, token);
    let service = crate::remote::start(db.clone(), Arc::new(config.clone()), api, shutdown)?;
    tracing::info!("remote interaction enabled (Discord bot)");
    Ok(Some(service))
}
