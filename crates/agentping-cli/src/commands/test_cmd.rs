//! `agentping test [provider]`

use agentping_notifiers::{build_provider, http_client, render};
use serde::Serialize;

use crate::cli::TestArgs;
use crate::commands::setup::title_case;
use crate::context::Ctx;
use crate::ui;

#[derive(Serialize)]
struct TestResult {
    provider: String,
    ok: bool,
    detail: String,
}

pub fn run(ctx: &Ctx, args: &TestArgs) -> anyhow::Result<()> {
    let config = ctx.load_config()?.config;
    let targets: Vec<String> = match args.provider {
        Some(p) => vec![p.as_str().to_owned()],
        None => config
            .providers
            .enabled_names()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    };
    if targets.is_empty() {
        anyhow::bail!(
            "no providers are enabled; run `agentping setup discord` or `agentping setup telegram`"
        );
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let http = http_client()?;
    let secrets = ctx.secrets();
    let event = render::test_event();
    let notification = render(&event, event.severity, &config);
    let mut results = Vec::new();
    for name in &targets {
        let result = match build_provider(name, &config, &secrets, &http) {
            Err(e) => TestResult {
                provider: name.clone(),
                ok: false,
                detail: e,
            },
            Ok(p) => match rt.block_on(p.send(&notification)) {
                Ok(d) => TestResult {
                    provider: name.clone(),
                    ok: true,
                    detail: format!(
                        "delivered{}",
                        d.http_status
                            .map(|s| format!(" (HTTP {s})"))
                            .unwrap_or_default()
                    ),
                },
                Err(e) => TestResult {
                    provider: name.clone(),
                    ok: false,
                    detail: e.message,
                },
            },
        };
        results.push(result);
    }
    if ctx.json {
        ui::json(&results)?;
    } else {
        for r in &results {
            if r.ok {
                ui::ok(&format!(
                    "{}: test notification {}",
                    title_case(&r.provider),
                    r.detail
                ));
            } else {
                ui::fail(&format!("{}: {}", title_case(&r.provider), r.detail));
            }
        }
        if results.iter().all(|r| r.ok) {
            ui::hint("Check your phone: the message should arrive within a few seconds.");
        }
    }
    if results.iter().any(|r| !r.ok) {
        anyhow::bail!("one or more providers failed (`agentping doctor` can help)");
    }
    Ok(())
}
