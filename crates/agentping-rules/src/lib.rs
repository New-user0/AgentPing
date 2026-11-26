//! Deterministic, configuration-driven notification rules.
//!
//! Evaluation order (documented in docs/configuration.md):
//!
//! 1. Status-only events (`activity`) never notify.
//! 2. The global switch and per-agent switches.
//! 3. The first matching `[[rules]]` entry decides (notify or mute, providers, severity).
//!    When no rule matches, the per-event-type toggles and project filters decide.
//! 4. Delivery goes to enabled providers (optionally restricted by the rule).
//! 5. Quiet hours suppress events below the configured severity.
//! 6. Repeat suppression mutes repeated low-value events in the same session.

use std::time::Duration;

use agentping_core::config::{parse_hhmm, Config, RuleConfig};
use agentping_core::{Event, EventType, Severity};
use chrono::NaiveTime;
use globset::{Glob, GlobMatcher};
use serde::Serialize;

/// Errors building the engine from configuration.
#[derive(Debug, thiserror::Error)]
pub enum RulesError {
    /// A glob pattern is invalid.
    #[error("rule '{rule}': invalid glob '{pattern}': {message}")]
    BadGlob {
        /// Rule name.
        rule: String,
        /// Pattern.
        pattern: String,
        /// Parser message.
        message: String,
    },
}

/// Facts about the environment that the engine needs but cannot compute itself.
#[derive(Debug, Clone, Copy)]
pub struct RuleContext {
    /// Current local wall-clock time (for quiet hours).
    pub now_local: NaiveTime,
    /// Whether a notification of the same type was sent for this session within the repeat window.
    pub recently_notified: bool,
}

/// The outcome of evaluating an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    /// Whether to notify.
    pub notify: bool,
    /// Providers to deliver through (empty when not notifying).
    pub providers: Vec<String>,
    /// Effective severity.
    pub severity: Severity,
    /// Human readable explanation.
    pub reason: String,
}

impl Decision {
    fn mute(severity: Severity, reason: impl Into<String>) -> Self {
        Self {
            notify: false,
            providers: Vec::new(),
            severity,
            reason: reason.into(),
        }
    }

    /// Compact summary stored with the event.
    pub fn summary(&self) -> String {
        if self.notify {
            format!("notify via {} ({})", self.providers.join(", "), self.reason)
        } else {
            format!("muted: {}", self.reason)
        }
    }
}

struct CompiledRule {
    config: RuleConfig,
    path_globs: Vec<GlobMatcher>,
}

impl CompiledRule {
    fn matches(&self, event: &Event) -> bool {
        let r = &self.config;
        if !r.agents.is_empty() && !r.agents.iter().any(|a| a == event.agent.as_str()) {
            return false;
        }
        if !r.event_types.is_empty()
            && !r.event_types.iter().any(|t| t == event.event_type.as_str())
        {
            return false;
        }
        if !r.projects.is_empty() {
            let Some(project) = event.project_name.as_deref() else {
                return false;
            };
            if !r.projects.iter().any(|p| p.eq_ignore_ascii_case(project)) {
                return false;
            }
        }
        if !self.path_globs.is_empty() {
            let Some(path) = event.project_path.as_deref().or(event.cwd.as_deref()) else {
                return false;
            };
            let normalized = path.replace('\\', "/");
            if !self.path_globs.iter().any(|g| g.is_match(&normalized)) {
                return false;
            }
        }
        if let Some(min) = r.min_severity {
            if event.severity < min {
                return false;
            }
        }
        true
    }

    fn label(&self, index: usize) -> String {
        if self.config.name.is_empty() {
            format!("rule #{}", index + 1)
        } else {
            format!("rule '{}'", self.config.name)
        }
    }
}

/// A compiled rules engine for one configuration snapshot.
pub struct RulesEngine {
    config: Config,
    rules: Vec<CompiledRule>,
    quiet: Option<(NaiveTime, NaiveTime, Severity)>,
}

impl RulesEngine {
    /// Compiles the rules in `config`.
    pub fn new(config: &Config) -> Result<Self, RulesError> {
        let mut rules = Vec::with_capacity(config.rules.len());
        for rule in &config.rules {
            let mut path_globs = Vec::new();
            for pattern in &rule.project_paths {
                let glob =
                    Glob::new(&pattern.replace('\\', "/")).map_err(|e| RulesError::BadGlob {
                        rule: rule.name.clone(),
                        pattern: pattern.clone(),
                        message: e.to_string(),
                    })?;
                path_globs.push(glob.compile_matcher());
            }
            rules.push(CompiledRule {
                config: rule.clone(),
                path_globs,
            });
        }
        let quiet = if config.quiet_hours.enabled {
            match (
                parse_hhmm(&config.quiet_hours.start),
                parse_hhmm(&config.quiet_hours.end),
            ) {
                (Some(s), Some(e)) => Some((s, e, config.quiet_hours.min_severity)),
                _ => None,
            }
        } else {
            None
        };
        Ok(Self {
            config: config.clone(),
            rules,
            quiet,
        })
    }

