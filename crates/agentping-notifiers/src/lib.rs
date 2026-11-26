//! Notification rendering and delivery providers for AgentPing.
//!
//! The event pipeline only depends on [`NotificationProvider`]; concrete providers (Discord,
//! Telegram) live behind it and are constructed from configuration plus the secret store by
//! [`build_providers`].

pub mod discord;
pub mod provider;
pub mod render;
pub mod telegram;

#[cfg(test)]
mod testutil;

use std::collections::BTreeMap;
use std::sync::Arc;

use agentping_core::secrets::{SecretKey, SecretStore};
use agentping_core::Config;

pub use provider::{
    http_client, Delivered, DeliveryError, FailureKind, NotificationProvider, ProviderHealth,
};
pub use render::{render, RenderedNotification};

/// Providers built from configuration, plus any that could not be built.
#[derive(Default)]
pub struct ProviderSet {
    /// Ready providers, keyed by name.
    pub providers: BTreeMap<String, Arc<dyn NotificationProvider>>,
    /// Enabled providers that are misconfigured, with a redacted explanation.
    pub errors: BTreeMap<String, String>,
}

impl ProviderSet {
    /// Looks up a provider by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn NotificationProvider>> {
        self.providers.get(name).cloned()
    }
}

/// Builds a single provider by name from configuration and secrets, ignoring `enabled`.
pub fn build_provider(
    name: &str,
    config: &Config,
    secrets: &SecretStore,
    http: &reqwest::Client,
) -> Result<Arc<dyn NotificationProvider>, String> {
    match name {
        "discord" => {
            let (url, _) = secrets
                .get(SecretKey::DiscordWebhookUrl)
                .map_err(|e| e.to_string())?
                .ok_or("no webhook URL is stored; run `agentping setup discord`")?;
            let p = discord::DiscordProvider::new(
                http.clone(),
                &url,
                &config.providers.discord.username,
            )
            .map_err(|e| format!("stored webhook URL is invalid: {e}"))?;
            Ok(Arc::new(p))
        }
        "telegram" => {
            let (token, _) = secrets
                .get(SecretKey::TelegramBotToken)
                .map_err(|e| e.to_string())?
                .ok_or("no bot token is stored; run `agentping setup telegram`")?;
            let chat = config
                .providers
                .telegram
                .chat_id
                .as_deref()
                .filter(|c| !c.trim().is_empty())
                .ok_or("providers.telegram.chat_id is not set; run `agentping setup telegram`")?;
            let p = telegram::TelegramProvider::new(
                http.clone(),
                token,
                chat,
                config.providers.telegram.silent_low_severity,
            )
            .map_err(|e| format!("stored bot token is invalid: {e}"))?;
            Ok(Arc::new(p))
        }
        "whatsapp" => Err("WhatsApp delivery is not implemented yet".into()),
        other => Err(format!("unknown provider '{other}'")),
    }
}

/// Builds every enabled provider.
pub fn build_providers(
    config: &Config,
    secrets: &SecretStore,
    http: &reqwest::Client,
) -> ProviderSet {
    let mut set = ProviderSet::default();
    for name in config.providers.enabled_names() {
        match build_provider(name, config, secrets, http) {
            Ok(p) => {
                set.providers.insert(name.to_owned(), p);
            }
            Err(e) => {
                set.errors.insert(name.to_owned(), e);
            }
        }
    }
    set
}
