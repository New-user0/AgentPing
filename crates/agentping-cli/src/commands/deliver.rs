//! `agentping deliver`: drains the notification queue when the daemon is not running.

use std::time::Duration;

use agentping_daemon::runtime::deliver_until_idle;

use crate::cli::DeliverArgs;
use crate::context::Ctx;
use crate::logging;

pub fn run(ctx: &Ctx, args: &DeliverArgs) -> anyhow::Result<()> {
    let config = ctx.load_config()?.config;
    logging::init(
        &ctx.paths,
        &config.general.log_level,
        logging::Mode::FileOnly,
    );
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let stats = rt.block_on(deliver_until_idle(
        &ctx.paths,
        &config,
        Duration::from_secs(args.horizon_secs),
        Duration::from_secs(15 * 60),
    ));
    match stats {
        Ok(s) => {
            tracing::info!(
                delivered = s.delivered,
                retried = s.retried,
                failed = s.failed,
                "background delivery finished"
            );
            Ok(())
        }
        Err(e) => {
            tracing::error!(error = %e, "background delivery failed");
            Err(e)
        }
    }
}