    /// Repeat-suppression window for this event type, if any.
    ///
    /// Actionable events (permission requests, questions, failures) are never suppressed.
    pub fn repeat_window(&self, event_type: &EventType) -> Option<Duration> {
        let secs = self.config.notifications.repeat_window_secs;
        let suppressible = matches!(
            event_type,
            EventType::AgentIdle
                | EventType::TaskCompleted
                | EventType::Notification
                | EventType::AgentStarted
                | EventType::AgentStopped
        );
        (suppressible && secs > 0).then(|| Duration::from_secs(secs))
    }

    /// Evaluates `event`.
    pub fn evaluate(&self, event: &Event, ctx: &RuleContext) -> Decision {
        let mut severity = event.severity;
        if event.event_type == EventType::Activity {
            return Decision::mute(severity, "status-only event");
        }
        if !self.config.general.notifications_enabled {
            return Decision::mute(
                severity,
                "notifications are disabled (general.notifications_enabled)",
            );
        }
        if event.agent == agentping_core::AgentKind::Claude && !self.config.agents.claude.enabled {
            return Decision::mute(
                severity,
                "the Claude Code integration is disabled (agents.claude.enabled)",
            );
        }

        let mut restrict: Option<&[String]> = None;
        let matched = self
            .rules
            .iter()
            .enumerate()
            .find(|(_, rule)| rule.matches(event));
        let reason = match matched {
            Some((index, rule)) => {
                let label = rule.label(index);
                if !rule.config.notify {
                    return Decision::mute(severity, format!("{label} mutes this event"));
                }
                if let Some(s) = rule.config.severity {
                    severity = s;
                }
                if !rule.config.providers.is_empty() {
                    restrict = Some(&rule.config.providers);
                }
                label
            }
            None => {
                let n = &self.config.notifications;
                if !n.enabled_for(&event.event_type) {
                    return Decision::mute(
                        severity,
                        format!(
                            "{} notifications are turned off in [notifications]",
                            event.event_type
                        ),
                    );
                }
                if let Some(project) = event.project_name.as_deref() {
                    if n.filters
                        .exclude_projects
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(project))
                    {
                        return Decision::mute(
                            severity,
                            format!("project '{project}' is excluded"),
                        );
                    }
                }
                if !n.filters.include_projects.is_empty() {
                    let included = event.project_name.as_deref().is_some_and(|project| {
                        n.filters
                            .include_projects
                            .iter()
                            .any(|p| p.eq_ignore_ascii_case(project))
                    });
                    if !included {
                        return Decision::mute(severity, "project is not in include_projects");
                    }
                }
                "default preferences".to_owned()
            }
        };

        let enabled = self.config.providers.enabled_names();
        let providers: Vec<String> = enabled
            .iter()
            .filter(|p| restrict.is_none_or(|r| r.iter().any(|x| x == *p)))
            .map(|p| (*p).to_owned())
            .collect();
        if providers.is_empty() {
            return Decision::mute(
                severity,
                if enabled.is_empty() {
                    "no notification providers are enabled".to_owned()
                } else {
                    format!("{reason} targets providers that are not enabled")
                },
            );
        }

        if let Some((start, end, min)) = self.quiet {
            if in_window(ctx.now_local, start, end) && severity < min {
                return Decision::mute(
                    severity,
                    format!("quiet hours ({severity} is below {min})"),
                );
            }
        }

        if ctx.recently_notified && self.repeat_window(&event.event_type).is_some() {
            return Decision::mute(
                severity,
                format!(
                    "repeat within {}s for this session",
                    self.config.notifications.repeat_window_secs
                ),
            );
        }

        Decision {
            notify: true,
            providers,
            severity,
            reason,
        }
    }
}

/// Whether `now` falls in `[start, end)`, wrapping past midnight when `end <= start`.
pub fn in_window(now: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    match start.cmp(&end) {
        std::cmp::Ordering::Less => now >= start && now < end,
        std::cmp::Ordering::Greater => now >= start || now < end,
        std::cmp::Ordering::Equal => false,
    }
}

#[cfg(test)]
mod tests;
