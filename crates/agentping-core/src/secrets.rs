//! Credential storage.
//!
//! Secrets are resolved in this order:
//! 1. Environment variables (`AGENTPING_DISCORD_WEBHOOK_URL`, `AGENTPING_TELEGRAM_BOT_TOKEN`)
//! 2. The OS credential store (Windows Credential Manager, macOS Keychain, Secret Service)
//! 3. A plaintext `secrets.toml` (owner-only permissions), only when the user explicitly opted in
//!
//! Secrets are never written to `config.toml`, the database, or logs.

use std::collections::BTreeMap;
use std::fmt;

use crate::error::CoreError;
use crate::paths::Paths;

/// Service name used for OS credential store entries.
pub const KEYRING_SERVICE: &str = "agentping";

/// Set to `1` to skip the OS credential store entirely (headless systems, tests).
pub const DISABLE_KEYRING_ENV: &str = "AGENTPING_DISABLE_KEYRING";

/// A secret value. Its `Debug` and `Display` output is always redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exposes the secret. Callers must not log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Known secret identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SecretKey {
    /// Discord webhook URL (contains the webhook token).
    DiscordWebhookUrl,
    /// Telegram bot token.
    TelegramBotToken,
}

impl SecretKey {
    /// All known keys.
    pub const ALL: [SecretKey; 2] = [SecretKey::DiscordWebhookUrl, SecretKey::TelegramBotToken];

    /// Identifier used in the keyring and the secrets file.
    pub fn id(self) -> &'static str {
        match self {
            SecretKey::DiscordWebhookUrl => "discord_webhook_url",
            SecretKey::TelegramBotToken => "telegram_bot_token",
        }
    }

    /// Environment variable that overrides the stored value.
    pub fn env_var(self) -> &'static str {
        match self {
            SecretKey::DiscordWebhookUrl => "AGENTPING_DISCORD_WEBHOOK_URL",
            SecretKey::TelegramBotToken => "AGENTPING_TELEGRAM_BOT_TOKEN",
        }
    }

    /// Human readable label.
    pub fn label(self) -> &'static str {
        match self {
            SecretKey::DiscordWebhookUrl => "Discord webhook URL",
            SecretKey::TelegramBotToken => "Telegram bot token",
        }
    }
}

/// Where a secret was found or stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretSource {
    /// An environment variable.
    Environment,
    /// The OS credential store.
    Keyring,
    /// The plaintext secrets file.
    File,
}

impl fmt::Display for SecretSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SecretSource::Environment => "environment variable",
            SecretSource::Keyring => "OS credential store",
            SecretSource::File => "secrets file (plaintext)",
        })
    }
}

/// Resolves and stores secrets.
#[derive(Debug, Clone)]
pub struct SecretStore {
    paths: Paths,
    keyring_enabled: bool,
}

impl SecretStore {
    /// Creates a store for the given paths.
    pub fn new(paths: Paths) -> Self {
        let keyring_enabled = keyring_supported()
            && !std::env::var(DISABLE_KEYRING_ENV).is_ok_and(|v| v == "1" || v == "true");
        Self {
            paths,
            keyring_enabled,
        }
    }

    /// Whether the OS credential store will be used.
    pub fn keyring_enabled(&self) -> bool {
        self.keyring_enabled
    }

