//! Shared command context.

use std::path::PathBuf;

use agentping_core::config::{LoadedConfig, DEFAULT_CONFIG_TOML};
use agentping_core::fsutil::atomic_write;
use agentping_core::secrets::SecretStore;
use agentping_core::{Config, Paths};
use agentping_storage::Database;
use anyhow::Context as _;

/// State shared by all commands.
pub struct Ctx {
    /// Filesystem locations.
    pub paths: Paths,
    /// Emit JSON.
    pub json: bool,
}

impl Ctx {
    /// Discovers paths.
    pub fn new(json: bool) -> anyhow::Result<Self> {
        Ok(Self {
            paths: Paths::discover()?,
            json,
        })
    }

    /// Loads and validates the configuration (defaults when the file does not exist).
    pub fn load_config(&self) -> anyhow::Result<LoadedConfig> {
        Config::load(&self.paths).with_context(|| {
            format!(
                "could not load {} (run `agentping config validate` for details)",
                self.paths.config_file().display()
            )
        })
    }

    /// Loads the configuration, falling back to defaults if it is missing or invalid.
    pub fn config_or_default(&self) -> Config {
        self.load_config().map(|l| l.config).unwrap_or_default()
    }

    /// Credential store.
    pub fn secrets(&self) -> SecretStore {
        SecretStore::new(self.paths.clone())
    }

    /// Opens the database for `config`.
    pub fn open_db(&self, config: &Config) -> anyhow::Result<Database> {
        let path = config.database_path(&self.paths);
        Database::open(&path).with_context(|| format!("could not open database {}", path.display()))
    }

    /// Ensures directories and a commented config file exist. Returns true if the file was created.
    pub fn ensure_config_file(&self) -> anyhow::Result<bool> {
        self.paths.ensure_dirs()?;
        let path = self.paths.config_file();
        if path.exists() {
            return Ok(false);
        }
        atomic_write(&path, DEFAULT_CONFIG_TOML.as_bytes(), true)?;
        Ok(true)
    }

    /// Applies `key = value` edits to the config file (creating it if needed), validating the result.
    pub fn set_config_values(&self, edits: &[(&str, &str)]) -> anyhow::Result<()> {
        self.ensure_config_file()?;
        let path = self.paths.config_file();
        let mut text = std::fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        for (key, value) in edits {
            text = agentping_core::config::set_value(&text, key, value)?;
        }
        atomic_write(&path, text.as_bytes(), true)?;
        Ok(())
    }

    /// Path of the running executable (used for hook commands and services).
    pub fn current_exe(&self) -> anyhow::Result<PathBuf> {
        std::env::current_exe().context("could not determine the agentping executable path")
    }
}

/// Whether the executable lives in a Cargo `target/` directory (a development build).
pub fn is_dev_build(exe: &std::path::Path) -> bool {
    exe.components().any(|c| c.as_os_str() == "target")
        && exe
            .components()
            .any(|c| c.as_os_str() == "debug" || c.as_os_str() == "release")
}
