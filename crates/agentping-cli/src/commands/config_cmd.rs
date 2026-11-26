//! `agentping config …`

use agentping_core::config::Config;
use agentping_core::paths::display_path;
use agentping_core::CoreError;

use crate::cli::{ConfigAction, ConfigArgs};
use crate::context::Ctx;
use crate::ui;

pub fn run(ctx: &Ctx, args: &ConfigArgs) -> anyhow::Result<()> {
    match &args.action {
        Some(ConfigAction::Path) => {
            ui::line(&ctx.paths.config_file().display().to_string());
            Ok(())
        }
        Some(ConfigAction::Show) => {
            let config = ctx.load_config()?.config;
            if ctx.json {
                ui::json(&config)
            } else {
                ui::line(&config.to_toml_string()?);
                Ok(())
            }
        }
        Some(ConfigAction::Validate) => validate(ctx),
        Some(ConfigAction::Set { key, value }) => {
            ctx.set_config_values(&[(key.as_str(), value.as_str())])?;
            ui::ok(&format!("Set {key} = {value}"));
            ui::hint("Restart the daemon to apply: agentping daemon restart");
            Ok(())
        }
        None => summary(ctx),
    }
}

fn validate(ctx: &Ctx) -> anyhow::Result<()> {
    let path = ctx.paths.config_file();
    if !path.exists() {
        ui::warn(&format!(
            "{} does not exist; built-in defaults are used",
            display_path(&path)
        ));
        ui::hint("Create it with `agentping init`.");
        return Ok(());
    }
    let text = std::fs::read_to_string(&path)?;
    let mut config = match Config::from_toml_str(&text, &path) {
        Ok(c) => c,
        Err(e) => {
            ui::fail(&e.to_string());
            anyhow::bail!("configuration is invalid");
        }
    };
    config.apply_env_overrides(|k| std::env::var(k).ok());
    let issues = config.validate();
    if ctx.json {
        ui::json(
            &serde_json::json!({ "path": path, "valid": issues.is_empty(), "issues": issues }),
        )?;
    } else if issues.is_empty() {
        ui::ok(&format!("{} is valid", display_path(&path)));
    } else {
        for i in &issues {
            ui::fail(&i.to_string());
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(CoreError::ConfigInvalid(issues).into())
    }
}

fn summary(ctx: &Ctx) -> anyhow::Result<()> {
    let path = ctx.paths.config_file();
    ui::heading("Configuration");
    ui::kv("File", &display_path(&path));
    ui::kv("Data", &display_path(&ctx.paths.data_dir));
    ui::kv("Logs", &display_path(&ctx.paths.log_dir));
    if !path.exists() {
        ui::warn("No config file yet (defaults in use). Run `agentping init`.");
        return Ok(());
    }
    match ctx.load_config() {
        Ok(l) => {
            ui::ok("Valid");
            ui::kv(
                "Database",
                &display_path(&l.config.database_path(&ctx.paths)),
            );
        }
        Err(e) => ui::fail(&format!("{e:#}")),
    }
    ui::blank();
    ui::line(&ui::dim(
        "Subcommands: path, show, validate, set <key> <value>",
    ));
    Ok(())
}