    /// Looks up a secret.
    pub fn get(&self, key: SecretKey) -> Result<Option<(Secret, SecretSource)>, CoreError> {
        if let Ok(value) = std::env::var(key.env_var()) {
            if !value.trim().is_empty() {
                return Ok(Some((Secret::new(value.trim()), SecretSource::Environment)));
            }
        }
        if self.keyring_enabled {
            match keyring_get(key) {
                Ok(Some(v)) => return Ok(Some((Secret::new(v), SecretSource::Keyring))),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(key = key.id(), error = %e, "credential store lookup failed")
                }
            }
        }
        if let Some(v) = self.read_file()?.remove(key.id()) {
            return Ok(Some((Secret::new(v), SecretSource::File)));
        }
        Ok(None)
    }

    /// Stores a secret in the OS credential store, or in the plaintext file when
    /// `allow_plaintext` is true and the credential store is unavailable.
    pub fn set(
        &self,
        key: SecretKey,
        value: &Secret,
        allow_plaintext: bool,
    ) -> Result<SecretSource, CoreError> {
        if self.keyring_enabled {
            match keyring_set(key, value.expose()) {
                Ok(()) => {
                    // Never leave a stale plaintext copy behind.
                    self.remove_from_file(key)?;
                    return Ok(SecretSource::Keyring);
                }
                Err(e) if allow_plaintext => {
                    tracing::warn!(error = %e, "credential store unavailable; using plaintext secrets file");
                }
                Err(e) => {
                    return Err(CoreError::Secret(format!(
                        "could not save to the OS credential store ({e}). Re-run with --insecure-store to save it in a user-only plaintext file instead, or set {}",
                        key.env_var()
                    )))
                }
            }
        } else if !allow_plaintext {
            return Err(CoreError::Secret(format!(
                "the OS credential store is disabled or unsupported here. Re-run with --insecure-store to use a user-only plaintext file, or set {}",
                key.env_var()
            )));
        }
        let mut map = self.read_file()?;
        map.insert(key.id().to_owned(), value.expose().to_owned());
        self.write_file(&map)?;
        Ok(SecretSource::File)
    }

    /// Deletes a secret from every persistent location. Returns true if anything was removed.
    pub fn delete(&self, key: SecretKey) -> Result<bool, CoreError> {
        let mut removed = false;
        if self.keyring_enabled {
            match keyring_delete(key) {
                Ok(r) => removed |= r,
                Err(e) => {
                    tracing::warn!(key = key.id(), error = %e, "credential store delete failed")
                }
            }
        }
        removed |= self.remove_from_file(key)?;
        Ok(removed)
    }

    fn remove_from_file(&self, key: SecretKey) -> Result<bool, CoreError> {
        let mut map = self.read_file()?;
        if map.remove(key.id()).is_some() {
            self.write_file(&map)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn read_file(&self) -> Result<BTreeMap<String, String>, CoreError> {
        let path = self.paths.secrets_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| CoreError::ConfigParse {
                path,
                message: e.message().to_owned(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(CoreError::io(&path, e)),
        }
    }

    fn write_file(&self, map: &BTreeMap<String, String>) -> Result<(), CoreError> {
        let path = self.paths.secrets_file();
        if map.is_empty() {
            return match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(CoreError::io(&path, e)),
            };
        }
        let mut text = String::from(
            "# AgentPing plaintext secrets (opted in with --insecure-store).\n# Prefer the OS credential store. This file must stay private to your user.\n",
        );
        text.push_str(
            &toml::to_string(map).map_err(|e| CoreError::Secret(format!("serialize: {e}")))?,
        );
        crate::fsutil::atomic_write(&path, text.as_bytes(), true)
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
fn keyring_supported() -> bool {
    true
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn keyring_supported() -> bool {
    false
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
fn keyring_get(key: SecretKey) -> Result<Option<String>, String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, key.id()).map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
fn keyring_set(key: SecretKey, value: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, key.id()).map_err(|e| e.to_string())?;
    entry.set_password(value).map_err(|e| e.to_string())
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
fn keyring_delete(key: SecretKey) -> Result<bool, String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, key.id()).map_err(|e| e.to_string())?;
    match entry.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn keyring_get(_key: SecretKey) -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn keyring_set(_key: SecretKey, _value: &str) -> Result<(), String> {
    Err("no OS credential store support on this platform".into())
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn keyring_delete(_key: SecretKey) -> Result<bool, String> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_only_store() -> (tempfile::TempDir, SecretStore) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let store = SecretStore {
            paths,
            keyring_enabled: false,
        };
        (dir, store)
    }

    #[test]
    fn secret_debug_and_display_are_redacted() {
        let s = Secret::new("https://discord.com/api/webhooks/1/abc");
        assert_eq!(format!("{s:?}"), "Secret([REDACTED])");
        assert_eq!(format!("{s}"), "[REDACTED]");
    }

    #[test]
    fn plaintext_requires_explicit_opt_in() {
        let (_dir, store) = file_only_store();
        let err = store
            .set(SecretKey::TelegramBotToken, &Secret::new("123:abc"), false)
            .unwrap_err();
        assert!(err.to_string().contains("--insecure-store"));
    }

    #[test]
    fn file_store_roundtrip_and_delete() {
        let (_dir, store) = file_only_store();
        let source = store
            .set(SecretKey::TelegramBotToken, &Secret::new("123:abc"), true)
            .unwrap();
        assert_eq!(source, SecretSource::File);
        assert!(!crate::fsutil::is_world_or_group_accessible(
            &store.paths.secrets_file()
        ));
        // The env override for this key is not set in tests, so the file value wins.
        if std::env::var(SecretKey::TelegramBotToken.env_var()).is_err() {
            let (value, src) = store.get(SecretKey::TelegramBotToken).unwrap().unwrap();
            assert_eq!(value.expose(), "123:abc");
            assert_eq!(src, SecretSource::File);
        }
        assert!(store.delete(SecretKey::TelegramBotToken).unwrap());
        assert!(!store.paths.secrets_file().exists());
    }
}
